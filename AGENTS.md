# AGENTS.md

Instructions for any AI coding agent in this repo. Every agent reads these
`AGENTS.md` files natively, Claude Code from v2.1.277
([code.claude.com/docs/en/memory](https://code.claude.com/docs/en/memory#agents-md)).
This file is the workspace map; per-crate detail lives in the nested guides:

- [`crates/pixtuoid-core/AGENTS.md`](crates/pixtuoid-core/AGENTS.md) — headless lib: sources/decoders, reducer/state.
  - [`tests/AGENTS.md`](crates/pixtuoid-core/tests/AGENTS.md) — the test-binary layout and the add-a-CLI test steps.
- [`crates/pixtuoid-scene/AGENTS.md`](crates/pixtuoid-scene/AGENTS.md) — backend-agnostic render+sim engine: sprite format, grid/walkable, pixel painter, layout, walk/pose/pathfind, theme model, weather, pets, chitchat.
- [`crates/pixtuoid/AGENTS.md`](crates/pixtuoid/AGENTS.md) — the binary: install, runtime, cli, config, multi-floor.
  - [`src/tui/AGENTS.md`](crates/pixtuoid/src/tui/AGENTS.md) — the terminal painter.
- [`integrations/AGENTS.md`](integrations/AGENTS.md), [`site/AGENTS.md`](site/AGENTS.md) — the non-Rust `--json` consumers; their gates are `tsc`/`eslint` / `just site-check`, not cargo.

**What the guides hold:**

- A nested `AGENTS.md` says what its crate is, plus the facts no single
  declaration can own: cross-file workflows, comment-less manifests and
  directory-shape rules.
- Everything else a change needs (the constraint that looks like a bug, the
  why, the test that pins it) sits on the narrowest thing it constrains. Before
  changing an item, read all of it, its doc comment, and the comments on the
  lines it governs.

**A third consumer lives outside this repo.** homebrew-core's `pixtuoid`
formula asserts exact CLI output in its `test do`, and needs `pixtuoid man` and
`completions <shell>` on clean stdout. The code behind each asserted row carries
a "homebrew-core contract" comment. How CI holds this tree to the formula, and
what a release owes it: [CONTRIBUTING's Homebrew](docs/CONTRIBUTING.md#homebrew).

<!-- org-rules:begin -->
## Org rules

Shared by every pixtuoid repo. The source is pixtuoid/.github's `.github/actions/org-rules/org-rules.md`, and that action fails CI when a repo's copy differs: change the rules there, then sync each repo's copy.

The owner's bar: every change is best practice, the best architecture design and the most future-proof, follows the industry's standard practice and these md rules, and refactors existing code that breaks them; no low-ROI work, no over-engineering, no legacy or backward-compatibility paths.

- **Evidence.** An external claim, "best practice" included, cites a doc URL or `path:line` fetched this session, or is labelled opinion.
- **Refactor.** Existing code a change touches that breaks a written rule is refactored in that change. Any other code may be, in a commit of its own, when that removes a named defect, never on taste alone.
- **Every mechanism earns its upkeep.** Code, config, a test, a CI job or a doc stays only if it names a failure it prevents that nothing else already does. A maintained library, the platform or the upstream tool's own mechanism is reused before one is built.
- **One source.** What every repo shares lives once, in pixtuoid/.github or, for CodeRabbit, pixtuoid/coderabbit, and is referenced, never copied, but for these rules in each agent guide and the files a repo holds to reference the rest, whose drift the org-rules action fails CI on. The org's agent skills are pixtuoid/.github's `pixtuoid-dev` Claude Code plugin, which project settings enable but each clone installs once (code.claude.com/docs/en/plugins/loading, "Enabled in project settings but not installed"): `claude plugin install pixtuoid-dev --marketplace pixtuoid/.github --scope project`, which also declares the marketplace in the user's settings (code.claude.com/docs/en/discover-plugins, "Add and install from your shell", Claude Code v2.1.292 or later).
- **TDD.** A failing test, then the minimal implementation, then the commit.
- **Comments are WHY only**, doc prose included, and sit on the narrowest thing they constrain; a PR body says what the change does and why (the problem, why this approach, its shortcomings) as claims each checked against the diff, never how it got there, and is re-read against the head before it queues. Name the authority instead of restating its value. Measurements go in the PR body.
- **No magic numbers.** Reuse the existing authority, else one named constant at the narrowest scope; when a copy of a value must cross a boundary, a test pins the pair. No `unwrap()` outside tests.
- **Review findings.** A finding's premise is checked against the item it names before it is accepted. A refuted finding produces a mechanism (a test, a compile-time constraint, a CI gate) or nothing. A fix lengthens prose only when its disposition names a reader who would act wrongly without the added words.
- **Local review.** A session runs `/pixtuoid-dev:local-review` on its own work before a PR from the repo's own branches opens for review, and before a push to it, once open for review, carrying a change that neither answers a review thread, the owner's included, nor merges the base branch. The PR body names the head that review ended on, its last fold included, and such a change after that head is a finding; the rest is left to the [CI review](https://github.com/pixtuoid/.github/blob/main/.github/workflows/claude-review.yml).
- **Docs** change in the same commit as what they describe, and no new doc lands unless the owner asks.
- **Git.** Never force-push and never skip hooks. A change reaches `main` only through a pull request and the org's Mergify queue, a release PR aside, which merges by hand. The session that opened a PR queues it (`@mergifyio queue`) once every finding has its disposition and the repo's must-run results are in the body; whoever queues a bot's PR reads it first.
- **The [Roadmap](https://github.com/orgs/pixtuoid/projects/1) is the plan of record**, the owner's rule: every step (an item started or finished, a PR opened, merged or closed, a reorder) updates its item, the draft or the issue it tracks, in the same turn. Agents add Roadmap draft items, never issues.
<!-- org-rules:end -->

## What this is

Terminal-native, multi-agent pixel-art visualizer for AI coding agents: each
running agent session is an animated sprite in a pixel office. The tagline is
`Cargo.toml`'s `description`; the overview is [`README.md`](README.md).

## Layout

```
crates/   DAG: pixtuoid-core ← pixtuoid-scene ← {pixtuoid, pixtuoid-web, pixtuoid-web-music}  (+ standalone pixtuoid-hook)
├── pixtuoid-core/   headless lib — no terminal deps; `native` feature gates the async
│                    source runtime (off = wasm32-clean decode/reduce)
├── pixtuoid-scene/  render+sim engine — terminal- and window-free by crate boundary
│                    (its music: the external `pixtuoid-lofi`, https://github.com/pixtuoid/lofi)
├── pixtuoid/        binary — two thin painters over pixtuoid-scene: `tui/`, `floating/`
├── pixtuoid-web/    third painter — wasm canvas, publish=false; a site build input
│                    (`just gen-wasm` → gitignored site/public/wasm/, built in CI)
├── pixtuoid-web-music/ the site's ♩ lofi in its own wasm, so the office's carries none
└── pixtuoid-hook/   tiny shim an agent CLI's hooks invoke — stdin JSON → socket/named pipe
scripts/  gen-media.py: the one driver for the README's committed media and the site's CI-rendered demos
          gen-art.py: the generated sprites, every @Nx plus the 1x pieces it owns
          each shipped plugin's selftest; in lib/, its harness and the openclaw tier
policy/   CI contracts no linter sees (jq over yq) + behavior tests of workflow shell
fuzz/     cargo-fuzz targets over harness::Drive and remote, its own workspace; `.clusterfuzzlite/` builds them for fuzz.yml
site/     Astro landing page; integrations/raycast/  Raycast extension
```

## Build & test

```
just build [--release] · just test                   # the one test runner (nextest)
just test -p <crate> <filter>                        # fast loop
just preflight [full]                                # pre-push gate; `full` adds some of CI's Rust recipes (`just --list` shows which)
cargo run --release --example snapshot -- /tmp/snap.png   # render TUI to PNG
cargo run --profile release-local --example snapshot -- /tmp/snap.png   # the same, rebuilt faster; scripts still read target/release
```

- Once per clone, run `just setup-tools`. It points git at `.githooks`, then
  installs `mise.toml`'s tools and rust-analyzer, without which the agent LSP
  degrades to grep.
- Iterate with clippy or test, not both: clippy doesn't warm test's build
  ([why](docs/CONTRIBUTING.md#build--test)).
- Never pipe preflight through `tail`/`head`: the pipe eats its exit code.
- After touching `--json`, `SourceStatus` or `OutcomeRow`, run
  `just gen-contract`. It regenerates the schemas and the Raycast types.
- In a PR that changes the look, run `just gen` and commit everything it
  rewrote. Media is not the PR's to commit: the site's demos render in CI, and
  the README's `docs/images` in `media-regen.yml` from main.
- Real wire bytes ride one pipeline, `pixtuoid_core::harness::Drive`:
  [its rules](crates/pixtuoid-core/tests/AGENTS.md#the-one-pipeline).
- Fixtures are recorded, never composed:
  [the rules](crates/pixtuoid-core/tests/sources/README.md).
- For sprite work, verify visually: the snapshot example, then
  `scripts/crop-snapshot.py`, then read the PNG. The loop is in
  `.claude/skills/beautify-decoration/SKILL.md`.
- A green preflight is not a green PR; CI is the gate. A PR's pushes run only
  the light tier. The full tier runs on `main` and in the merge queue, which
  [the merge gate](docs/CONTRIBUTING.md#the-merge-gate) details.
- Advisory, never gating: `just mutants`, `just bench`, `just bench-pacing`,
  CodSpeed, and `fuzz.yml`. A fuzz crash's input is its run's
  `crashes-<target>` artifact, and `just fuzz <target> <file>` replays it.
- The fluency gate, `just pace-check`, runs locally and before a release, never
  in CI, since it has a wall-clock budget. Its runs and verdict are in
  `scripts/pace-check.py`'s header.
- Release is a human step: [`CONTRIBUTING.md#releasing`](docs/CONTRIBUTING.md#releasing).

## Workflow

- Non-trivial work runs as [an arc](docs/CONTRIBUTING.md#the-arc-loop). Read
  each step there before taking it, never from recall.
- The CI review applies REVIEW.md. The org's local-review skill runs it first,
  when [the org rules](#org-rules)' Local review says. An agent without the
  plugin reads the skill's
  [source](https://github.com/pixtuoid/.github/blob/main/plugins/pixtuoid-dev/skills/local-review/SKILL.md).
- Every PR, an arc's or not, queues as
  [the merge gate](docs/CONTRIBUTING.md#the-merge-gate) says.
- A session pushes its own PR branches freely.
- **The open-PR cap: at most 6 open PRs across the sessions**, bots' and
  outside contributors' aside. It is the owner's cap: the runners take
  [20 jobs at once](https://docs.github.com/en/actions/reference/limits), and
  one light-tier push needs more, so each open PR past the first lengthens
  every push's wait.
  - Until a slot frees, the branch stays pushed with no PR.
- Open a PR once its branch is ready to merge and its local review is done,
  when one is due. Never open it as a draft.
- Don't rerun preflight or CI by hand: the pre-push hook runs preflight, and CI
  runs the rest.
- Close a PR that stops being ready. Keep its branch, and never force-push it,
  since a force-pushed branch can't reopen.
  - Retarget a stacked PR to `main` before closing it: merging its base
    deletes the branch it targets.
  - Then [force-cancel](https://docs.github.com/en/rest/actions/workflow-runs#force-cancel-a-workflow-run)
    its runs. Closing leaves them queued, and a plain cancel still runs the
    `always()` gate jobs.

Repo skills (committed): `beautify-decoration`, `add-source`.

## Conventions

- **Mechanism upkeep and reuse** (org rule):
  - Extending a mechanism re-adds it. Delete a mechanism you touch that can't
    name its failure; don't polish it.
  - When deleting it would remove a user-facing feature, propose the deletion
    to the owner instead; never delete it outright.
  - The load-bearing defenses that [REVIEW.md](REVIEW.md#design)'s Proportion
    row lists have named their failures.
  - Skip a reuse only when its own cost is larger (wasm32, a version freeze),
    and name that cost in the PR.
- **Chibi figures and pets stay round**, the head flowing into the body.
- **Group related parameters into a struct first**;
  `#[allow(clippy::too_many_arguments)]` is a last resort.
- **Comments** (org rule):
  - The rule also covers a workflow or manifest comment, a justfile recipe
    header and a CI contract's `why`.
  - Every sentence adds what the earlier ones don't, and the first is the
    whole answer.
  - A rationale stays on the statement, struct-literal field or match arm it
    governs. Never hoist it to the declaration to shorten a body.
  - A change leaves the net comment volume of what it touches no larger,
    unless each added line earns its place.
  - An intra-doc link to the authority also turns a rename into a `doc-check`
    red, private items included.
- **DRY.** One implementation per piece of logic and one source per value: two
  copies are a latent drift bug.
- **Magic numbers** (org rule): prefer a type (enum/newtype) for a related set.
  Self-evident `0`/`1`/`2`, indices and test fixtures stay inline.
- **Errors**: `anyhow::Result` in app code, `thiserror` in core.
- **The hook listener and the JSONL watcher log and continue**; they never
  panic.
- **Visibility**: keep a layer-internal item `pub(crate)`.
  - `unreachable_pub` misses a `pub` reachable through a `pub mod`. `just hawk`,
    a full-tier gate, fails on one that no other workspace crate's code needs.
  - `#[doc(hidden)] pub` is an escape hatch for a mechanism, not a contract. No
    public signature names one (`just api-surface` fails on it), and nothing an
    out-of-workspace painter reads is one: the iOS App, which builds against
    the published crates.
  - The binary's lib has one `pub mod`: the `#[doc(hidden)]` `pixtuoid::dev`
    facade its examples, tests and benches reach it through. So
    `unreachable_pub` reports any `pub` the facade's list doesn't name.
- **Keep state updated as events arrive**; never derive it by scanning the
  history backward.
- **Shell**: match the surrounding shell; macOS-first (BSD CLI, brew).
- **Evidence** (org rule):
  - A hermetic profile scenario that reaches the surface (`just hermetic`)
    counts as a source too.
  - The population is the whole upstream repo
    (`gh api .../git/trees/<ref>?recursive=1`), not one plausible file.
- **Review findings** (org rule):
  - A refutation adds a code comment only for an external fact (another CLI's
    wire bytes, an OS semantic), on the narrowest thing it constrains.
  - Every finding reaches a terminal state before the PR merges.
    [CONTRIBUTING's Dispositions](docs/CONTRIBUTING.md#dispositions) defines
    the states and what #N may be.
- **Only the latest released version of each agent CLI is supported.**
  - When upstream renames or reshapes a wire name, repoint the decoder and the
    plugin at the current declaration and delete the old one.
  - No dual-listening beside a replacement, and no legacy-format arm kept "just
    in case": every superseded arm is a second copy of a wire contract, and it
    drifts silently.
  - This rule doesn't govern our own upgrade path: an arm for an artifact an
    older pixtuoid wrote is ours to keep or drop, not upstream's (#457).
  - Nor does it govern mirroring a resolver that upstream itself still
    branches on.
- **Path asserts compare `PathBuf` structurally**, never `to_string_lossy()`
  with a hardcoded separator: string asserts pass on Unix and fail only in
  `windows-test`.
- **Resolution policy is per CLI** (HOME vs USERPROFILE, %APPDATA% vs
  `~/.config`): mirror each CLI's own resolver. A runtime's mirror lives in
  `platform`; any other lives beside its target.

## Architecture invariants (load-bearing)

1. **`pixtuoid-core` and `pixtuoid-scene` have no terminal/window/audio-device deps.** The crate boundary makes the compiler enforce it, and `just arch` holds their dep set to the allowlist in `deny-arch.toml`, so a new crate fails until it is listed. New render targets are thin painters over `pixtuoid_scene::look::render`.
2. **Agent events flow through one channel**, `mpsc::Sender<(Transport, AgentEvent)>`. The `Transport` tag drives hook-wins dedup, and producers tag their own events. Daemon presence rides a separate `AgentId`-free channel (`PresenceMsg { key: DaemonInstanceKey, delta }`) and never enters `Reducer::apply`.
3. **The `Source` trait is the only seam** for a transcript-bearing CLI; per-source format knowledge lives in that source's decoder. The exceptions are hook-only CLIs (the registry rows with `transcript: None`) and the shared ACP wire standard (`source/acp.rs`, reused by grok): see [`CONTRIBUTING.md`](docs/CONTRIBUTING.md#adding-a-new-agent-cli) step 3 and `source/acp.rs`'s header.
4. **Hook install writes through symlinks** (`resolve_symlink` in `install/io.rs`), which stow-managed configs depend on. Windows keeps the bounded rename-retry.
5. **The hook shim never blocks the agent CLI**: it always exits 0, silently. The send bound (pixtuoid-hook's `transport::WRITE_TIMEOUT`) is watchdog-enforced on both platforms. Shim coverage is child-process level only.
6. **The walkable mask is the ground footprint only.** Sprite size never moves a sim position: the sprite fit (`keep_sprite_inside`) adjusts only a placement's paint top-left (#912).

## Ownership by crate

- **core**: session lifecycle, identity and seating (registration, dedup,
  first-sight, the liveness ladder, subagent parenting, feature boundaries).
- **scene**: look and motion (per-agent palette recolor, walk timing,
  footprints, sky and light invariants, reachability).
- **binary**: install and runtime wiring (config rewriting, publishing desk
  capacities, boot order, doctor, daemon announce-only).
- **tui**: the flush (popup geometry, the office's hit-test ladder).
- **panels**: what both painters' panels share (their key dispatch and mouse
  ladder).

Terminal cell aspect drives sprite design. The half-block ▀ technique assumes
~1:2 cells, so sprites past ~16×16 px break on taller-cell terminals. Bundled
base character sprites max at 8×12 px; their `@Nx` variants are cutaway art the
half-block painter never draws.

## Things not to do

- **No write to another CLI's config outside `install/io.rs`**
  (`write_config_atomic`, `ConfigLock`).
- **No `println!`/`eprintln!` on production paths**: a print macro panics when
  its reader leaves (`| head`).
  - CLI and headless output goes through a `CliOut`; a command's is
    `crate::cli_stdout()`.
  - A stderr notice is a `let _ = writeln!`.
- **No value baked into a diagnostic's format string.** Diagnostics are
  `tracing` events with a constant message and every value as a field
  (`error = %e`, `path = ?path`, `pid`): the constant message is a stable grep
  key, and a field is filterable.
  - Every path, session id and cwd rides a `?` field, never `%`, whatever its
    provenance. So does an error chain that can embed one (`SourceDeath.error`,
    `SocketBusy`).
  - Why: `str`'s `Debug` escapes control and bidi chars. Under the default
    `fmt` layer a Display field writes them raw, and `EscapeGuard` covers only
    the message and error text (`test_capture`'s
    `debug_fields_escape_control_and_bidi_chars`).
  - Core logs under whatever subscriber hosts it, so this rule is the primary
    defense. The binary's `EscapedFields` (`app/logging.rs`) escapes every
    value only as a backstop.
  - Only a whole-message relay re-emitting an already-sanitized user-facing
    string interpolates (`config::warn_user`, the `read_log` warning in
    `sources_cli`).
- **No doc generator or cap in place of a proposed diff.** The owner reviews
  every doc change, so propose the diff.
- **No stale `Closes #N` after a re-scope.** The keyword fires from a commit
  body or the PR text
  ([docs.github.com](https://docs.github.com/en/issues/tracking-your-work-with-issues/using-issues/linking-a-pull-request-to-an-issue#linking-a-pull-request-to-an-issue-using-a-keyword)),
  even inside a conditional sentence.

## Where to look

- Tool call → sprite: `runtime/driver.rs::run_async` → `SourceManager::spawn` → source → decoder → `reducer::Reducer::apply` → watch channel → `TuiRenderer::render` → `pixtuoid_scene::floor::OfficeSession::render` → `pixtuoid_scene::look::render` → `tui::renderer::flush_classic`, or the cutaway's tiles via `tui::cutaway::TileCutaway`.
- Architecture + data flow: [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).
- Refactoring the channel type, `Source` trait, `AgentEvent`, or reducer signature touches all three test areas (`tests/reducer/`, `tests/transport/socket.rs`, `tests/watcher/`) + `runtime/driver.rs`; a new `AgentEvent` variant needs an `agent_id()` arm.
- Adding an agent CLI: [`CONTRIBUTING.md`](docs/CONTRIBUTING.md#adding-a-new-agent-cli)'s checklist, which the `add-source` skill drives.
