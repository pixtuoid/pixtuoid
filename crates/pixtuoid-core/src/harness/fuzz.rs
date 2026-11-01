//! What a fuzz target (`fuzz/fuzz_targets/`) drives past [`Drive`] alone. Their
//! one contract: a refusal is fine, a panic is not.

use std::str::FromStr;
use std::time::{Duration, UNIX_EPOCH};

use mutatis::{Candidates, Mutate, Session, mutators as m};
use serde_json::Value;

use super::{DEFAULT_NOW_EPOCH_SECS, DRIVEN_DESKS, Drive, Driven};
use crate::remote::{self, Hello, PairLink, RemoteOffice, Reply};
use crate::source::admit::{lines, whole};
use crate::source::claude_code::cc_activity_recency;
use crate::source::registry;
use crate::state::MAX_FLOORS;

/// `<source>\n<transcript>`: its source's first-sight reads and tail scan, then
/// [`Drive`]. A source by name, not by index, so a new row leaves every
/// accumulated input meaning what it did. `None` when the first line names no
/// transcript source.
pub fn transcript(bytes: &[u8]) -> Option<Driven> {
    let end = bytes.iter().position(|b| *b == b'\n')?;
    let (source, body) = bytes.split_at(end);
    let source = std::str::from_utf8(source).ok()?;
    let drive = Drive::transcript(source, "/fuzz/session.jsonl")?.seeded();
    let cwd = registry::cwd_extractor_for(source);
    let parent = registry::parent_extractor_for(source);
    let parsed = lines(whole(body))
        .filter_map(Result::ok)
        .filter_map(|line| serde_json::from_str(line).ok());
    for line in parsed {
        let _ = cwd(&line);
        let _ = parent.map(|extract| extract(&line));
    }
    let _ = cc_activity_recency(body);
    Some(drive.body(body))
}

/// All a network peer can send, in any order: a pairing link, a hello, a reply,
/// then one stream's lines, whose office is then re-encoded and re-seated.
pub fn remote(bytes: &[u8]) -> RemoteOffice {
    if let Ok(text) = std::str::from_utf8(bytes) {
        let _ = PairLink::from_str(text);
        let _ = serde_json::from_str::<Hello>(text);
        let _ = serde_json::from_str::<Reply>(text);
    }
    let now = UNIX_EPOCH + Duration::from_secs(DEFAULT_NOW_EPOCH_SECS);
    let mut office = RemoteOffice::new([DRIVEN_DESKS; MAX_FLOORS]);
    for line in bytes.split(|b| *b == b'\n') {
        let _ = office.apply(line, now);
    }
    let _ = remote::encode(office.scene(), now);
    office.set_capacities([1; MAX_FLOORS]);
    office
}

/// `fuzz_mutator!`'s body: mutates one node or line of `data`'s JSON lines, which
/// every other line, a transcript's source among them, passes through. A byte
/// mutation of JSON text is nearly always a parse error, and no run of them
/// spells a multi-byte character, so a parser's strings would stay as recorded
/// (<https://github.com/google/fuzzing/blob/master/docs/structure-aware-fuzzing.md>).
///
/// `None` leaves this mutation to `fuzzer_mutate`: on even seeds, for what only
/// bytes reach, a line's syntax, and when no JSON line fits `max_size`.
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
        .mutate_with(&mut Lines, &mut lines)
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

/// Any node of any JSON line, or the lines' own order: a fold's state turns on
/// which event came first.
struct Lines;

impl Mutate<Vec<Line>> for Lines {
    fn mutate(&mut self, c: &mut Candidates, lines: &mut Vec<Line>) -> mutatis::Result<()> {
        for line in lines.iter_mut() {
            if let Line::Json(v) = line {
                Json.mutate(c, v)?;
            }
        }
        if lines.len() > 1 {
            c.mutation(|ctx| {
                let index = ctx.rng().gen_index(lines.len()).unwrap_or_default();
                lines.remove(index);
                Ok(())
            })?;
            c.mutation(|ctx| {
                let a = ctx.rng().gen_index(lines.len()).unwrap_or_default();
                let b = ctx.rng().gen_index(lines.len()).unwrap_or_default();
                lines.swap(a, b);
                Ok(())
            })?;
        }
        c.mutation(|ctx| {
            let index = ctx.rng().gen_index(lines.len()).unwrap_or_default();
            lines.insert(index, lines[index].clone());
            Ok(())
        })
    }
}

/// Every node may lose its fields, take a neighboring value, or change kind.
struct Json;

impl Mutate<Value> for Json {
    fn mutate(&mut self, c: &mut Candidates, value: &mut Value) -> mutatis::Result<()> {
        match value {
            Value::Bool(flag) => m::default::<bool>().mutate(c, flag)?,
            Value::String(text) => m::default::<String>().mutate(c, text)?,
            Value::Number(number) => {
                let near = number.as_i64().unwrap_or_default();
                c.mutation(|_| Ok(*number = near.wrapping_add(1).into()))?;
                c.mutation(|_| Ok(*number = near.wrapping_sub(1).into()))?;
                c.mutation(|ctx| Ok(*number = ctx.rng().gen_i64().into()))?;
                c.mutation(|_| Ok(*number = u64::MAX.into()))?;
                c.mutation(|_| Ok(*number = i64::MIN.into()))?;
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
    fn a_transcript_folds_under_the_source_its_first_line_names() {
        let input = format!("claude-code\n{LINE}\n");
        let driven = transcript(input.as_bytes()).expect("a transcript source");
        assert_eq!(driven.registered(), 1);
        assert!(driven.wire_events() > 0, "{driven:?}");
    }

    #[test]
    fn a_first_line_naming_no_transcript_source_drives_nothing() {
        for input in ["not-a-source\n{}\n", "openclaw\n{}\n", "no newline"] {
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
    fn a_mutant_stays_json_lines_and_keeps_its_opaque_line_verbatim() {
        let mut count = 0;
        for mutant in mutants() {
            count += 1;
            assert!(mutant.ends_with('\n'), "{mutant}");
            for line in mutant.lines().filter(|line| *line != "claude-code") {
                serde_json::from_str::<Value>(line).expect(line);
            }
        }
        assert_eq!(count, 4096);
    }

    #[test]
    fn the_mutants_reach_what_a_byte_mutation_cannot() {
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
        reached("drops a field", |m| !m.contains("\"text\""));
        reached("drops a line", |m| m.lines().count() == 1);
        reached("repeats a line", |m| m.lines().count() == 3);
        reached("reorders its lines", |m| m.starts_with("{\"other\""));
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
