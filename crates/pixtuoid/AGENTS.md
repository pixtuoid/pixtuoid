# pixtuoid (binary) — agent guide

The **TUI binary**: wires sources → reducer → renderer; owns the CLI
subcommands, hook installation, config persistence, and multi-floor
orchestration. Its two thin painters over the `pixtuoid-scene` engine
([`../pixtuoid-scene/AGENTS.md`](../pixtuoid-scene/AGENTS.md)) are `src/tui/`
([`src/tui/AGENTS.md`](src/tui/AGENTS.md)) and `floating/` — neither depends
on the other; the modal panels both show, and the one key dispatch that drives
them, are `src/panels/`. Module map: `ls src/` — each file's `//!` header is its
annotation. Cross-cutting rules: workspace [`AGENTS.md`](../../AGENTS.md).

## The corpus census

`just corpus-all` drives every local transcript through the real render seam;
what it reports and why it doesn't gate: `examples/corpus_check.rs`'s `//!` header.
