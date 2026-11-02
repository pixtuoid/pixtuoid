//! What a fuzz target (`fuzz/fuzz_targets/`) drives past [`Drive`] alone, and
//! the mutator they share. Their one contract: a refusal is fine, a panic is not.

use std::path::Path;
use std::str::FromStr;
use std::time::{Duration, UNIX_EPOCH};

use mutatis::{Candidates, Context, Mutate, Session, mutators as m};
use serde_json::{Number, Value};

use super::{DEFAULT_NOW_EPOCH_SECS, DRIVEN_DESKS, Drive, Driven};
use crate::remote::{self, Answer, Ask, Hello, PairLink, RemoteOffice, Reply};
use crate::source::admit::{is_transcript, lines, whole};
use crate::source::registry;
#[cfg(feature = "native")]
use crate::source::registry::Transcript;
use crate::state::MAX_FLOORS;

/// A hook connection's bytes, framed as the listener frames them: a last line
/// needs no `\n`, unlike a transcript's, and one that isn't UTF-8 ends the
/// connection.
pub fn hook(bytes: &[u8]) -> Driven {
    let lines = (bytes.split(|b| *b == b'\n')).map_while(|line| std::str::from_utf8(line).ok());
    Drive::hooks().lines(lines)
}

/// What [`hook`] never reaches of the endpoint's wire.
///
/// # Panics
///
/// If the demux changes a payload, which is what the target hunts.
#[cfg(feature = "native")]
pub fn endpoint(bytes: &[u8]) {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("a runtime")
        .block_on(crate::source::hook::drive_wire(bytes));
}

/// `<source>\n<path>\n<transcript>`: everything `source`'s registry row reads of
/// a transcript, then [`Drive`]. A source by name, not by index, so a new row
/// leaves every accumulated input meaning what it did, and a path because a
/// nested transcript's parent is its path's. `None` when a header is missing, the
/// first names no transcript source, or the watcher wouldn't read the path.
pub fn transcript(bytes: &[u8]) -> Option<Driven> {
    let mut parts = bytes.splitn(3, |b| *b == b'\n');
    let source = std::str::from_utf8(parts.next()?).ok()?;
    let path = std::str::from_utf8(parts.next()?).ok()?;
    let body = parts.next()?;
    let row = registry::transcript_for(source)?;
    if !is_transcript(Path::new(path), &row.path_filter) {
        return None;
    }
    let drive = Drive::transcript_at(source, Path::new(path))?.seeded();
    let parsed: Vec<Value> = lines(whole(body))
        .filter_map(Result::ok)
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    for line in &parsed {
        let _ = (row.cwd_extractor)(line);
        let _ = row.parent_extractor.map(|extract| extract(line));
    }
    #[cfg(feature = "native")]
    native_reads(row, source, Path::new(path), body, &parsed);
    Some(drive.body(body))
}

#[cfg(feature = "native")]
fn native_reads(row: &Transcript, source: &str, path: &Path, body: &[u8], parsed: &[Value]) {
    let native = row.native;
    let _ = (native.check_ended)(body);
    let _ = native.activity_recency.map(|recency| recency(body));
    let head_cwd = parsed.iter().find_map(row.cwd_extractor);
    let cwd = head_cwd.or_else(|| native.cwd_derive.and_then(|derive| derive(path)));
    let _ = native.derive_label.map(|label| {
        label(path, source, cwd.as_deref().unwrap_or(Path::new("")));
    });
    if let Some(label) = native.head_label {
        parsed.iter().for_each(|line| drop(label(line)));
    }
}

/// Each parser a peer's bytes or a scanned link reach: the whole input to the
/// link, hello, reply and ask parsers, then split as one stream of answers and
/// office lines, whose office is re-encoded and re-seated.
pub fn remote(bytes: &[u8]) -> RemoteOffice {
    if let Ok(text) = std::str::from_utf8(bytes) {
        let _ = PairLink::from_str(text);
        let _ = serde_json::from_str::<Hello>(text);
        let _ = serde_json::from_str::<Reply>(text);
        let _ = serde_json::from_str::<Ask>(text);
    }
    let now = UNIX_EPOCH + Duration::from_secs(DEFAULT_NOW_EPOCH_SECS);
    let mut office = RemoteOffice::new([DRIVEN_DESKS; MAX_FLOORS]);
    for line in bytes.split(|b| *b == b'\n') {
        if Answer::from_line(line).is_none() {
            let _ = office.apply(line, now);
        }
    }
    let _ = remote::encode(office.scene(), now);
    office.set_capacities([1; MAX_FLOORS]);
    office
}

/// `fuzz_mutator!`'s body: mutates one node or the order of `data`'s JSON lines,
/// every other line, a transcript's source among them, staying put. A byte
/// mutation of JSON text is nearly always a parse error
/// (<https://github.com/google/fuzzing/blob/master/docs/structure-aware-fuzzing.md>).
///
/// `None` leaves this mutation to `fuzzer_mutate`: on even seeds, for what only
/// bytes reach, a line's syntax; with no JSON line; and when the mutant outgrows
/// `max_size`.
pub fn json_mutate(data: &mut [u8], size: usize, max_size: usize, seed: u32) -> Option<usize> {
    if seed.is_multiple_of(2) {
        return None;
    }
    let mut lines: Vec<_> = (data[..size].split(|b| *b == b'\n'))
        .filter(|line| !line.is_empty())
        .map(Line::parse)
        .collect();
    if !lines.iter().any(|line| matches!(line, Line::Json(_))) {
        return None;
    }
    Session::new()
        .seed(seed.into())
        .mutate_with(&mut LinesMutator, &mut lines)
        .ok()?;
    let mutant = lines.iter().fold(Vec::new(), |mut bytes, line| {
        line.write(&mut bytes);
        bytes
    });
    let out = data
        .get_mut(..mutant.len())
        .filter(|_| mutant.len() <= max_size)?;
    out.copy_from_slice(&mutant);
    Some(mutant.len())
}

#[derive(Clone, Debug)]
enum Line {
    Json(Value),
    Opaque(Vec<u8>),
}

impl Line {
    fn parse(line: &[u8]) -> Self {
        serde_json::from_slice(line).map_or_else(|_| Self::Opaque(line.to_vec()), Self::Json)
    }

    fn write(&self, bytes: &mut Vec<u8>) {
        match self {
            Self::Json(v) => bytes.extend(v.to_string().into_bytes()),
            Self::Opaque(line) => bytes.extend(line),
        }
        bytes.push(b'\n');
    }
}

/// Any node of any JSON line, or those lines' own order: a fold's state turns on
/// which event came first.
struct LinesMutator;

impl Mutate<Vec<Line>> for LinesMutator {
    fn mutate(&mut self, c: &mut Candidates, lines: &mut Vec<Line>) -> mutatis::Result<()> {
        for line in lines.iter_mut() {
            if let Line::Json(v) = line {
                JsonMutator.mutate(c, v)?;
            }
        }
        let json: Vec<_> = (lines.iter().enumerate())
            .filter(|(_, line)| matches!(line, Line::Json(_)))
            .map(|(index, _)| index)
            .collect();
        let pick = |ctx: &mut Context| json[ctx.rng().gen_index(json.len()).unwrap_or_default()];
        if json.len() > 1 {
            c.mutation(|ctx| {
                lines.remove(pick(ctx));
                Ok(())
            })?;
            c.mutation(|ctx| {
                lines.swap(pick(ctx), pick(ctx));
                Ok(())
            })?;
        }
        c.mutation(|ctx| {
            let index = pick(ctx);
            lines.insert(index, lines[index].clone());
            Ok(())
        })
    }
}

/// Every node may lose its fields, take a neighboring value, or change kind.
struct JsonMutator;

impl Mutate<Value> for JsonMutator {
    fn mutate(&mut self, c: &mut Candidates, value: &mut Value) -> mutatis::Result<()> {
        match value {
            Value::Bool(flag) => m::default::<bool>().mutate(c, flag)?,
            Value::String(text) => m::default::<String>().mutate(c, text)?,
            Value::Number(number) => {
                let near = number.as_i64().unwrap_or_default();
                let neighbors = [
                    Number::from(near.wrapping_add(1)),
                    Number::from(near.wrapping_sub(1)),
                    Number::from(u64::MAX),
                    Number::from(i64::MIN),
                ];
                for neighbor in neighbors {
                    c.mutation(|_| {
                        *number = neighbor.clone();
                        Ok(())
                    })?;
                }
                c.mutation(|ctx| {
                    *number = ctx.rng().gen_i64().into();
                    Ok(())
                })?;
            }
            Value::Array(items) => {
                for item in items.iter_mut() {
                    self.mutate(c, item)?;
                }
                if !items.is_empty() {
                    c.mutation(|ctx| {
                        let index = ctx.rng().gen_index(items.len()).unwrap_or_default();
                        items.remove(index);
                        Ok(())
                    })?;
                }
            }
            Value::Object(fields) => {
                for item in fields.values_mut() {
                    self.mutate(c, item)?;
                }
                if !fields.is_empty() {
                    c.mutation(|ctx| {
                        let index = ctx.rng().gen_index(fields.len()).unwrap_or_default();
                        if let Some(key) = fields.keys().nth(index).cloned() {
                            fields.remove(&key);
                        }
                        Ok(())
                    })?;
                }
            }
            Value::Null => {}
        }
        c.mutation(|ctx| {
            *value = match ctx.rng().gen_index(6) {
                Some(0) => Value::Null,
                Some(1) => false.into(),
                Some(2) => 0.into(),
                Some(3) => "".into(),
                Some(4) => Value::Array(Vec::new()),
                _ => Value::Object(serde_json::Map::new()),
            };
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINE: &str = r#"{"type":"assistant","sessionId":"s","cwd":"/repo","message":{"role":"assistant","content":[{"type":"tool_use","id":"t","name":"Write","input":{}}]}}"#;

    #[test]
    fn a_hook_connection_is_framed_as_the_listener_frames_it() {
        let payload = br#"{"session_id":"s","cwd":"/repo","hook_event_name":"SessionStart"}"#;
        assert_eq!(
            hook(payload).registered(),
            1,
            "a last line needs no newline"
        );
        let past_a_bad_line = [b"\xff\n".as_slice(), payload].concat();
        assert_eq!(hook(&past_a_bad_line).registered(), 0);
    }

    #[cfg(feature = "native")]
    #[test]
    fn the_hook_seeds_are_the_status_line_and_its_reply() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/seeds/hook");
        for seed in ["status", "reply"] {
            let bytes = std::fs::read(dir.join(seed)).unwrap();
            endpoint(&bytes);
            assert_eq!(hook(&bytes).registered(), 0, "{seed}");
        }
        let reply = std::fs::read(dir.join("reply")).unwrap();
        serde_json::from_slice::<crate::source::hook::OwnerStatus>(&reply).unwrap();
    }

    #[test]
    fn a_transcript_folds_under_the_source_its_first_line_names() {
        let input = format!("claude-code\n/p/session.jsonl\n{LINE}\n");
        let driven = transcript(input.as_bytes()).expect("a transcript source");
        assert_eq!(driven.registered(), 1);
        assert!(driven.wire_events() > 0, "{driven:?}");
    }

    #[test]
    fn the_path_line_keys_a_transcript_as_the_watcher_would() {
        let drive = |path: &str| {
            let input = format!("omp\n{path}\n{{}}\n");
            let driven = transcript(input.as_bytes()).expect("a transcript source");
            driven.scene.agents.keys().copied().collect::<Vec<_>>()
        };
        let session = "2026-08-10T10-11-12-000Z_0197fa30";
        assert_ne!(
            drive(&format!("/s/{session}/task.jsonl")),
            drive("/s/plain/task.jsonl"),
            "only a nesting the whole path shows tells these apart"
        );
    }

    #[test]
    fn a_path_the_watcher_would_not_read_drives_nothing() {
        assert!(transcript(b"grok\n/s/updates.jsonl\n{}\n").is_some());
        assert!(transcript(b"grok\n/s/chat_history.jsonl\n{}\n").is_none());
        assert!(transcript(b"omp\n/s/session.jsonl\n{}\n").is_some());
        assert!(transcript(b"omp\n/s/session.txt\n{}\n").is_none());
    }

    #[test]
    fn an_input_missing_a_header_or_a_transcript_source_drives_nothing() {
        let inputs = [
            "not-a-source\n/p\n{}\n",
            "openclaw\n/p\n{}\n",
            "claude-code\n{}",
            "no newline",
        ];
        for input in inputs {
            assert!(transcript(input.as_bytes()).is_none(), "{input}");
        }
    }

    #[test]
    fn a_stream_seats_its_agents_past_a_foreign_line() {
        let golden: Value =
            serde_json::from_str(include_str!("../remote.v1.golden.json")).expect("the golden");
        let office = remote(format!("garbage\n{golden}").as_bytes());
        assert!(!office.scene().agents.is_empty());
        assert!(remote(b"garbage").scene().agents.is_empty());
    }

    /// A line that reads as an office line too seats no one: only the answer
    /// guard keeps it out, since `apply` would take it.
    #[test]
    fn an_answer_among_the_lines_is_not_taken_for_the_office() {
        let golden: Value =
            serde_json::from_str(include_str!("../remote.v1.golden.json")).expect("the golden");
        let answer = serde_json::to_string(&Answer::Unsupported).expect("it encodes");
        let office = remote(format!("{golden}\n{answer}").as_bytes());
        assert!(!office.scene().agents.is_empty());

        let mut both = golden;
        both["answer"] = "unsupported".into();
        assert!(
            remote(both.to_string().as_bytes())
                .scene()
                .agents
                .is_empty()
        );
    }

    #[test]
    fn each_committed_remote_seed_gets_past_its_parser() {
        let seed = |name: &str| {
            let seeds = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/seeds/remote");
            std::fs::read_to_string(seeds.join(name)).expect(name)
        };
        PairLink::from_str(&seed("link")).expect("a link");
        serde_json::from_str::<Hello>(&seed("hello")).expect("a hello");
        let reply = serde_json::from_str(&seed("reply")).expect("a reply");
        assert!(matches!(reply, Reply::Paired { .. }));
        let ask = serde_json::from_str::<Ask>(&seed("ask")).expect("an ask");
        assert_eq!(ask, Ask::Sources);
        let answer = Answer::from_line(seed("answer").as_bytes()).expect("an answer");
        assert!(matches!(answer, Answer::Sources { rows } if !rows.is_empty()));
        let change = serde_json::from_str::<Ask>(&seed("change")).expect("a change");
        assert!(matches!(change, Ask::Change { .. }));
        let changed = Answer::from_line(seed("changed").as_bytes()).expect("an answer");
        assert!(matches!(changed, Answer::Changed(_)));
    }

    const MUTABLE: &str =
        "claude-code\n{\"text\":\"ascii\",\"n\":1,\"list\":[true,null]}\n{\"other\":{}}\n";
    const ROOM: usize = 4096;

    fn mutants() -> impl Iterator<Item = String> {
        (0..4096).filter_map(|seed| {
            let mut data = vec![0; ROOM];
            data[..MUTABLE.len()].copy_from_slice(MUTABLE.as_bytes());
            let len = json_mutate(&mut data, MUTABLE.len(), ROOM, seed * 2 + 1)?;
            data.truncate(len);
            Some(String::from_utf8(data).expect("serde_json writes UTF-8"))
        })
    }

    #[test]
    fn a_mutant_stays_json_lines_under_its_opaque_line() {
        let mut count = 0;
        for mutant in mutants() {
            count += 1;
            assert!(mutant.ends_with('\n'), "{mutant}");
            let json = mutant
                .strip_prefix("claude-code\n")
                .expect("its opaque line");
            for line in json.lines() {
                serde_json::from_str::<Value>(line).expect(line);
            }
        }
        assert_eq!(count, 4096);
    }

    #[test]
    fn the_mutants_reach_every_kind_of_change() {
        let all: Vec<_> = mutants().map(|m| m.replace("claude-code\n", "")).collect();
        let reached = |what: &str, found: fn(&str) -> bool| {
            assert!(all.iter().any(|m| found(m)), "no mutant {what}");
        };
        reached("carries a multi-byte character", |m| !m.is_ascii());
        reached("shortens a string", |m| m.contains("\"asci\""));
        reached("changes a value's kind", |m| m.contains("\"n\":[]"));
        reached("takes a boundary number", |m| {
            m.contains(&u64::MAX.to_string())
        });
        reached("drops a field", |m| {
            m.lines().count() == 2 && m.contains("\"n\":") && !m.contains("\"text\"")
        });
        reached("drops an array item", |m| {
            m.contains("\"list\":[true]") || m.contains("\"list\":[null]")
        });
        reached("drops a line", |m| m.lines().count() == 1);
        reached("repeats a line", |m| m.lines().count() == 3);
        reached("reorders its lines", |m| {
            m.lines().count() == 2 && m.starts_with("{\"other\"") && m.contains("\"text\"")
        });
    }

    #[test]
    fn what_no_json_tree_can_mutate_is_left_to_libfuzzer() {
        let input = |text: &str| {
            let mut data = vec![0; ROOM];
            data[..text.len()].copy_from_slice(text.as_bytes());
            data
        };
        let leaves = |text: &str, max_size, seed| {
            json_mutate(&mut input(text), text.len(), max_size, seed).is_none()
        };
        assert!(leaves(MUTABLE, ROOM, 2), "an even seed");
        assert!(leaves("no json here", ROOM, 1));
        assert!(leaves(MUTABLE, 1, 1), "no mutant fits");
    }
}
