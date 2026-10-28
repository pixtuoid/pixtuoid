//! Backend-agnostic render + simulation engine shared by every front-end.
//!
//! It has **no** terminal or window dependency — `tui` (ratatui half-block),
//! `floating` (winit/wgpu) and `pixtuoid-web` (wasm canvas) are thin
//! painters layered on top.
//!
//! ```
//! use pixtuoid_scene::layout::SceneLayout;
//! use pixtuoid_scene::theme::theme_by_name;
//!
//! let theme = theme_by_name("dracula").expect("bundled theme");
//! assert_eq!(theme.name, "dracula");
//!
//! // `None` fills the buffer with as many desk pods as physically fit.
//! let office = SceneLayout::compute_with_seed(192, 64, None, 0)
//!     .expect("viewport is large enough for an office");
//! assert!(!office.home_desks.is_empty());
//! ```

// Terminal- and window-free (invariant #1): the dep boundary can't see a raw
// `println!` (std, no dep); this restriction lint can.
#![cfg_attr(not(test), warn(clippy::print_stdout, clippy::print_stderr))]
// Lives here, not in Cargo.toml: `[lints] workspace = true` cannot be combined
// with a per-crate `[lints.rust]` table.
#![forbid(unsafe_code)]
// Scoped to this PUBLISHED crate, not `[workspace.lints]` — the binary crates'
// `pub` items aren't a semver surface.
#![warn(missing_docs)]
// The leak-direction twin of `unreachable_pub`: a `pub` item reachable through
// a signature but not nameable outside the crate.
#![warn(unnameable_types)]
// AGENTS.md's Errors rule, as a lint: no `unwrap()` outside tests.
#![cfg_attr(not(test), warn(clippy::unwrap_used))]

#[doc(hidden)]
pub mod anim;
pub(crate) mod atmosphere;
#[doc(hidden)]
pub mod audio;
#[doc(hidden)]
pub mod badge;
#[doc(hidden)]
pub mod burn;
pub(crate) mod cached_layer;
pub(crate) mod celestial;
pub(crate) mod character;
pub mod chitchat;
pub(crate) mod clouds;
pub(crate) mod composite;
pub(crate) mod creatures;
#[doc(hidden)]
pub mod hit;
#[doc(hidden)]
pub mod interact;
#[doc(hidden)]
pub use creatures::PET_LONGEST_REST_MS;
#[doc(hidden)]
pub mod cutaway;
#[doc(hidden)]
pub mod display;
pub(crate) mod dither;
pub(crate) mod effects;
pub mod floor;
#[doc(hidden)]
pub mod footer;
#[doc(hidden)]
pub mod frame_cache;
pub(crate) mod glass;
pub(crate) mod glass_weather;
pub(crate) mod ground;
pub mod layout;
pub(crate) mod lighting;
#[doc(hidden)]
pub mod localclock;
pub mod look;
#[doc(hidden)]
pub mod neon_sign;
pub(crate) mod outside;
#[doc(hidden)]
pub mod screen;
#[doc(hidden)]
pub use outside::OutsideCache;
pub mod pack;
#[doc(hidden)]
pub mod par;
pub mod pathfind;
/// Office pets — the `Pet`/`PetKind` model and per-floor selection.
pub mod pet;
pub(crate) mod physics;
pub(crate) mod pixel_painter;
pub mod pose;
pub mod render_scale;
pub mod sim;
pub mod sky;
pub(crate) mod skyline;
#[doc(hidden)]
pub mod tally;
/// The color-theme MODEL: the `Theme` role palette and the bundled themes.
pub mod theme;
pub mod token_meter;
#[doc(hidden)]
pub mod tooltip;
pub mod walk;

/// The repos (cwd basenames) the demo agents work in: pixtuoid-web's hero
/// cast takes one each, in order, and the `snapshot` and `cutaway_snapshot`
/// examples' casts cycle the first [`DEMO_TEAMS`].
#[doc(hidden)]
pub const DEMO_REPOS: &[&str] = &[
    "api", "web", "infra", "data", "cli", "docs", "etl", "tests", "ml", "research", "embedded",
];

/// How many of [`DEMO_REPOS`] the snapshot examples' casts cycle through:
/// fewer than their agents, so agents share a working directory and so an
/// outfit, the grouping Team Palette exists to show.
#[doc(hidden)]
pub const DEMO_TEAMS: usize = 4;
const _: () = assert!(DEMO_TEAMS > 0 && DEMO_TEAMS <= DEMO_REPOS.len());

/// ⌊2⁶⁴/φ⌋, φ the golden ratio: the Fibonacci-hashing multiplier and splitmix64's increment.
pub(crate) const GOLDEN_GAMMA: u64 = 0x9e37_79b9_7f4a_7c15;
pub(crate) const GOLDEN_GAMMA_32: u32 = (GOLDEN_GAMMA >> 32) as u32;
pub(crate) const MURMUR64A_M: u64 = 0xc6a4_a793_5bd1_e995;
pub(crate) const MURMUR3_FMIX32_M1: u32 = 0x85eb_ca6b;

/// Draw `n` of the splitmix64 stream seeded at `seed`, counting from 1: draw 0
/// is the bare finalizer, which maps 0 to 0.
pub(crate) fn splitmix_draw(seed: u64, n: u64) -> u64 {
    pixtuoid_core::id::splitmix64(seed.wrapping_add(n.wrapping_mul(GOLDEN_GAMMA)))
}

/// `hash` spread over `0..len` alike on every target: the modulo runs in
/// `u64`, so a 32-bit `usize` (the wasm build) keeps the hash's high bits. An
/// empty range spreads to 0.
pub(crate) fn spread(hash: u64, len: usize) -> usize {
    u64::try_from(len)
        .ok()
        .and_then(|len| hash.checked_rem(len))
        .and_then(|i| usize::try_from(i).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod spread_tests {
    /// The modulo is the hash's own, high bits included; nothing spreads over an
    /// empty range. A 64-bit host can't see a 32-bit truncation, so the call
    /// sites' `#[deny(clippy::cast_possible_truncation)]` is that guard.
    #[test]
    fn spread_keeps_the_high_bits_and_an_empty_range_is_zero() {
        let hash = (1u64 << 40) | 5;
        assert_eq!(super::spread(hash, 7), usize::try_from(hash % 7).unwrap());
        assert_ne!(super::spread(hash, 7), super::spread(5, 7));
        assert_eq!(super::spread(hash, 0), 0);
    }
}
