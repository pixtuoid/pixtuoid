//! The `native`-only runtime half of the Antigravity source: `AntigravitySource`
//! and its `JsonlWatcher` wiring. The pure decoder stays in the parent module.

use std::path::PathBuf;

use anyhow::{Context as _, Result};

use super::{CLI_APP_DATA_DIR, SOURCE_NAME, gemini_dir};
use crate::source::jsonl::JsonlWatcher;
use crate::source::registry::{Native, NativeTranscript};
use crate::source::{Source, TaggedSender};

/// Source that watches Antigravity CLI conversation log directories.
#[derive(Debug)]
pub struct AntigravitySource {
    /// The watched Antigravity brain-dir root; conversation-log JSONL lives under it.
    pub brain_root: PathBuf,
}

impl AntigravitySource {
    /// The Antigravity **CLI** (`agy`) brain dir, if [`gemini_dir`] resolves.
    pub fn default_paths() -> Option<Self> {
        Some(Self {
            brain_root: gemini_dir()?.join(CLI_APP_DATA_DIR).join("brain"),
        })
    }
}

impl Source for AntigravitySource {
    fn name(&self) -> &str {
        SOURCE_NAME
    }

    async fn run(self: Box<Self>, tx: TaggedSender) -> Result<()> {
        let watcher = JsonlWatcher::new(self.brain_root.clone(), SOURCE_NAME.to_string())
            .context("no transcript row")?;
        watcher.run(tx).await
    }
}

pub(crate) const TRANSCRIPT: NativeTranscript = NativeTranscript {
    activity_recency: None,
    head_label: None,
    derive_label: None,
    cwd_derive: None,
};

pub(crate) const NATIVE: Native = Native {
    root: Some(brain_root),
    focus_probe: None,
};

fn brain_root() -> Option<PathBuf> {
    AntigravitySource::default_paths().map(|s| s.brain_root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brain_root_is_the_cli_brain_under_dot_gemini() {
        let home = tempfile::tempdir().unwrap();
        let pinned = [
            ("HOME", Some(home.path().as_os_str())),
            ("USERPROFILE", Some(home.path().as_os_str())),
        ];
        temp_env::with_vars(pinned, || {
            assert_eq!(
                AntigravitySource::default_paths().map(|s| s.brain_root),
                Some(
                    home.path()
                        .join(".gemini")
                        .join("antigravity-cli")
                        .join("brain")
                )
            );
        });
    }
}
