//! The enriched orthographic cutaway profile — a SIBLING of the classic
//! half-block painter, not a fidelity knob on it. Both read
//! `sim::SimFrame`.
//!
//! `render_cutaway`, `CutawayCanvas` and the screen-text sink (`paint_grid`,
//! its cells and `draws`) are `#[doc(hidden)] pub` as seams for out-of-crate drivers
//! (the snapshot example, the render bench, the binary's font-coverage test,
//! the painters) — MECHANISM, not a promise to a crates.io consumer. `shade` has no
//! cross-crate caller and stays `pub(crate)`: a `pub` item on a published crate
//! is the one thing a follow-up cannot quietly undo.
#[doc(hidden)]
pub mod canvas;
pub(crate) mod effects;
mod grid;
pub(crate) mod light;
#[doc(hidden)]
pub mod paint;
pub(crate) mod pen;
pub(crate) mod shade;
pub(crate) mod text;
#[doc(hidden)]
pub use crate::display::cells::CellPx;
pub use grid::{Canvas, GridInk, chrome_cell, draws, glyph_cell, office_text_draws, paint_grid};
pub(crate) mod wall;
