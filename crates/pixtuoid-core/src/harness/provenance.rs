//! A capture's `provenance.json`, parsed once at the boundary: where its
//! bytes came from and what the recorder did to them. A key no variant names
//! fails to parse, so a misspelled one is refused instead of read as absent.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::PROVENANCE;
use super::hermetic::dropped::Dropped;

/// Where a capture's bytes came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "origin", rename_all = "lowercase", deny_unknown_fields)]
pub enum Provenance {
    /// Real wire bytes, a real CLI's against a scripted model where `aimock`
    /// and `script` name it.
    Recorded(Box<Recorded>),
    /// Hand-written, and the note says how you can tell.
    Composed {
        /// How a reader can tell.
        note: String,
    },
    /// Predates the rule; nobody recorded where it came from.
    Unknown {
        /// What is known of it.
        note: String,
    },
}

/// A recording's record, its fields in the order the recorder writes them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recorded {
    /// The CLI's name, as its registry probe spells it.
    pub cli: String,
    /// The CLI's `--version` answer.
    pub version: String,
    /// The day it was recorded.
    pub captured: String,
    /// What recorded it, unexpanded.
    pub command: String,
    /// A hand edit, said where a reader would otherwise miss it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// What the strip did to the bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deidentified: Option<Deidentified>,
    /// The prompt, when it was not the scenario's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// The seed, when one was set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<String>,
    /// The aimock version that played the model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aimock: Option<String>,
    /// The scenario script the model played, as the checkout spells it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,
    /// What the decoders dropped, the baseline a New finding is judged against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dropped: Option<Dropped>,
}

/// What the strip did to a recording's bytes. `Exempt` is declared by hand,
/// for a capture whose purpose is to pin a wire premise the decoder drops on
/// purpose: a test that reads raw bytes is invisible to the probe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", deny_unknown_fields)]
pub enum Deidentified {
    /// Subtrees blanked because no decoder reads them.
    #[serde(rename = "decoder-allowlist")]
    Stripped {
        /// How many, summed over every pass.
        blanked: usize,
    },
    /// Left as recorded, for the reason given.
    #[serde(rename = "none")]
    Exempt {
        /// Why no strip may touch it.
        why: String,
    },
}

impl Provenance {
    /// The record in `dir`.
    ///
    /// # Errors
    ///
    /// If it cannot be read or does not parse.
    pub fn read(dir: &Path) -> std::io::Result<Self> {
        let path = dir.join(PROVENANCE);
        serde_json::from_str(&std::fs::read_to_string(&path)?).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{}: {e}", path.display()),
            )
        })
    }

    /// The record in `dir`; none where no capture was recorded yet.
    ///
    /// # Errors
    ///
    /// If it exists and cannot be read or does not parse.
    pub fn read_if_any(dir: &Path) -> std::io::Result<Option<Self>> {
        match Self::read(dir) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            read => read.map(Some),
        }
    }

    /// Write it to `path`, pretty and newline-terminated, as committed.
    ///
    /// # Errors
    ///
    /// If it cannot be written.
    pub fn write_to(&self, path: &Path) -> std::io::Result<()> {
        std::fs::write(path, format!("{}\n", serde_json::to_string_pretty(self)?))
    }

    /// Its recording's record, if it is one.
    pub fn recorded(&self) -> Option<&Recorded> {
        match self {
            Self::Recorded(r) => Some(r.as_ref()),
            _ => None,
        }
    }

    /// Whether the strip applies: a recording not declared exempt, whose
    /// committed bytes then hold only what a decoder reads.
    pub(crate) fn strips(&self) -> bool {
        self.recorded()
            .is_some_and(|r| !matches!(r.deidentified, Some(Deidentified::Exempt { .. })))
    }

    /// The exemption it declares, if any.
    pub fn exemption(&self) -> Option<&str> {
        match self.recorded()?.deidentified.as_ref()? {
            Deidentified::Exempt { why } => Some(why),
            Deidentified::Stripped { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> Result<Provenance, serde_json::Error> {
        serde_json::from_str(json)
    }

    const RECORDED: &str =
        r#"{"origin":"recorded","cli":"c","version":"1","captured":"d","command":"x"}"#;

    /// serde's docs say nothing of `deny_unknown_fields` inside an internally
    /// tagged newtype variant; the whole gate rests on it.
    #[test]
    fn a_key_no_variant_names_fails_to_parse() {
        assert!(parse(RECORDED).is_ok());
        let stray = RECORDED.replace(r#""command":"x""#, r#""command":"x","droped":{}"#);
        assert!(parse(&stray).is_err(), "a recording's stray key");
        assert!(parse(r#"{"origin":"composed","note":"n","cli":"c"}"#).is_err());
        let method = RECORDED.replace(
            r#""command":"x""#,
            r#""command":"x","deidentified":{"method":"none","why":"w","blanked":1}"#,
        );
        assert!(parse(&method).is_err(), "a stray key inside `deidentified`");
    }

    #[test]
    fn a_record_round_trips_in_the_order_the_recorder_writes() {
        let text = r#"{
  "origin": "recorded",
  "cli": "copilot",
  "version": "1.0.98",
  "captured": "2026-10-19",
  "command": "just hermetic --record copilot",
  "deidentified": {
    "method": "decoder-allowlist",
    "blanked": 38
  },
  "aimock": "1.45.1",
  "script": "tests/sources/hermetic/copilot/tool-run.json"
}
"#;
        let p = parse(text).expect("parses");
        assert_eq!(
            format!("{}\n", serde_json::to_string_pretty(&p).expect("json")),
            text
        );
        assert!(p.strips());
        assert_eq!(p.exemption(), None);
    }

    #[test]
    fn a_recording_not_yet_stripped_is_stripped() {
        assert!(parse(RECORDED).expect("parses").strips());
    }

    #[test]
    fn an_exempt_recording_is_not_stripped_and_says_why() {
        let p = parse(&RECORDED.replace(
            r#""command":"x""#,
            r#""command":"x","deidentified":{"method":"none","why":"raw bytes"}"#,
        ))
        .expect("parses");
        assert!(!p.strips());
        assert_eq!(p.exemption(), Some("raw bytes"));
        assert!(
            !parse(r#"{"origin":"composed","note":"n"}"#)
                .expect("parses")
                .strips()
        );
    }

    #[test]
    fn no_record_is_none_and_a_bad_one_names_its_path() {
        let d = tempfile::tempdir().expect("tempdir");
        assert_eq!(Provenance::read_if_any(d.path()).expect("absent"), None);
        std::fs::write(d.path().join(PROVENANCE), "{}").expect("write");
        let e = Provenance::read_if_any(d.path()).expect_err("no origin");
        assert!(e.to_string().contains(PROVENANCE), "{e}");
    }
}
