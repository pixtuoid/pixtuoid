//! The `native`-only async transport seam: the `Transport`-tagged tokio
//! channel aliases + the `Source`/`DynSource` traits. A `--no-default-features`
//! (wasm) build has no tokio, so the whole module sits behind ONE
//! `#[cfg(feature = "native")]` gate in `source/mod.rs`.

use tokio::sync::mpsc;

use super::{AgentEvent, Transport};

/// Events sent on a tagged channel so the reducer knows which transport produced them.
pub type TaggedSender = mpsc::Sender<(Transport, AgentEvent)>;
/// Receiving half of the `Transport`-tagged event channel (see `TaggedSender`).
pub type TaggedReceiver = mpsc::Receiver<(Transport, AgentEvent)>;

/// A `Source` produces `AgentEvent`s from one agent CLI flavor and sends them
/// on a `Transport`-tagged channel.
///
/// ## Implementor contract
///
/// 1. **`name()`** — a stable, lowercase identifier (`"claude-code"`), used as
///    the `AgentSlot.source` field and as the first argument to
///    [`AgentId::from_parts`].
/// 2. **`AgentId` derivation** — every `AgentEvent::SessionStart` MUST carry an
///    `agent_id` built via [`AgentId::from_parts(self.name(),
///    opaque_id)`][`AgentId::from_parts`]; any other construction risks
///    cross-source collisions.
/// 3. **Transport tagging** — the reducer reads the [`Transport`] tag (its
///    doc says for what); the wrong tag silently breaks it.
/// 4. **Never panic** — sources run inside a tokio task that doesn't propagate
///    panics cleanly. Log + continue on malformed input.
///
/// [`AgentId::from_parts`]: crate::AgentId::from_parts
pub trait Source: Send + 'static {
    /// Stable, lowercase identifier for this source (e.g. `"claude-code"`).
    fn name(&self) -> &str;
    /// Consume the source and pump its `AgentEvent`s onto `tx` until the stream
    /// ends or a fatal error.
    fn run(
        self: Box<Self>,
        tx: TaggedSender,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send;
}

/// Object-safety twin of [`Source`] — the type `SourceManager` actually boxes
/// (`Box<dyn DynSource>`). It exists ONLY because [`Source`]'s `-> impl Future
/// + Send` return (RPITIT) is not dyn-compatible, so `dyn Source` cannot exist;
/// don't merge the two traits. Source authors never name this trait: the blanket
/// impl below + unsize coercion let `with_source(Box::new(my_source))` work directly.
pub trait DynSource: Send + 'static {
    /// This source's stable identifier — mirrors `Source::name`.
    fn name(&self) -> &str;
    /// Boxed-future form of `Source::run` — the dyn-safe erasure point `SourceManager` awaits.
    fn run(
        self: Box<Self>,
        tx: TaggedSender,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send>>;
}

/// The bridge: every [`Source`] is a [`DynSource`] whose future is boxed at
/// the erasure boundary. The inner `self.name()`/`self.run(tx)` calls resolve
/// to `<T as Source>` (the where-clause candidate), not recursively here.
impl<T: Source> DynSource for T {
    fn name(&self) -> &str {
        self.name()
    }

    fn run(
        self: Box<Self>,
        tx: TaggedSender,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send>> {
        Box::pin(self.run(tx))
    }
}
