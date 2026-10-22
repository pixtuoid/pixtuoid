use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::source::{DynSource, TaggedSender};

/// A source's fatal exit, published on the health channel so the binary can
/// surface it (#157). `non_exhaustive` + [`SourceDeath::new`] keep a future
/// field a minor bump instead of a major at the CI semver gate.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SourceDeath {
    /// The dead source's registry name (e.g. "claude-code").
    pub source: String,
    /// Display rendering of the fatal error.
    pub error: String,
}

impl SourceDeath {
    /// Build a `SourceDeath` from the dead source's name and its display error.
    pub fn new(source: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            error: error.into(),
        }
    }
}

/// Owns a set of `Source` implementations and spawns each as its own tokio
/// task, multiplexing their events onto a single `TaggedSender`.
#[derive(Default)]
pub struct SourceManager {
    sources: Vec<Box<dyn DynSource>>,
}

impl std::fmt::Debug for SourceManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourceManager")
            .field(
                "sources",
                &self.sources.iter().map(|s| s.name()).collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl SourceManager {
    /// An empty `SourceManager` — register sources with `with_source`, then `spawn`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register one more `Source`. Builder-style — chain to add several.
    #[must_use]
    pub fn with_source(mut self, source: Box<dyn DynSource>) -> Self {
        self.sources.push(source);
        self
    }

    /// Spawn one tokio task per source. Each task gets its own clone of `tx`,
    /// so the channel stays open as long as any source is alive. A source's
    /// error is logged via `tracing` and does not abort its siblings.
    pub fn spawn(self, tx: TaggedSender) -> Vec<JoinHandle<()>> {
        // `send_modify` works with no receivers, so the no-health path can
        // share one implementation with the health one.
        let (deaths, _) = watch::channel(Vec::new());
        self.spawn_with_health(tx, deaths)
    }

    /// Like [`SourceManager::spawn`], additionally APPENDING each source's
    /// fatal exit onto the `deaths` watch channel (#157) — a death must reach
    /// a surface the user watches, not only `tracing`.
    pub fn spawn_with_health(
        self,
        tx: TaggedSender,
        deaths: watch::Sender<Vec<SourceDeath>>,
    ) -> Vec<JoinHandle<()>> {
        self.sources
            .into_iter()
            .map(|src| {
                let tx = tx.clone();
                let deaths = deaths.clone();
                let name = src.name().to_string();
                tokio::spawn(async move {
                    if let Err(e) = src.run(tx).await {
                        let chain = format!("{e:#}");
                        tracing::error!(source = %name, error = ?chain, "source died");
                        deaths.send_modify(|v| v.push(SourceDeath::new(name, chain)));
                    }
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{AgentEvent, Source, Transport};

    struct DyingSource;

    impl Source for DyingSource {
        fn name(&self) -> &str {
            "dying-test-source"
        }
        async fn run(self: Box<Self>, _tx: TaggedSender) -> anyhow::Result<()> {
            anyhow::bail!("listener exploded")
        }
    }

    struct HealthySource;

    impl Source for HealthySource {
        fn name(&self) -> &str {
            "healthy-test-source"
        }
        async fn run(self: Box<Self>, _tx: TaggedSender) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn fatal_source_exit_is_published_on_the_health_channel() {
        let (tx, _rx) = tokio::sync::mpsc::channel::<(Transport, AgentEvent)>(8);
        let (deaths_tx, deaths_rx) = watch::channel(Vec::new());
        let handles = SourceManager::new()
            .with_source(Box::new(DyingSource))
            .with_source(Box::new(HealthySource))
            .spawn_with_health(tx, deaths_tx);
        for h in handles {
            h.await.unwrap();
        }
        let deaths = deaths_rx.borrow().clone();
        assert_eq!(
            deaths,
            vec![SourceDeath::new("dying-test-source", "listener exploded")],
            "a fatal source exit must be attributed and published; a clean exit must not"
        );
    }

    /// A source's fatal error chain can embed external strings (a transcript path
    /// under a cloned repo's directory name), so it is wire-derived: the "source
    /// died" line must carry it as a `?` field or the escapes reach the terminal.
    struct HostileSource;

    impl Source for HostileSource {
        fn name(&self) -> &str {
            "hostile-test-source"
        }
        async fn run(self: Box<Self>, _tx: TaggedSender) -> anyhow::Result<()> {
            anyhow::bail!("open /tmp/\u{1b}]0;pwned\u{7}\u{202e}a.jsonl failed")
        }
    }

    #[tokio::test]
    async fn source_died_line_escapes_the_error_chain() {
        let (logs, _guard) = crate::test_capture::capture_warns();
        let (tx, _rx) = tokio::sync::mpsc::channel::<(Transport, AgentEvent)>(8);
        let (deaths_tx, _deaths_rx) = watch::channel(Vec::new());
        for h in SourceManager::new()
            .with_source(Box::new(HostileSource))
            .spawn_with_health(tx, deaths_tx)
        {
            h.await.unwrap();
        }
        let out = logs.contents();
        assert!(out.contains("source died"), "got {out:?}");
        for raw in ['\u{1b}', '\u{7}', '\u{202e}'] {
            assert!(
                !out.contains(raw),
                "{raw:?} must not reach the log raw: {out:?}"
            );
        }
    }

    #[tokio::test]
    async fn spawn_without_health_listener_does_not_panic_on_death() {
        let (tx, _rx) = tokio::sync::mpsc::channel::<(Transport, AgentEvent)>(8);
        for h in SourceManager::new()
            .with_source(Box::new(DyingSource))
            .spawn(tx)
        {
            h.await.unwrap();
        }
    }
}
