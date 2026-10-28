use std::path::PathBuf;

use anyhow::Result;

use super::{SOURCE_NAME, copilot_home, decode_copilot_line};
use crate::source::decoder::parsed_tail_lines;
use crate::source::jsonl::JsonlWatcher;
use crate::source::registry::Native;
use crate::source::{Source, TaggedSender};

/// Copilot persists a real [`super::SESSION_SHUTDOWN`] event, so a transcript
/// that has already ended carries that marker — the first-sight gate uses it to
/// avoid resurrecting a finished session. Anchored on the structural top-level
/// `type`: copilot persists tool `arguments` verbatim in events.jsonl, so a
/// substring scan would let a grep for the marker end a live session.
fn copilot_session_ended(tail: &[u8]) -> bool {
    parsed_tail_lines(tail)
        .any(|v| v.get("type").and_then(|t| t.as_str()) == Some(super::SESSION_SHUTDOWN))
}

/// Source that watches the Copilot session-state directory.
#[derive(Debug)]
pub struct CopilotSource {
    /// The watched Copilot `session-state` root; each session's `events.jsonl` lives under it.
    pub sessions_root: PathBuf,
}

impl CopilotSource {
    /// Construct pointed at the default Copilot `session-state` root.
    pub fn default_paths() -> Self {
        Self {
            sessions_root: copilot_home().join("session-state"),
        }
    }
}

impl Source for CopilotSource {
    fn name(&self) -> &str {
        SOURCE_NAME
    }

    async fn run(self: Box<Self>, tx: TaggedSender) -> Result<()> {
        JsonlWatcher::new(
            self.sessions_root.clone(),
            SOURCE_NAME.to_string(),
            decode_copilot_line,
            copilot_session_ended,
        )
        .run(tx)
        .await
    }
}

pub(crate) const NATIVE: Native = Native {
    root: Some(sessions_root),
    focus_probe: None,
};

fn sessions_root() -> Option<PathBuf> {
    Some(CopilotSource::default_paths().sessions_root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_ended_marker_is_anchored_on_the_type_field() {
        assert!(copilot_session_ended(
            br#"{"type":"session.shutdown","data":{}}"#
        ));
        assert!(!copilot_session_ended(
            br#"{"type":"tool.execution_complete","data":{"result":{"content":"run session.shutdown the cluster"}}}"#
        ));
        assert!(!copilot_session_ended(
            br#"{"type":"tool.execution_start"}"#
        ));
    }

    /// Copilot's event schema names no `session_end` (the release asset's
    /// `schemas/session-events.schema.json`), so a line typed so is not its end.
    #[test]
    fn a_session_end_type_copilot_never_writes_does_not_end_it() {
        assert!(!copilot_session_ended(
            br#"{"type":"session_end","data":{}}"#
        ));
    }

    #[test]
    fn session_ended_matches_marker_after_a_partial_first_tail_line() {
        assert!(copilot_session_ended(
            b"...tail-fragment\"}\n{\"type\":\"session.shutdown\",\"data\":{}}\n"
        ));
    }

    #[test]
    fn session_ended_ignores_marker_bytes_inside_tool_arguments() {
        assert!(!copilot_session_ended(
            br#"{"type":"tool.execution_start","data":{"toolName":"grep","arguments":{"pattern":"session_end"}}}"#
        ));
        assert!(!copilot_session_ended(
            br#"{"type":"tool.execution_start","data":{"arguments":{"pattern":"\"type\":\"session.shutdown\""}}}"#
        ));
        assert!(!copilot_session_ended(
            br#"{"type":"tool.execution_complete","data":{"result":{"type":"session_end"}}}"#
        ));
    }
}
