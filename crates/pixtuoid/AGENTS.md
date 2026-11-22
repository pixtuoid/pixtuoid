# pixtuoid (binary) — agent guide

The **binary**: it wires sources → reducer → renderer and owns the CLI
subcommands, hook installation, config persistence and multi-floor
orchestration. Module map: `ls src/`; each file's `//!` header annotates it.
Cross-cutting rules: the root [`AGENTS.md`](../../AGENTS.md).

## The painters

- Two thin painters sit on the `pixtuoid-scene` engine
  ([its guide](../pixtuoid-scene/AGENTS.md)): `src/tui/`
  ([its guide](src/tui/AGENTS.md)) and `src/floating/`. Neither depends on
  the other.
- `src/panels/` holds what both show: the modal panels, and the one key
  dispatch and mouse ladder that drive them.
- A change to the panels' widgets or the dispatch precedence adds or updates an
  output-first test in the
  [tui harness](src/tui/AGENTS.md#when-refactoring).

## The corpus census

`just corpus-all` drives every local transcript through the real render seam;
`examples/corpus_check.rs`'s `//!` header says what it reports and why it
doesn't gate.
