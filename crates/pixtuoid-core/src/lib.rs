//! pixtuoid-core: headless logic for the pixtuoid TUI. [`Reducer::apply`] folds
//! each [`AgentEvent`] (tagged with the [`Transport`] it arrived on) into the
//! per-agent slots of a [`SceneState`] that a renderer then paints.
//!
//! ```
//! use std::path::PathBuf;
//! use std::time::SystemTime;
//!
//! use pixtuoid_core::{AgentEvent, AgentId, Reducer, SceneState, Transport};
//!
//! let mut scene = SceneState::uniform(4);
//! let mut reducer = Reducer::new();
//! let id = AgentId::from_parts("claude-code", "session-1");
//!
//! reducer.apply(
//!     &mut scene,
//!     AgentEvent::SessionStart {
//!         agent_id: id,
//!         source: "claude-code".into(),
//!         session_id: "session-1".into(),
//!         cwd: PathBuf::from("/repo"),
//!         parent_id: None,
//!     },
//!     SystemTime::now(),
//!     Transport::Hook,
//! );
//!
//! // The desk label is `<source-prefix>·<cwd-basename>`.
//! let slot = scene.agents.get(&id).expect("the session took a desk");
//! assert_eq!(&*slot.label, "cc·repo");
//! ```

// Invariant #1: this crate is headless. `just arch` allowlists the dep tree,
// but a raw `println!` pulls no dep and slips past it.
#![cfg_attr(not(test), warn(clippy::print_stdout, clippy::print_stderr))]
// Scoped here rather than `[workspace.lints]` because only this PUBLISHED
// crate's `pub` items are a semver surface.
#![warn(missing_docs)]
// The leak-direction twin of `unreachable_pub`: a `pub` item reachable through
// a signature but not nameable outside the crate.
#![warn(unnameable_types)]
// AGENTS.md's Errors rule, as a lint: no `unwrap()` outside tests.
#![cfg_attr(not(test), warn(clippy::unwrap_used))]

pub mod grid;
/// The offline decode→reduce driver every test/tool that feeds real wire bytes
/// through the production path rides.
#[cfg(feature = "harness")]
pub mod harness;
/// Agent identity: the `AgentId` session key and its path/parts derivations.
pub mod id;
pub mod platform;
/// The source/decoder seam — the `Source` trait, per-CLI transcript/hook
/// decoders, listeners, and the `SourceManager`.
pub mod source;
/// Sprite vocabulary: `Frame`/`Sprite`/`Palette`, the `RgbBuffer` blit target,
/// and the `.sprite`/`pack.toml` pack loader.
pub mod sprite;
/// The reducer and `SceneState` — the event coordinator turning `AgentEvent`s
/// into per-agent slot state.
pub mod state;
#[cfg(windows)]
pub(crate) mod win32;
// `WalkableMask` is an ALIAS for `Grid<bool>` whose obstacle ops are an
// inherent `impl Grid<bool>`, and the orphan rule pins that impl to the crate
// owning `Grid` — so the mask vocabulary stays here even though its producers
// and consumers live in `pixtuoid-scene`.
pub mod walkable;

pub use grid::Grid;
pub use id::AgentId;
pub use source::{AgentEvent, ToolDetail, Transport};
#[cfg(feature = "native")]
pub use source::{Source, TaggedReceiver, TaggedSender};
pub use sprite::{Frame, Palette, Pixel, Rgb, RgbBuffer, Sprite, WriteEpoch};
pub use state::reducer::Reducer;
pub use state::{
    ActivityState, AgentSlot, FloorLocalDeskIndex, GlobalDeskIndex, SceneState, SlotLabel, ToolKind,
};
pub use walkable::{OccupancyOverlay, WalkableMask};

#[cfg(test)]
pub(crate) mod test_capture;
#[cfg(test)]
pub(crate) mod test_child;
