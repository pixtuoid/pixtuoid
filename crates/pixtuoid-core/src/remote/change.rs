//! What a device's [`Ask::Change`](super::Ask::Change) did, and the words a
//! device and the Sources panel show for it.

use serde::{Deserialize, Deserializer, Serialize};

use super::display_safe_text;
use crate::source::decoder::display_safe;

/// [`display_safe_text`] for text that may be absent.
fn display_safe_some<'de, D: Deserializer<'de>>(text: D) -> Result<Option<String>, D::Error> {
    Option::<String>::deserialize(text).map(|text| text.as_deref().map(display_safe))
}

/// What an [`Ask::Change`](super::Ask::Change) did. A failure's reason stays on
/// the server: it can quote another CLI's config.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Changed {
    #[serde(deserialize_with = "display_safe_text")]
    source: String,
    on: bool,
    outcome: Outcome,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    cli_absent: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    hook_off_path: bool,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "display_safe_some"
    )]
    step: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "display_safe_some"
    )]
    plugin_left: Option<String>,
}

impl Changed {
    /// `&'static str`s, as [`SourceRow::new`](super::SourceRow::new) takes: `step`
    /// is what a target still asks of the user after a connect, and `plugin_left`
    /// the CLI a disconnect couldn't reach to deregister its plugin.
    pub fn new(
        source: &'static str,
        on: bool,
        outcome: Outcome,
        hook_off_path: bool,
        step: Option<&'static str>,
        plugin_left: Option<&'static str>,
    ) -> Self {
        Self {
            source: source.to_owned(),
            on,
            outcome,
            cli_absent: false,
            hook_off_path,
            step: step.map(str::to_owned),
            plugin_left: plugin_left.map(str::to_owned),
        }
    }

    /// A connect of `source` that did nothing, its CLI not on the server's machine.
    pub fn absent_cli(source: &'static str) -> Self {
        Self {
            cli_absent: true,
            ..Self::new(source, true, Outcome::NoOp, false, None, None)
        }
    }

    /// The [`SourceRow::id`](super::SourceRow::id) it changed.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Whether it was asked to be connected.
    pub fn on(&self) -> bool {
        self.on
    }

    /// What came of it.
    pub fn outcome(&self) -> Outcome {
        self.outcome
    }

    /// The sentence a device shows for it, the source named `display_name`: the
    /// Sources panel's own, but a failure's, which the panel words with its reason.
    pub fn line(&self, display_name: &str) -> String {
        let Self {
            source,
            on,
            outcome,
            cli_absent,
            hook_off_path,
            step,
            plugin_left,
        } = self;
        let mut line = match outcome {
            Outcome::Connected => format!("\u{2713} {display_name} connected"),
            Outcome::Disconnected => format!("\u{2713} {display_name} disconnected"),
            Outcome::NoOp if *cli_absent => not_detected(display_name),
            Outcome::NoOp => nothing_to_do(display_name),
            Outcome::Failed => {
                let op = if *on { "connect" } else { "disconnect" };
                return format!(
                    "{display_name}: {op} failed \u{2014} `pixtuoid {op} {source}` on the server says why"
                );
            }
            Outcome::Unknown => format!("{display_name}: done"),
        };
        if *hook_off_path {
            line.push_str(" \u{00b7} \u{26a0} pixtuoid-hook not on PATH");
        }
        // Connecting is not always the last step: OpenClaw's `plugins.load` is
        // `kind: "restart"` upstream, so a RUNNING gateway keeps serving without
        // our plugin until it restarts.
        if let Some(step) = step {
            line.push_str(" \u{00b7} ");
            line.push_str(step);
        }
        if let Some(cli) = plugin_left {
            line.push_str(" \u{00b7} \u{26a0} ");
            line.push_str(&plugin_left_registered(cli));
        }
        line
    }
}

/// How a change of the source named `display_name` that had nothing to do reads.
pub fn nothing_to_do(display_name: &str) -> String {
    format!("nothing to do for {display_name}")
}

/// How the source named `display_name` reads when its CLI isn't on the machine
/// to connect.
pub fn not_detected(display_name: &str) -> String {
    format!("{display_name} not detected on this machine")
}

/// How a disconnect that couldn't reach `command` to deregister the plugin
/// reads, which stays registered with that CLI.
pub fn plugin_left_registered(command: &str) -> String {
    format!("plugin left registered ({command} not on PATH)")
}

/// What a change came to, as the binary's `--json` tokens spell it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Now connected.
    Connected,
    /// Now disconnected.
    Disconnected,
    /// Nothing to change: already so, or a CLI not on the machine to connect.
    NoOp,
    /// It failed; the server keeps why.
    Failed,
    /// An outcome from a newer server.
    #[serde(other)]
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::{Answer, Ask};
    use crate::source::decoder::MAX_DECODED_FIELD_CHARS;

    /// A change's wire, frozen as an ask's is.
    #[test]
    fn a_change_and_what_it_did_keep_their_shape() {
        fn wire(value: &impl Serialize) -> String {
            serde_json::to_string(value).expect("it encodes")
        }
        let ask = Ask::Change {
            source: "codex".into(),
            on: true,
        };
        let line = wire(&ask);
        assert_eq!(line, r#"{"ask":"change","source":"codex","on":true}"#);
        assert_eq!(serde_json::from_str::<Ask>(&line).unwrap(), ask);

        for (changed, json) in [
            (
                Changed::new(
                    "openclaw",
                    true,
                    Outcome::Connected,
                    true,
                    Some("restart it"),
                    None,
                ),
                r#"{"answer":"changed","source":"openclaw","on":true,"outcome":"connected","hook_off_path":true,"step":"restart it"}"#,
            ),
            (
                Changed::new(
                    "codex",
                    false,
                    Outcome::Disconnected,
                    false,
                    None,
                    Some("codex"),
                ),
                r#"{"answer":"changed","source":"codex","on":false,"outcome":"disconnected","plugin_left":"codex"}"#,
            ),
            (
                Changed::new("kimi", true, Outcome::NoOp, false, None, None),
                r#"{"answer":"changed","source":"kimi","on":true,"outcome":"no_op"}"#,
            ),
            (
                Changed::absent_cli("kimi"),
                r#"{"answer":"changed","source":"kimi","on":true,"outcome":"no_op","cli_absent":true}"#,
            ),
            (
                Changed::new("kimi", false, Outcome::Failed, false, None, None),
                r#"{"answer":"changed","source":"kimi","on":false,"outcome":"failed"}"#,
            ),
        ] {
            let answer = Answer::Changed(changed);
            assert_eq!(wire(&answer), json);
            assert_eq!(Answer::from_line(json.as_bytes()), Some(answer));
        }
    }

    #[test]
    fn a_change_reads_back_through_its_accessors() {
        let changed = Changed::new("codex", true, Outcome::Failed, false, None, None);
        assert_eq!(changed.source(), "codex");
        assert!(changed.on());
        assert_eq!(changed.outcome(), Outcome::Failed);
    }

    #[test]
    fn what_a_change_did_reads_as_one_sentence() {
        let line = |changed: Changed| changed.line("Codex");
        let connected = |hook_off_path, step| {
            Changed::new("codex", true, Outcome::Connected, hook_off_path, step, None)
        };
        assert_eq!(line(connected(false, None)), "\u{2713} Codex connected");
        assert_eq!(
            line(connected(true, Some("restart it"))),
            "\u{2713} Codex connected \u{b7} \u{26a0} pixtuoid-hook not on PATH \u{b7} restart it"
        );
        let disconnected = |plugin_left| {
            Changed::new(
                "codex",
                false,
                Outcome::Disconnected,
                false,
                None,
                plugin_left,
            )
        };
        assert_eq!(line(disconnected(None)), "\u{2713} Codex disconnected");
        assert_eq!(
            line(disconnected(Some("codex"))),
            "\u{2713} Codex disconnected \u{b7} \u{26a0} plugin left registered (codex not on PATH)"
        );
        assert_eq!(
            line(Changed::new(
                "codex",
                true,
                Outcome::NoOp,
                false,
                None,
                None
            )),
            "nothing to do for Codex"
        );
        assert_eq!(
            line(Changed::absent_cli("codex")),
            "Codex not detected on this machine"
        );
        // Exact: "disconnect failed" holds "connect failed".
        for (on, op) in [(true, "connect"), (false, "disconnect")] {
            assert_eq!(
                line(Changed::new(
                    "codex",
                    on,
                    Outcome::Failed,
                    false,
                    None,
                    None
                )),
                format!("Codex: {op} failed \u{2014} `pixtuoid {op} codex` on the server says why")
            );
        }
    }

    #[test]
    fn a_newer_servers_outcome_is_unknown_and_a_hostile_change_is_stripped() {
        let dirty = format!(
            "x\u{1b}[2J\u{202e}{}",
            "y".repeat(MAX_DECODED_FIELD_CHARS * 2)
        );
        let line = serde_json::json!({
            "answer": "changed", "source": dirty, "on": true, "outcome": "from_the_future",
            "step": dirty, "plugin_left": dirty,
        })
        .to_string();
        let Some(Answer::Changed(changed)) = Answer::from_line(line.as_bytes()) else {
            panic!("an answer");
        };
        assert_eq!(changed.outcome(), Outcome::Unknown);
        let Changed {
            source,
            step,
            plugin_left,
            ..
        } = changed;
        for text in [source, step.unwrap(), plugin_left.unwrap()] {
            assert!(!text.contains(['\u{1b}', '\u{202e}']), "{text}");
            assert!(
                text.chars().count() <= MAX_DECODED_FIELD_CHARS + 1,
                "{text}"
            );
        }
    }
}
