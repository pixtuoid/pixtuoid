//! The unknown-type report's baseline: at each discriminator of a source's
//! transcript, the values its decoders dropped. A hermetic recording writes it
//! into its scenario's provenance (`dropped`); a run that drops a value there
//! its recording did not is New, a type the CLI added that no decoder reads.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use std::path::PathBuf;

use super::literals;
use crate::harness::captures::json_lines;
use crate::harness::reads::{child_pointer, is_hook_envelope, strip_lines};

/// What a recording's decoders dropped, by discriminator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dropped {
    /// The OS it was recorded on: a type only another OS's build writes is not
    /// New there.
    pub os: String,
    /// Each discriminator, an index-free pointer where the strip kept a
    /// non-blank string a decoder of the source names, with the values the
    /// strip blanked at it.
    pub at: BTreeMap<String, BTreeSet<String>>,
}

impl Dropped {
    /// What a recording of `source` in `files` drops: each transcript's lines
    /// raw and as the strip would keep them. Hook envelopes have none.
    ///
    /// # Errors
    ///
    /// If a file cannot be read or the decoders cannot be parsed.
    pub fn of_run(source: &str, files: &[PathBuf]) -> std::io::Result<Self> {
        let mut pairs = Vec::new();
        for file in files.iter().filter(|f| !is_hook_envelope(f)) {
            let raw = json_lines(file)?;
            let mut kept = raw.clone();
            if strip_lines(source, file, &mut kept) {
                pairs.push((raw, kept));
            }
        }
        Self::of(source, &pairs)
    }

    /// What a recording of `source` dropped, from each transcript's lines raw
    /// and as the strip kept them.
    ///
    /// # Errors
    ///
    /// If the decoders cannot be parsed.
    pub fn of(source: &str, files: &[(Vec<Value>, Vec<Value>)]) -> std::io::Result<Self> {
        let named = literals::own_exact(source)?;
        let mut at: BTreeMap<String, BTreeSet<String>> = files
            .iter()
            .flat_map(|(_, kept)| kept)
            .flat_map(strings)
            .filter(|(_, s)| named.contains(*s))
            .map(|(p, _)| (p, BTreeSet::new()))
            .collect();
        for (raw, kept) in files {
            for (p, v) in blanked(&at, raw, kept) {
                at.entry(p).or_default().insert(v);
            }
        }
        Ok(Self {
            os: std::env::consts::OS.to_owned(),
            at,
        })
    }

    /// Each `(pointer, value)` a run's `raw` and `kept` lines drop at this
    /// baseline's discriminators that the baseline lacks.
    pub fn new_in(&self, raw: &[Value], kept: &[Value]) -> BTreeSet<(String, String)> {
        blanked(&self.at, raw, kept)
            .into_iter()
            .filter(|(p, v)| !self.at.get(p).is_some_and(|seen| seen.contains(v)))
            .collect()
    }
}

/// Each `(pointer, value)` a raw line holds at one of `at`'s pointers as a
/// non-blank string its kept line blanked or lost.
fn blanked(
    at: &BTreeMap<String, BTreeSet<String>>,
    raw: &[Value],
    kept: &[Value],
) -> BTreeSet<(String, String)> {
    let mut out = BTreeSet::new();
    for (r, k) in raw.iter().zip(kept) {
        for (p, v) in strings(r) {
            let read = k
                .pointer(&p)
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty());
            if at.contains_key(&p) && !read {
                out.insert((p, v.to_owned()));
            }
        }
    }
    out
}

/// Each non-blank string of `v` by its pointer, objects only: a discriminator
/// is a field, never an array element.
fn strings(v: &Value) -> Vec<(String, &str)> {
    fn walk<'a>(v: &'a Value, at: &str, out: &mut Vec<(String, &'a str)>) {
        match v {
            Value::Object(m) => {
                for (k, x) in m {
                    walk(x, &child_pointer(at, k), out);
                }
            }
            Value::String(s) if !s.is_empty() => out.push((at.to_owned(), s)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(v, "", &mut out);
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// copilot dispatches on `/type`: `tool.execution_start` is a literal of
    /// its decoder, and `assistant.turn_start` one it drops.
    fn copilot_run() -> (Vec<Value>, Vec<Value>) {
        let raw = vec![
            json!({"type": "tool.execution_start", "data": {"toolName": "zz-custom"}}),
            json!({"type": "assistant.turn_start", "data": {"turnId": "t1"}}),
        ];
        let kept = vec![
            json!({"type": "tool.execution_start", "data": {"toolName": "zz-custom"}}),
            json!({}),
        ];
        (raw, kept)
    }

    #[test]
    fn a_type_the_strip_blanked_at_a_named_pointer_is_dropped() {
        let d = Dropped::of("copilot", &[copilot_run()]).expect("parse");
        assert_eq!(
            d.at.get("/type"),
            Some(&BTreeSet::from(["assistant.turn_start".to_owned()]))
        );
        assert!(
            !d.at.contains_key("/data/toolName"),
            "a pointer holding only values no decoder names is no discriminator: {d:?}"
        );
    }

    #[test]
    fn a_type_kept_blank_for_another_field_is_dropped_too() {
        let raw = vec![
            json!({"type": "tool.execution_start"}),
            json!({"type": "assistant.turn_start", "cwd": "/w"}),
        ];
        let kept = vec![
            json!({"type": "tool.execution_start"}),
            json!({"type": "", "cwd": "/w"}),
        ];
        let d = Dropped::of("copilot", &[(raw, kept)]).expect("parse");
        assert!(d.at["/type"].contains("assistant.turn_start"), "{d:?}");
    }

    #[test]
    fn a_run_is_new_only_where_its_recording_did_not_drop_the_value() {
        let d = Dropped::of("copilot", &[copilot_run()]).expect("parse");
        let (raw, kept) = copilot_run();
        assert!(d.new_in(&raw, &kept).is_empty());
        let raw = vec![
            json!({"type": "assistant.reasoning_start"}),
            json!({"type": "x", "data": {"turnId": "t"}}),
        ];
        let kept = vec![json!({}), json!({})];
        assert_eq!(
            d.new_in(&raw, &kept),
            BTreeSet::from([
                ("/type".to_owned(), "assistant.reasoning_start".to_owned()),
                ("/type".to_owned(), "x".to_owned()),
            ]),
            "only at the baseline's pointers: `/data/turnId` is none"
        );
    }
}
