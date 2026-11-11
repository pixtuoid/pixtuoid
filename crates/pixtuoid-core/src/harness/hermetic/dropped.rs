//! The baseline a New finding is judged against: at each discriminator of a
//! source's transcript, the values its decoders dropped. A hermetic recording
//! writes it into its scenario's provenance ([`Dropped`]); a run that drops a
//! value there its recording did not is New, a type the CLI added that no
//! decoder reads.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::{Profile, Scenario, literals};
use crate::harness::PROVENANCE;
use crate::harness::captures::json_lines;
use crate::harness::provenance::{Dropped, Provenance};
use crate::harness::reads::{is_hook_envelope, string_leaves, strip_lines};

/// One transcript's lines, raw and as the strip keeps them.
pub(crate) type Pair = (Vec<Value>, Vec<Value>);

impl Dropped {
    /// What `source`'s decoders drop in `files`.
    ///
    /// # Errors
    ///
    /// If a file cannot be read or the decoders cannot be parsed.
    pub fn of_files(source: &str, files: &[PathBuf]) -> std::io::Result<Self> {
        Self::of(source, &pairs(source, files)?)
    }

    fn of(source: &str, pairs: &[Pair]) -> std::io::Result<Self> {
        let named = literals::own_exact(source)?;
        let shown = shown_at(&named, pairs);
        let mut at: BTreeMap<String, BTreeSet<String>> = pairs
            .iter()
            .flat_map(|(_, kept)| kept)
            .flat_map(string_set)
            .filter(|(p, s)| named.contains(s) && !shown.contains(p))
            .map(|(p, _)| (p, BTreeSet::new()))
            .collect();
        for (p, v) in blanked(&at, pairs) {
            at.entry(p).or_default().insert(v);
        }
        Ok(Self {
            os: std::env::consts::OS
                .try_into()
                .map_err(std::io::Error::other)?,
            at,
        })
    }

    /// Whether a run on this OS is judged against it: a type only another OS's
    /// build writes is not New there.
    pub(crate) fn judges_here(&self) -> bool {
        self.os.as_str() == std::env::consts::OS
    }

    /// Each `(pointer, value)` `pairs` drop at this baseline's discriminators
    /// that it lacks.
    ///
    /// # Errors
    ///
    /// If `source`'s decoders cannot be parsed.
    pub(crate) fn new_in(
        &self,
        source: &str,
        pairs: &[Pair],
    ) -> std::io::Result<BTreeSet<(String, String)>> {
        // Judged again on the run, so a baseline recorded before a pointer
        // proved to be text names nothing New there.
        let shown = shown_at(&literals::own_exact(source)?, pairs);
        Ok(blanked(&self.at, pairs)
            .into_iter()
            .filter(|(p, v)| {
                !shown.contains(p) && !self.at.get(p).is_some_and(|seen| seen.contains(v))
            })
            .collect())
    }
}

/// Each transcript of `files` the strip applies to, as a [`Pair`]: a hook
/// envelope's unread name already breadcrumbs (`drift::unknown_event`).
///
/// # Errors
///
/// If a file cannot be read.
pub(crate) fn pairs(source: &str, files: &[PathBuf]) -> std::io::Result<Vec<Pair>> {
    let mut out = Vec::new();
    for file in files.iter().filter(|f| !is_hook_envelope(f)) {
        let raw = json_lines(file)?;
        let mut kept = raw.clone();
        if strip_lines(source, file, &mut kept) {
            out.push((raw, kept));
        }
    }
    Ok(out)
}

/// Why a run judged no new types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unjudged {
    /// Its recording wrote no baseline: one `--record` writes it.
    NoBaseline,
    /// Its baseline was recorded on this other OS.
    RecordedOn(String),
}

/// The baseline `s`'s recording wrote; none for a scenario not yet recorded.
///
/// # Errors
///
/// If its provenance cannot be read or does not parse.
pub(crate) fn baseline_of(p: &Profile, s: &Scenario) -> std::io::Result<Option<Dropped>> {
    baseline_at(&s.fixture(p.source))
}

fn baseline_at(fixture: &Path) -> std::io::Result<Option<Dropped>> {
    let path = crate::harness::sources_root()
        .join(fixture)
        .join(PROVENANCE);
    Ok(Provenance::read_if_any(&path)?.and_then(|p| p.dropped().cloned()))
}

/// Each `(pointer, value)` a raw line holds at one of `at`'s pointers that its
/// kept line does not: what the strip blanked there.
fn blanked(at: &BTreeMap<String, BTreeSet<String>>, pairs: &[Pair]) -> BTreeSet<(String, String)> {
    let mut out = BTreeSet::new();
    for (raw, kept) in pairs {
        for (r, k) in raw.iter().zip(kept) {
            let kept = string_set(k);
            out.extend(
                string_set(r)
                    .into_iter()
                    .filter(|leaf| at.contains_key(&leaf.0) && !kept.contains(leaf)),
            );
        }
    }
    out
}

/// Each pointer where `pairs` keep a string no decoder names: text a decoder
/// shows, not a type it dispatches on, so no discriminator.
fn shown_at(named: &BTreeSet<String>, pairs: &[Pair]) -> BTreeSet<String> {
    pairs
        .iter()
        .flat_map(|(_, kept)| kept)
        .flat_map(string_set)
        .filter(|(_, s)| !s.is_empty() && !named.contains(s))
        .map(|(p, _)| p)
        .collect()
}

fn string_set(v: &Value) -> BTreeSet<(String, String)> {
    let mut out = BTreeSet::new();
    string_leaves(v, "", &mut out);
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// copilot dispatches on `/type`: `tool.execution_start` is a literal of
    /// its decoder, and `assistant.turn_start` one it drops.
    fn copilot_run() -> Pair {
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

    /// claude-code decides on each content block's `type`
    /// (`claude_code.rs`' `tool_use` check), an array element's field.
    #[test]
    fn a_discriminator_inside_an_array_element_counts() {
        let raw =
            vec![json!({"message": {"content": [{"type": "tool_use"}, {"type": "thinking"}]}})];
        let kept = vec![json!({"message": {"content": [{"type": "tool_use"}, {"type": ""}]}})];
        let d = Dropped::of("claude-code", &[(raw, kept)]).expect("parse");
        assert!(
            d.at["/message/content/*/type"].contains("thinking"),
            "{d:?}"
        );
    }

    /// grok's `title` names a call's tool, `spawn_subagent` a literal of its
    /// decoder, and an update's display text: text it shows, not a type it
    /// dispatches on.
    #[test]
    fn a_pointer_that_keeps_text_no_decoder_names_is_no_discriminator() {
        let titled = |title| json!({"params": {"update": {"title": title}}});
        let raw = vec![
            titled("spawn_subagent"),
            titled("read_file"),
            titled("Read `NOTE.txt`"),
        ];
        let kept = vec![titled("spawn_subagent"), titled("read_file"), titled("")];
        let pairs = [(raw, kept)];
        let d = Dropped::of("grok", &pairs).expect("parse");
        assert!(!d.at.contains_key("/params/update/title"), "{d:?}");
        let recorded = Dropped {
            os: d.os.clone(),
            at: BTreeMap::from([("/params/update/title".to_owned(), BTreeSet::new())]),
        };
        assert_eq!(
            recorded.new_in("grok", &pairs).expect("parse"),
            BTreeSet::new()
        );
    }

    #[test]
    fn a_run_is_new_only_where_its_recording_did_not_drop_the_value() {
        let d = Dropped::of("copilot", &[copilot_run()]).expect("parse");
        assert!(
            d.new_in("copilot", &[copilot_run()])
                .expect("parse")
                .is_empty()
        );
        let run = (
            vec![
                json!({"type": "assistant.reasoning_start"}),
                json!({"type": "x", "data": {"turnId": "t"}}),
            ],
            vec![json!({}), json!({})],
        );
        assert_eq!(
            d.new_in("copilot", &[run]).expect("parse"),
            BTreeSet::from([
                ("/type".to_owned(), "assistant.reasoning_start".to_owned()),
                ("/type".to_owned(), "x".to_owned()),
            ]),
            "only at the baseline's pointers: `/data/turnId` is none"
        );
    }

    #[test]
    fn a_scenario_not_yet_recorded_has_no_baseline() {
        assert_eq!(
            baseline_at(Path::new("copilot/fixtures/never-recorded-hermetic"))
                .expect("a missing record is no error"),
            None
        );
    }
}
