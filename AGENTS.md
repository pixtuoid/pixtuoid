# AGENTS.md

Instructions for any AI coding agent in this repo. Every agent, Claude Code
included (v2.1.277 or later), reads these `AGENTS.md` files natively; there is
deliberately no `CLAUDE.md`, `.claude/CLAUDE.md` or `CLAUDE.local.md`, because
any of them switches Claude Code to CLAUDE.md-only. This is the workspace map;
per-crate detail lives in the nested guides:

- [`crates/pixtuoid-core/AGENTS.md`](crates/pixtuoid-core/AGENTS.md) — headless lib: sources/decoders, reducer/state, sprites, grid/walkable. (+ [`tests/AGENTS.md`](crates/pixtuoid-core/tests/AGENTS.md): the test-binary layout, add-a-CLI test steps.)
- [`crates/pixtuoid-scene/AGENTS.md`](crates/pixtuoid-scene/AGENTS.md) — backend-agnostic render+sim engine: pixel painter, layout, walk/pose/pathfind, theme model, weather, pets, chitchat.
- [`crates/pixtuoid/AGENTS.md`](crates/pixtuoid/AGENTS.md) — the binary: install, runtime, cli, config, multi-floor. (+ [`src/tui/AGENTS.md`](crates/pixtuoid/src/tui/AGENTS.md): the terminal painter.)
- [`integrations/raycast/AGENTS.md`](integrations/raycast/AGENTS.md), [`site/AGENTS.md`](site/AGENTS.md) — the non-Rust `--json` consumers; their gates are `tsc`/`eslint` / `just site-check`, not cargo.

**What the guides hold.** A nested `AGENTS.md` says what its crate IS, plus the
facts no single declaration can own: cross-file workflows, comment-less
manifests, and directory-shape rules. Everything else a change needs —
the constraint that looks like a bug, the WHY, the test that pins it — is on
the narrowest thing it constrains: read the whole item, its doc comment and the
comments on the lines it governs, before changing it.

**A third consumer lives outside this repo**: homebrew-core's `pixtuoid`
formula asserts exact CLI output (`test do`) and needs `pixtuoid man` /
`completions <shell>` on clean stdout — breakage surfaces in THEIR CI on an
autobump we never see, while our suite stays green (it asserts the same
strings as goldens). The asserted rows carry a "homebrew-core contract"
comment; release-side consequences
in [`CONTRIBUTING.md`](docs/CONTRIBUTING.md#releasing).

## What this is

Terminal-native, multi-agent pixel-art visualizer for AI coding agents: each
running CC session is an animated sprite in an ASCII office. Rust workspace,
five crates. Overview: [`README.md`](README.md).

## Layout

```
crates/   DAG: pixtuoid-core ← pixtuoid-scene ← {pixtuoid, pixtuoid-web}  (+ standalone pixtuoid-hook)
├── pixtuoid-core/   headless lib — no terminal deps; `native` feature gates the async
│                    source runtime (off = wasm32-clean decode/reduce)
├── pixtuoid-scene/  render+sim engine — terminal- AND window-free BY CRATE BOUNDARY
│                    (its music: the external `pixtuoid-lofi`, https://github.com/pixtuoid/lofi)
├── pixtuoid/        binary — two thin painters over pixtuoid-scene: `tui/`, `floating/`
├── pixtuoid-web/    third painter — wasm canvas, publish=false; a SITE BUILD INPUT
│                    (`just gen-wasm` → gitignored site/public/wasm/, built in CI)
└── pixtuoid-hook/   tiny shim CC invokes — stdin JSON → socket/named pipe
scripts/  gen-media.py (the ONE driver for the README's committed media and the site's CI-rendered demos), gen-art.py (the generated sprites: every @Nx + the 1x pieces it owns), e2e tiers (lib/)
policy/   CI contracts no linter sees (jq over yq) + behavior tests of workflow shell
site/     Astro landing page; integrations/raycast/  Raycast extension
```

## Build & test

```
just build [--release] · just test                   # the one test runner (nextest)
just test -p <crate> <filter>                        # fast loop
just preflight [full]                                # pre-push gate: lint → clippy; `full` adds hack → test (CI's Rust recipes)
cargo run --release --example snapshot -- /tmp/snap.png   # render TUI to PNG
cargo run --profile release-local --example snapshot -- /tmp/snap.png   # the same, rebuilt faster; scripts still read target/release
```

- clippy doesn't warm test's build ([why](docs/CONTRIBUTING.md#build--test)) — iterate with one of them. Never pipe preflight through `tail`/`head` (exit code eaten).
- Touched `--json` / `SourceStatus` / `OutcomeRow` / the source roster → `just gen-contract` (regenerates schemas + Raycast types).
- Look-changing PR → `just gen`, commit everything it rewrote. Media is not a PR's: the site's demos render in CI, the README's `docs/images` in `media-regen.yml` from main.
- Real wire bytes ride ONE pipeline, `pixtuoid_core::harness::Drive` — rules in [`tests/AGENTS.md`](crates/pixtuoid-core/tests/AGENTS.md#the-one-pipeline).
- Fixtures are RECORDED, never composed (`just hermetic --record` for a scenario a profile records, else `just capture-fixture` — BILLED), and the recorder blanks every subtree no decoder reads (derived by probe, never listed); every scenario declares `provenance.json`. Rules: [`fixtures/README.md`](crates/pixtuoid-core/tests/sources/fixtures/README.md). `just restrip-fixtures` re-strips the committed corpus offline; `just corpus-all` censuses local corpora.
- Visual verification for sprite work: snapshot example → `scripts/crop-snapshot.py` → READ the PNG; loop in `.claude/skills/beautify-decoration/SKILL.md`.
- CI is the gate, and a green preflight is NOT a green PR. A PR's pushes run only the light tier; the full tier runs on `main`, and in the merge queue unless Mergify merges an up-to-date PR alone at the front directly (two-step CI).
- Advisory (never gates): `just mutants`, `just bench`, `just bench-pacing`, CodSpeed.
- Fluency gate, local and pre-release (a wall-clock budget, so never CI): `just pace-check`; its runs and verdict in `scripts/pace-check.py`'s header.
- Hooks: `git config core.hooksPath .githooks` once per clone; `just setup-tools` installs cargo tools (incl. rust-analyzer — without it the agent LSP degrades to grep).
- Release is a human step: [`CONTRIBUTING.md#releasing`](docs/CONTRIBUTING.md#releasing).

## Workflow

Non-trivial work runs as [an arc](docs/CONTRIBUTING.md#the-arc-loop): read
each step there before taking it, never from recall. Merging is
`@mergifyio queue`, which a session comments itself once
[the merge gate](docs/CONTRIBUTING.md#the-merge-gate) holds, and a release PR
merges by hand. The CI review applies REVIEW.md on every push;
[arc step 7](docs/CONTRIBUTING.md#the-arc-loop) says when the `local-review`
skill runs it first.

**At most 6 open PRs across the sessions** (bots' and outside contributors'
aside), the owner's cap: the runners take
[20 jobs at once](https://docs.github.com/en/actions/reference/limits) and one
light-tier push needs more, so each open PR past the first lengthens the wait
of every push. A PR
opens, never as a draft, once its branch is ready to merge, its local review
done when one is due. The pre-push hook runs
preflight and CI the rest, so neither is rerun by hand. Until a slot frees, the branch stays pushed
with no PR. A PR
that stops being ready closes with its branch kept and never force-pushed,
since a force-pushed branch can't reopen; a stacked one is retargeted to
`main` first, because merging its base deletes that branch. Closing leaves its
runs queued, and a plain cancel still runs the `always()` gate jobs, so
[force-cancel](https://docs.github.com/en/rest/actions/workflow-runs#force-cancel-a-workflow-run)
them.

Repo skills (committed): `local-review`, `beautify-decoration`,
`add-source`.

## Conventions

- **TDD first** — failing test → minimal impl → commit. **DRY.**
- **Every mechanism earns its upkeep.** Code, config, a test, a CI job or a doc stays only if it names a failure it prevents that nothing else already does, or a need someone has. Extending a mechanism re-adds it: one you touch that can't name one is deleted, not polished. The load-bearing defenses [REVIEW.md](REVIEW.md#design)'s Proportion row lists have named theirs.
- **Reuse before you build.** A need that a maintained library, the platform, or the upstream tool's own mechanism already meets is met by it (Claude Code's plugin install, `[workspace.dependencies]`); a hand-rolled copy is upkeep nothing else would cost. The exception is a reuse whose own cost is larger (wasm32, a version freeze), and the PR names that cost.
- **Chibi figures and pets stay round**, the head flowing into the body.
- **`#[allow(clippy::too_many_arguments)]` is a last resort** — group related parameters into a struct first.
- **Comments: WHY only.** Only what the surrounding thing can't say (workaround, constraint, invariant). **Every comment the repo ships is in scope**, not just `//` and `#` in code: a `.md` doc's prose, a workflow or manifest comment, a justfile recipe header, a CI contract's `why` and a PR body are all held to the rules below — prose is where an assertion hides with no failure mode, so it earns its place the same way code does. Every sentence must add information the earlier ones don't — delete each after the first; if nothing is lost, cut it. First sentence is the whole answer. **A comment sits on the narrowest thing it constrains** — the declaration when it governs the whole item, the statement, struct-literal field or match arm when it governs only that one; hoisting a rationale to the declaration to shorten a body detaches it from the line it was pinned to, which is worse than the length it saved. The default is fewer: a comment is a cost the code must repay, and a link to the authority beats prose about it. A change leaves the net comment volume of what it touches no larger than it found it unless each added line earns its place — true of a PR that fixes comments too. Measurements belong in commit messages, not comments. **Name the authority, never restate its value** — `` × [`MAX_CONCURRENT_CONNS`] slots ``, not `× 128 slots`: a restated value drifts silently while a name greps, and an intra-doc link also turns a rename into a `doc-check` red, private items included (the magic-number rule, applied to prose).
- **No magic numbers** — reuse the existing authority (a dep's const, our registry/theme/layout value), else ONE named `const` at the narrowest covering scope; prefer a type (enum/newtype) for a related set. Two copies of one value is a latent drift bug — if a copy must cross a boundary, pin the pair with a test. Self-evident `0`/`1`/`2`, indices, and test fixtures stay inline.
- **Errors**: `anyhow::Result` in app code, `thiserror` in core; hook listener + JSONL watcher log-and-continue, never panic. **No `unwrap()` outside tests.**
- **Visibility**: layer-internal stays `pub(crate)` (`unreachable_pub` misses a `pub` reachable through a `pub mod`; `just hawk`, a full-tier gate, fails on one no other workspace crate's code needs); `#[doc(hidden)] pub` is a mechanism-not-contract escape hatch. The binary's lib has one `pub mod`, the `#[doc(hidden)]` `pixtuoid::dev` facade its examples, tests and benches reach it through, so `unreachable_pub` reports any `pub` that list doesn't name.
- **No scan-the-history** — keep state updated as events arrive; never derive it by scanning backward.
- **Shell**: match the surrounding shell; macOS-first (BSD CLI, brew).
- **Docs current in the same commit** as any structure/API/workflow change.
- **External-surface claims are fetched, not remembered**, and so is "best practice": cite the `path:line` or doc URL you fetched THIS session or add a drift watch (`just drift-watch`), and label anything unsourced opinion; the population is the whole upstream repo (`gh api .../git/trees/<ref>?recursive=1`), not one plausible file.
- **A refuted review finding produces a MECHANISM, or nothing** — a test, a compile-time constraint, or a CI gate; refuting never produces prose, because prose has no failure mode. Only an EXTERNAL fact (another CLI's wire bytes, an OS semantic) earns a comment, on the narrowest thing it constrains. **A real finding ends FIXED, RE-SCOPED → #N or FOLLOW-UP → #N before the PR merges; which applies, the four terminal states and what #N may be are defined once in [`CONTRIBUTING.md`](docs/CONTRIBUTING.md#dispositions). Agents never file issues.**
- **Only the latest released version of each agent CLI is supported.** When upstream renames or reshapes a wire name, repoint the decoder, the plugin, and the drift-watcher anchor at the CURRENT declaration and DELETE the old one — no dual-listening beside a replacement, no legacy-format arm kept "just in case". Every superseded arm is a second copy of a wire contract that drifts silently and that the watcher then has to anchor twice. Two things this does NOT govern: OUR OWN upgrade path (an arm for an artifact an older pixtuoid wrote is ours to keep or drop, not upstream's — #457), and mirroring a resolver upstream itself still branches on.
- **Path asserts compare `PathBuf` structurally**, never `to_string_lossy()` with a hardcoded separator — string asserts pass on Unix and fail only in `windows-test`. Resolution POLICY (HOME vs USERPROFILE, %APPDATA% vs `~/.config`) is per-CLI: mirror each CLI's own resolver (`platform::home_first_dir`).

## Architecture invariants (load-bearing)

1. **`pixtuoid-core` and `pixtuoid-scene` have no terminal/window/audio-device deps** (compiler-enforced by crate boundary; `just arch` holds their dep set to the allowlist in `deny-arch.toml`, so a new crate fails until it is listed). New render targets are thin painters over `pixtuoid_scene::look::render`.
2. **Agent events flow through ONE channel** `mpsc::Sender<(Transport, AgentEvent)>`; the `Transport` tag drives hook-wins dedup — producers tag their own events. Daemon presence rides a separate `AgentId`-free channel (`PresenceMsg { key: DaemonInstanceKey, delta }`) and never enters `Reducer::apply`.
3. **`Source` trait is the only seam** for a transcript-bearing CLI; per-source format knowledge lives in that source's decoder. Exceptions: hook-only CLIs (Reasonix) and the shared ACP wire standard (`source/acp.rs`, reused by grok) — [`CONTRIBUTING.md`](docs/CONTRIBUTING.md#adding-a-new-agent-cli) step 3 and `source/acp.rs`'s header.
4. **Hook install writes through symlinks** (`resolve_symlink` in `install/io.rs`) — critical for stow-managed configs; Windows keeps the bounded rename-retry.
5. **The hook shim never blocks CC** — always exit 0 silently; the send bound (pixtuoid-hook's `transport::WRITE_TIMEOUT`) is watchdog-enforced on both platforms. Shim coverage is child-process level only.
6. **Walkable mask = ground footprint only**; sprite size never moves a sim position — the canvas fit (`keep_sprite_on_canvas`) adjusts only a placement's paint top-left (#912).

## Ownership by crate

Who owns what: **core** owns session lifecycle/identity (registration, dedup,
first-sight, liveness ladder, subagent parenting, feature boundaries) ·
**scene** owns look/motion (per-agent palette recolor, walk timing,
footprints, sky/light invariants, reachability) · **binary** owns
install/runtime wiring (config rewriting, desk growth, boot order, doctor,
daemon announce-only) · **tui** owns the flush (popup geometry, the
office's hit-test ladder) · **panels** owns what both painters' panels share
(their key dispatch and mouse ladder). Terminal cell aspect drives sprite design: the
half-block ▀ technique assumes ~1:2 cells, so sprites past ~16×16 px break on
taller-cell terminals; bundled base character sprites max at 8×12 px (their
`@Nx` variants are cutaway art the half-block painter never draws).

## Things NOT to do

- No write to another CLI's config outside `install/io.rs` (`write_config_atomic` / `ConfigLock`).
- No `println!`/`eprintln!` on production paths — a print macro panics when its reader leaves (`| head`), so CLI and headless output goes through a `CliOut` (a command's is `crate::cli_stdout()`) and a stderr notice is a `let _ = writeln!`. Diagnostics are `tracing`, with a constant message and every value as a field (`error = %e`, `path = ?path`, `pid`): the constant message is a stable grep key and a field is filterable; a value baked into the format string is neither. Every path, session id and cwd rides a `?` field, never `%`, whatever its provenance, and so does an error chain that can embed one (`SourceDeath.error`, `SocketBusy`): `str`'s `Debug` escapes control and bidi chars, while under the default `fmt` layer a Display field writes them raw and `EscapeGuard` covers the message and error text only (`test_capture`'s `debug_fields_escape_control_and_bidi_chars`). Core logs under whatever subscriber hosts it, so the rule is primary; the binary's `EscapedFields` (`app/logging.rs`) escapes every value only as a backstop. Only a whole-message relay re-emitting an already-sanitized user-facing string (`config::warn_user`, the `read_log` warning in `sources_cli`) interpolates.
- Never add `--no-verify`/hook-skipping flags.
- No new `.md` files, READMEs, CHANGELOGs, or docs unless the owner explicitly asks — the owner reviews every doc change directly, so propose the diff rather than adding a generator or a cap. A session pushes its own PR branches freely; never force-push or push to `main`.
- No stale `Closes #N` on a re-scope (fires from commit body or PR text, even conditional).
- No merging past the review gate (PR #23 merged unreviewed with a path traversal), and no reviewer finding accepted before its premise is checked against the item it names.

## Where to look

- Tool call → sprite: `runtime/driver.rs::run_async` → `SourceManager::spawn` → source → decoder → `reducer::Reducer::apply` → watch channel → `TuiRenderer::render` → `pixtuoid_scene::floor::OfficeSession::render` → `pixtuoid_scene::look::render` → `tui::renderer::flush_classic`.
- Architecture + data flow: [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).
- Refactoring the channel type, `Source` trait, `AgentEvent`, or reducer signature touches all four test areas (`tests/reducer/`, `tests/e2e.rs`, `tests/transport/socket.rs`, `tests/watcher/`) + `runtime/driver.rs`; a new `AgentEvent` variant needs an `agent_id()` arm.
- Adding an agent CLI: [`CONTRIBUTING.md`](docs/CONTRIBUTING.md#adding-a-new-agent-cli)'s checklist, which the `add-source` skill drives.
