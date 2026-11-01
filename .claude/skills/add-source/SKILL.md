---
name: add-source
version: 1.3.0
description: "Wire a new agent-CLI Source adapter into pixtuoid (a new coding CLI whose sessions become office sprites). Use when the user says 'add support for <CLI>', 'add a source for <tool>', or 'integrate <agent CLI>'. Orchestrates the cross-crate checklist whose steps have TEST TEETH — the ones a diff-scoped edit silently misses (site manifest bridge, per-source badge hue, home-dir fn, plus the two `pixtuoid/tests/*` integration goldens that `-p <crate> --lib` never builds)."
metadata:
  scope: "pixtuoid repo only"
---

# add-source

Adding an agent CLI spans `pixtuoid-core` (decoder + registry + tests), the
`pixtuoid` binary (runtime wiring + install target + badge hue), and the site
manifest.

## When to use

- "Add support for <CLI>" / "integrate <agent tool>" / "add a source for X".
- A new transcript-bearing OR hook-only coding CLI should show up as sprites.

## The checklist

Follow **[`docs/CONTRIBUTING.md`](../../../docs/CONTRIBUTING.md#adding-a-new-agent-cli)**
"Adding a new agent CLI" step by step; its test steps are in
[`crates/pixtuoid-core/tests/AGENTS.md`](../../../crates/pixtuoid-core/tests/AGENTS.md).
Step 1's questions come before any code: their tests don't exist, so a wrong
answer ships green.

## The trap: `--lib` is not the suite

The `sources --json` golden (`cli_json`) and the `wire_to_pixels` matrix live in
`pixtuoid/tests/*.rs` INTEGRATION BINARIES that `-p <crate> --lib` never builds —
the #692 (kimi) miss: a green `--lib` run AND a multi-lens review both passed
while these two were red, caught only by the full suite. Run
`just test` (NOT just `--lib`) before declaring green.

## Finish

- `just gen-contract` only if you touched the `--json`/`SourceStatus`/`OutcomeRow`
  SHAPE (adding a row doesn't).
- `just hermetic <source>` for the [must-run](../../../REVIEW.md#must-run).
- **local-review**, always: a fork's PR gets no CI review, so this is its only
  one. Fold what it finds into one commit, and give the PR body its counts, step
  1's answers and each must-run result. CI runs the tests.
