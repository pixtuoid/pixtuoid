# pixtuoid/tui — terminal renderer agent guide

The **terminal painter**: `TuiRenderer`, whose inherent `render` flushes each
frame. It owns the half-block flush, mouse hit-testing, and the crossterm
event loop and terminal lifecycle. The panels it paints, their UI models and the key dispatch
are `crate::panels`, which the window shares. Every N/S claim here rests on
the scene guide's
[screen-space compass](../../../pixtuoid-scene/AGENTS.md#screen-space-compass).
Module map: `ls` this directory; each file's `//!` header annotates it.
Cross-cutting rules: the workspace [`AGENTS.md`](../../../../AGENTS.md).

## When refactoring

- A change to `flush_classic` adds or updates a harness test:
  `tui_renderer/harness` drives the real `TuiRenderer` through a ratatui
  `TestBackend`, output-first.
