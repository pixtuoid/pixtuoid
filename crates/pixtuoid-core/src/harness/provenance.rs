//! A capture's `provenance.json`, parsed once at the boundary: where its
//! bytes came from and what the recorder did to them. A key no variant names
//! fails to parse, so a misspelled one is refused instead of read as absent.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Where a capture's bytes came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(tag = "origin", rename_all = "lowercase", deny_unknown_fields)]
pub enum Provenance {
    /// Real wire bytes, a real CLI's against a scripted model where `aimock`
    /// and `script` name it, stripped of every subtree no decoder reads unless
    /// `deidentified` exempts them; the recorder writes this file itself.
    Recorded(Box<Recorded>),
    /// Hand-written, and the note says how you can tell.
    Composed {
        /// How a reader can tell.
        note: NonEmpty,
    },
    /// Predates the rule; nobody recorded where it came from.
    Unknown {
        /// What is known of it.
        note: NonEmpty,
    },
}

/// A recording's record, its fields in the order the recorder writes them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Recorded {
    /// The CLI's name, as its registry probe spells it.
    pub cli: NonEmpty,
    /// The CLI's `--version` answer.
    pub version: NonEmpty,
    /// The day it was recorded.
    pub captured: NonEmpty,
    /// What recorded it, unexpanded.
    pub command: NonEmpty,
    /// The scenario's own story, and any hand edit since, which nothing in the
    /// bytes shows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<NonEmpty>,
    /// What the strip did to the bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deidentified: Option<Deidentified>,
    /// The prompt, when it was not the recorder's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<NonEmpty>,
    /// The seed the recorder was given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<NonEmpty>,
    /// The aimock version that played the model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aimock: Option<NonEmpty>,
    /// The scenario script the model played, as the checkout spells it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script: Option<NonEmpty>,
    /// What the decoders dropped, the baseline a New finding is judged against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dropped: Option<Dropped>,
}

/// What the strip did to a recording's bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(tag = "method", deny_unknown_fields)]
pub enum Deidentified {
    /// Subtrees blanked because no decoder reads them.
    #[serde(rename = "decoder-allowlist")]
    Stripped {
        /// How many, summed over every pass.
        blanked: usize,
    },
    /// Left as recorded, declared by hand for a capture that pins a wire
    /// premise the decoder drops on purpose: a test that reads raw bytes is
    /// invisible to the probe the strip runs.
    #[serde(rename = "none")]
    Exempt {
        /// Why no strip may touch it.
        why: NonEmpty,
    },
}

/// What a recording's decoders dropped, by discriminator; derived and judged
/// in [`hermetic::dropped`](super::hermetic::dropped).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Dropped {
    /// The OS it was recorded on.
    pub(crate) os: NonEmpty,
    /// Each discriminator, a pointer (`*` for an array index) where the strip
    /// kept a non-blank string a decoder of the source names, with the values
    /// the strip blanked at it.
    pub(crate) at: BTreeMap<String, BTreeSet<String>>,
}

/// A string holding more than whitespace: the type of a record's text fields,
/// so a blank one fails to parse.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(try_from = "String")]
pub struct NonEmpty(String);

impl Recorded {
    /// Whether the strip applies: not declared exempt, so its committed bytes
    /// hold only what a decoder reads.
    pub(crate) fn strips(&self) -> bool {
        !matches!(self.deidentified, Some(Deidentified::Exempt { .. }))
    }
}

impl NonEmpty {
    /// The string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for NonEmpty {
    type Error = &'static str;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        if s.trim().is_empty() {
            Err("a blank string where a value is required")
        } else {
            Ok(Self(s))
        }
    }
}

impl TryFrom<&str> for NonEmpty {
    type Error = &'static str;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        s.to_owned().try_into()
    }
}

impl std::fmt::Display for NonEmpty {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl Provenance {
    /// The record at `path`.
    ///
    /// # Errors
    ///
    /// If it cannot be read or does not parse, naming `path`.
    pub fn read(path: &Path) -> std::io::Result<Self> {
        super::read_json(path)
    }

    /// The record at `path`; none where no capture was recorded yet.
    ///
    /// # Errors
    ///
    /// If it exists and cannot be read or does not parse.
    pub fn read_if_any(path: &Path) -> std::io::Result<Option<Self>> {
        match Self::read(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            read => read.map(Some),
        }
    }

    /// Write it to `path`, pretty and newline-terminated.
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

    /// The baseline its recording wrote, if any.
    pub fn dropped(&self) -> Option<&Dropped> {
        self.recorded()?.dropped.as_ref()
    }

    /// Whether the strip applies: to a recording, as [`Recorded::strips`].
    pub(crate) fn strips(&self) -> bool {
        self.recorded().is_some_and(Recorded::strips)
    }

    /// The exemption it declares, if any.
    pub fn exemption(&self) -> Option<&NonEmpty> {
        match self.recorded()?.deidentified.as_ref()? {
            Deidentified::Exempt { why } => Some(why),
            Deidentified::Stripped { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::{PROVENANCE, sources_root};

    fn parse(json: &str) -> Result<Provenance, serde_json::Error> {
        serde_json::from_str(json)
    }

    const RECORDED: &str =
        r#"{"origin":"recorded","cli":"c","version":"1","captured":"d","command":"x"}"#;

    /// serde's docs (https://serde.rs/container-attrs.html) say nothing of
    /// `deny_unknown_fields` inside an internally tagged newtype variant; the
    /// whole gate rests on it.
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

    /// Without it, a required key made optional passes every test once the
    /// README's row follows.
    #[test]
    fn a_record_missing_a_required_key_fails_to_parse() {
        for key in ["cli", "version", "captured", "command"] {
            let without: serde_json::Map<String, serde_json::Value> =
                serde_json::from_str::<serde_json::Map<_, _>>(RECORDED)
                    .expect("json")
                    .into_iter()
                    .filter(|(k, _)| k != key)
                    .collect();
            let text = serde_json::to_string(&without).expect("json");
            assert!(parse(&text).is_err(), "a recording without {key:?}");
        }
        assert!(parse(r#"{"origin":"composed"}"#).is_err());
        assert!(parse(r#"{"origin":"unknown"}"#).is_err());
    }

    #[test]
    fn a_blank_required_string_fails_to_parse() {
        assert!(parse(&RECORDED.replace(r#""cli":"c""#, r#""cli":" ""#)).is_err());
        assert!(parse(r#"{"origin":"unknown","note":""}"#).is_err());
        let why = RECORDED.replace(
            r#""command":"x""#,
            r#""command":"x","deidentified":{"method":"none","why":"\n"}"#,
        );
        assert!(parse(&why).is_err(), "an exemption without its reason");
    }

    #[test]
    fn a_record_round_trips_in_the_order_the_recorder_writes() {
        let text = r#"{
  "origin": "recorded",
  "cli": "copilot",
  "version": "1.0.98",
  "captured": "2026-10-19",
  "command": "just hermetic --record copilot",
  "note": "n",
  "deidentified": {
    "method": "decoder-allowlist",
    "blanked": 38
  },
  "prompt": "p",
  "seed": "s",
  "aimock": "1.45.1",
  "script": "tests/sources/hermetic/copilot/tool-run.json",
  "dropped": {
    "os": "macos",
    "at": {
      "/type": [
        "assistant.turn_start"
      ]
    }
  }
}
"#;
        let p = parse(text).expect("parses");
        assert_eq!(
            format!("{}\n", serde_json::to_string_pretty(&p).expect("json")),
            text
        );
        for unset in [RECORDED, r#"{"origin":"composed","note":"n"}"#] {
            let p = parse(unset).expect("parses");
            assert_eq!(
                serde_json::to_string(&p).expect("json"),
                unset,
                "an unset field writes no key"
            );
        }
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
        assert_eq!(p.exemption().map(NonEmpty::as_str), Some("raw bytes"));
        assert!(
            !parse(r#"{"origin":"composed","note":"n"}"#)
                .expect("parses")
                .strips()
        );
    }

    #[test]
    fn no_record_is_none_and_a_bad_one_names_its_path() {
        let d = tempfile::tempdir().expect("tempdir");
        let path = d.path().join(PROVENANCE);
        assert_eq!(Provenance::read_if_any(&path).expect("absent"), None);
        std::fs::write(&path, "{}").expect("write");
        let e = Provenance::read_if_any(&path).expect_err("no origin");
        assert_eq!(e.kind(), std::io::ErrorKind::InvalidData);
        assert!(e.to_string().contains(PROVENANCE), "{e}");
        let e = Provenance::read(d.path()).expect_err("a directory");
        assert!(
            e.to_string().contains(&d.path().display().to_string()),
            "{e}"
        );
    }

    /// The fixtures README's table is the prose a human reads instead of this
    /// type, so each row is the variant's required keys and doc comment.
    #[test]
    fn the_fixtures_readme_states_each_origin() {
        let schema = serde_json::to_value(schemars::schema_for!(Provenance)).expect("json");
        let readme = sources_root().join("fixtures/README.md");
        let readme = std::fs::read_to_string(&readme).expect("README");
        let variants = schema["oneOf"].as_array().expect("a schema per origin");
        for v in variants {
            let required: Vec<String> = v["required"]
                .as_array()
                .expect("required keys")
                .iter()
                .filter_map(|k| k.as_str().filter(|k| *k != "origin"))
                .map(|k| format!("`{k}`"))
                .collect();
            let means = v["description"].as_str().expect("a doc comment");
            let row = format!(
                "| `{}` | {} | {} |",
                v["properties"]["origin"]["const"].as_str().expect("tagged"),
                required.join(", "),
                means.split_whitespace().collect::<Vec<_>>().join(" ")
            );
            assert!(
                readme.contains(&row),
                "fixtures/README.md must carry this row verbatim:\n  {row}"
            );
        }
    }
}
