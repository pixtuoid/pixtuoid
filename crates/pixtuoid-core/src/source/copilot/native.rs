use std::path::PathBuf;

use anyhow::{Context as _, Result};

use super::{SOURCE_NAME, copilot_home};
use crate::source::jsonl::JsonlWatcher;
use crate::source::registry::{Native, NativeTranscript};
use crate::source::{Source, TaggedSender};

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
        JsonlWatcher::new(self.sessions_root.clone(), SOURCE_NAME.to_string())
            .context("no transcript row")?
            .run(tx)
            .await
    }
}

pub(crate) const TRANSCRIPT: NativeTranscript = NativeTranscript {
    activity_recency: None,
    head_label: None,
    derive_label: None,
    cwd_derive: None,
};

pub(crate) const NATIVE: Native = Native {
    root: Some(sessions_root),
    focus_probe: None,
};

fn sessions_root() -> Option<PathBuf> {
    Some(CopilotSource::default_paths().sessions_root)
}
