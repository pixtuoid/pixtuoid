//! The `native`-only runtime half of the Antigravity source: `AntigravitySource`
//! and its `JsonlWatcher` wiring. The pure decoder stays in the parent module.

use std::path::PathBuf;

use anyhow::Result;

use super::{CLI_APP_DATA_DIR, SOURCE_NAME, decode_ag_line, gemini_dir};
use crate::source::jsonl::JsonlWatcher;
use crate::source::registry::Native;
use crate::source::{Source, TaggedSender};

/// Source that watches Antigravity CLI conversation log directories.
#[derive(Debug)]
pub struct AntigravitySource {
    /// The watched Antigravity brain-dir root; conversation-log JSONL lives under it.
    pub brain_root: PathBuf,
}

impl AntigravitySource {
    /// The Antigravity **CLI** (`agy`) brain dir.
    pub fn default_paths() -> Self {
        Self {
            brain_root: gemini_dir().join(CLI_APP_DATA_DIR).join("brain"),
        }
    }
}

impl Source for AntigravitySource {
    fn name(&self) -> &str {
        SOURCE_NAME
    }

    async fn run(self: Box<Self>, tx: TaggedSender) -> Result<()> {
        let watcher = JsonlWatcher::new(
            self.brain_root.clone(),
            SOURCE_NAME.to_string(),
            decode_ag_line,
            ag_session_ended,
        );
        watcher.run(tx).await
    }
}

fn ag_session_ended(_tail: &[u8]) -> bool {
    false
}

pub(crate) const NATIVE: Native = Native {
    root: Some(brain_root),
    focus_probe: None,
};

fn brain_root() -> Option<PathBuf> {
    Some(AntigravitySource::default_paths().brain_root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ag_session_ended_is_always_false() {
        assert!(!ag_session_ended(b"x"));
        assert!(!ag_session_ended(b""));
    }

    #[test]
    fn brain_root_is_the_cli_brain_under_dot_gemini() {
        let p = AntigravitySource::default_paths().brain_root;
        assert!(
            p.ends_with(
                PathBuf::from(".gemini")
                    .join("antigravity-cli")
                    .join("brain")
            ),
            "brain_root must be <home>/.gemini/antigravity-cli/brain, got {p:?}"
        );
    }
}
