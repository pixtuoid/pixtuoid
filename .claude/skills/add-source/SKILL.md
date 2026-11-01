---
name: add-source
description: "Wires a new agent CLI into pixtuoid so its sessions become office sprites, from the upstream facts through a reviewed PR. Use when adding support for a CLI, adding a source for a tool, or integrating a coding agent, in a maintainer's session or a contributor's fork. Covers the cross-crate checklist, including what a diff-scoped edit silently misses: the site manifest, the per-source badge hue, the home-dir fn and the integration goldens a `--lib` run never builds."
---

# add-source

Copy this list into your plan and work it in order, fixing and rerunning each
check until it passes.

- [ ] **Upstream facts.** Answer [the checklist](../../../docs/CONTRIBUTING.md#adding-a-new-agent-cli)'s
      step 1 before any code, from the latest release's source. Its questions have
      no test, so a wrong answer ships green.
- [ ] **Steps 2–11** of that checklist, with the test steps in
      [`tests/AGENTS.md`](../../../crates/pixtuoid-core/tests/AGENTS.md).
- [ ] **`just test`, never `--lib` alone.** The `sources --json` golden (`cli_json`)
      and the `wire_to_pixels` matrix are integration binaries a `--lib` run skips:
      kimi's PR (#692) passed `--lib` and a multi-lens review with both red.
- [ ] **`just gen-contract`**, only if the `--json`/`SourceStatus`/`OutcomeRow` shape
      changed; adding a row doesn't change it.
- [ ] **`just hermetic <source>`**, the [must-run](../../../REVIEW.md#must-run). A new
      source runs against the real CLI, never a capture rig's flags.
- [ ] **[`local-review`](../local-review/SKILL.md)**, always: a fork's PR gets no CI
      review, so this is its only one. Fold what it finds into one commit.
- [ ] **The PR body** states the change's claims, step 1's answers with their cites,
      the review's counts and each must-run result. CI runs the tests.
