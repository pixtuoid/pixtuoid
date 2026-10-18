# Contributing to pixtuoid

Thanks for your interest! PRs are welcome — especially **new themes**, sprite and
decoration polish, and **`Source` adapters** for agent CLIs we don't support yet
(the agent CLIs plus the OpenClaw gateway already wired up are listed in the README).

Before you start, read [`AGENTS.md`](../AGENTS.md) at the repo root (and the
nested `crates/*/AGENTS.md` for the crate you touch). It holds the load-bearing
architecture invariants and conventions. Many things that look like bugs are
documented, intentional design: read the whole item, its doc comment and the
comments on the lines it governs, before changing it.

## Build & test

Requires a recent stable Rust toolchain and [`just`](https://github.com/casey/just)
(`brew install just`). On Linux you also need `lld`, `pkg-config` and the ALSA
headers (`apt install lld pkg-config libasound2-dev`), and, with no GPU driver,
Mesa's software Vulkan for the GPU tests (`apt install mesa-vulkan-drivers`).
The git hooks and most CI jobs call `justfile` recipes.

```bash
just              # list recipes
just preflight    # pre-push gate: lint → clippy; `just preflight full` adds hack → test (CI's Rust recipes)
just fmt          # auto-format
just test         # the suite without the `upstream` feature, under cargo-nextest (`just setup-tools`)
just test -p <crate> <filter>   # fast loop while iterating on one crate
just test -p pixtuoid --features upstream drift_watch   # the drift watch's tests, built only under it
```

> **Don't expect clippy to warm `test`'s build** — its check-mode (rmeta)
> builds carry over only build scripts and proc-macros, so iterate with one of
> them.

Activate the git hooks once per clone: `git config core.hooksPath .githooks`
(`pre-commit` = `just fmt-check`; `pre-push` = `just preflight`).

`just hermetic [--cli pinned|latest] [--record] [source [scenario]]` runs each profiled CLI against a
scripted model (aimock) with a headless pixtuoid: the full tier gates on it at
each `verified_version`, the nightly runs it at latest. `scripts/lib/`'s tiers
run in no CI: `just openclaw-e2e` · `just openclaw-multi-e2e`
· `just openclaw-backend-e2e` (BILLED) · `just live-sources [id ...]` (BILLED).

## Releasing

### Versioning

Pre-1.0: **patch (`0.y.Z`)** = bug fixes and polish only — no new public API,
nothing breaks. **minor (`0.Y.z`)** = everything else: new user-facing features
AND any breaking change to the published crates' API. Both halves are machine-
applied on the release PR, not per-PR: release-plz derives the level from the
commit log (`features_always_increment_minor` in `release-plz.toml` is the
"features also bump minor" half), and release-plz's own `cargo-semver-checks`
run is the "nothing breaks on a patch" half: a detected break raises the bump
to the next minor on its own, and the release PR's body reports it. Never weaken
a lint to dodge the bump.

### Cutting the release

[release-plz](https://release-plz.dev) owns every version number and the tag;
`release.yml` still owns every publish. Four steps, all human-initiated:

1. **Dispatch** `release-plz.yml` from Actions, on `main`. It opens
   `chore(release): vX.Y.Z` from a `release-plz-*` branch, with the workspace
   version, every path-dep requirement, `Cargo.lock` and `CHANGELOG.md`
   rewritten.
2. **Review it like any PR, but merge it by hand**: the merge queue refuses it,
   since it would squash it onto whatever `main` has become by then, which
   `release-merge` refuses after the merge. If `main` moved, **re-dispatch —
   never "Update branch"**: only a dispatch recomputes `CHANGELOG.md` for the
   new commits, and the merge commit "Update branch" adds counts as a human's,
   so the next dispatch closes this PR and opens a new number. Merged behind
   `main`, or with `main` merged or rebased in, it fails `release-plz.yml`'s
   `release-merge` and publishes nothing. Raise the bump with
   `cargo set-version --workspace X.Y.Z` (cargo-edit) and push only for a break
   `cargo-semver-checks` cannot see. A user-facing change that touched no packaged file
   (`npm/`, `release.yml` packaging) is not in the generated notes — add its line
   to `CHANGELOG.md` by hand as the last commit before merging.
3. **Merge it** (squash). That merge is the *irreversible* step: the `release` job
   publishes every crate to crates.io over OIDC, creates `vX.Y.Z` and a DRAFT
   GitHub release carrying the changelog; the tag then fires `release.yml`,
   which builds every target in its build matrix and the debs, attaches them,
   publishes the draft, and publishes the npm packages. The crates.io and npm
   jobs each wait for an approval from the `release` environment's reviewer,
   crates.io first. The tag also starts a homebrew-core autobump.
4. **If the dispatch's run summary listed an owed homebrew-core test update**,
   open `pixtuoid: update test` there once the tag exists, carrying
   packaging-build's replay as its test block.

The crates.io upload happens in the `release` job on the merge push, which
first waits for that commit's own `ci-gate`: a failure or timeout publishes
nothing, and re-running the workflow after a passing `ci.yml` re-run resumes it.
The wait is in the workflow because the queue can land other PRs while a
release PR is open, so no merge-time check proves the tree it publishes.

`cargo-semver-checks` runs inside release-plz on the release PR, not as a CI
job; `just semver` reproduces its verdict locally.

Both jobs authenticate with `RELEASE_PLZ_TOKEN`, a fine-grained PAT scoped to
this repository with Contents and Pull requests read/write; `release-plz.yml`'s
header says why it cannot be the automatic token.

A release PR that release-plz closes and re-opens (it does that when the branch
carries non-bot commits) leaves a commit you pushed to it — a raised bump —
behind: `git cherry-pick` it onto the new branch. No committed frame carries the
version (`BOARD_BRAND`), so a release PR needs no `just gen`.

The tag also publishes **outside** this repo:
homebrew-core's formula is `autobump: true` and builds from the tag tarball,
instantly, with DEFAULT features on macOS *and* Linux — the one configuration
our release never builds. Two consequences:

- **A from-source build break lands in Homebrew's CI, not ours.** Anything
  adding a system-library dependency needs a matching `depends_on` in the core
  formula, in the same bump PR.
- **Their `test do` block is a public contract** — see the "homebrew-core
  contract" comments at `crates/pixtuoid/src/app/sources_cli.rs`,
  `crates/pixtuoid-core/src/source/codewhale.rs`; the packaging-build action
  replays the block. A dispatch only warns when the block calls a subcommand
  this tree dropped, or connects a source that fails with no agent CLI
  installed, because the owner, sending a test update ahead of the release that
  needed it, was told to release first (no public record); step 4 sends it.

Do not try to preempt BrewTestBot: the formula is on homebrew-core's
autobump list, so `brew bump-formula-pr pixtuoid` refuses by policy and the
bot opens the PR itself on its next scheduled run
([`autobump.yml`](https://github.com/Homebrew/homebrew-core/blob/main/.github/workflows/autobump.yml)).
Watch THAT PR's CI and intervene only if it reds.

Publishing uses **OIDC trusted publishing** — CI carries no registry tokens;
the per-crate/per-package Trusted Publishers, each naming environment
`release`, must exist before the tag
([#216](https://github.com/pixtuoid/pixtuoid/issues/216)).

## The arc loop

Non-trivial work (a new feature, config key, flag, seam or module, a
documented sharp edge, or 3+ *counted files*: changed files outside tests,
fixtures, snapshots, lockfiles, `.md`, binary assets and what a `just gen*`
recipe regenerates; not a typo, a docs fix, a mechanical move or a one-file
fix with its pinning test) runs as an **arc**: design → build → gate → wrap.

1. **Pick** — an issue (`gh issue list`) or backlog item.
2. **Grill the design** — decide the open questions one at a time, each with a
   recommended answer, before writing code.
3. **Design gate** (before build) — three lenses so slop dies in design:
   best-practice search (confirm the idiomatic way against real docs online,
   never memory) · adversarial design review (red-team the design before code
   exists) · deepening lens (would deleting this concentrate complexity or
   just move it? does the change deepen a module or add a shallow one? a
   refactor it warrants is in scope).
4. **Spec** — synthesize into `docs/superpowers/specs/` (LOCAL, git-ignored),
   planned against [`REVIEW.md`](../REVIEW.md)'s lenses; the PR body states
   the change's claims, which the correctness lens holds it to.
5. **Mock gate** (taste/visual work only) — ratify the AFTER visual before code
   (`beautify-decoration` skill).
6. **Build** — TDD: failing test → minimal impl → commit.
7. **Local review** — before the push, when the change touches 5+ counted
   files or the owner asks: the `local-review` skill runs
   [`REVIEW.md`](../REVIEW.md) with the reviewers CI runs; the cleanup rides
   the same PR.
8. **Merge gate** — [the gate](#the-merge-gate).
9. **Wrap** — retro; a durable lesson becomes a mechanism (a test, a gate) or
   a line on the narrowest rule it amends — never an agent's private memory,
   which nobody reviews and nothing executes.

Checks hold only the merge gate ([`.mergify.yml`](../.mergify.yml)'s
`queue_conditions`) and step 6's tests (the review's test teeth); every other
step is the session's to keep.

### Parallel sessions

- **One `git worktree` and one cargo target per branch** — a target shared
  across branches swaps uplifted examples and builds one branch's types into
  another. Targets run to several GB each: check `df -h /` before parallel
  builds, and remove a PR's worktree and local branch once it merges.
- **Fold before opening** — a change to a surface an open PR already touches
  folds into it.
- **The queue never idles** — it checks one batch at a time (`.mergify.yml`'s
  `max_parallel_checks`), so queue every PR that holds the gate, in priority
  order, at once; dequeue one only when it would jump a priority PR that is
  already green.

## Pull requests

### The merge gate

Every [`.mergify.yml`](../.mergify.yml) `queue_conditions` check green at the
final head; every finding's review thread resolved by its disposition; zero
open confirmed blocking finding. Each [must-run](../REVIEW.md#must-run) item
the change touches has its result in the PR body, and a
[local review](../.claude/skills/local-review/SKILL.md)'s findings take
dispositions like the bots'. A refuted blocking finding clears
without a push: once its thread is resolved with the disposition, re-run the
review (`gh run rerun <id> --failed`).

Once the gate holds, comment `@mergifyio queue` ([`.mergify.yml`](../.mergify.yml)):
entry is a command because no queue condition can confirm a finding. The
queue tests up to `batch_size` PRs together on a draft PR
(`mergify/merge-queue/…`) running the full tier, then merges the PRs
themselves, but merges an up-to-date PR alone at the front on its own CI
([direct merge](https://docs.mergify.com/merge-queue/direct-merge/)); it never updates a PR's own branch
([batches](https://docs.mergify.com/merge-queue/batches/): "the original PRs
are the ones merged"), so the bots' statuses on the PR's head are the ones its
`queue_conditions` read. A session or the owner queues a bot's PR after reading
what no review sees: `media-regen.yml`'s images, `hermetic-nightly.yml`'s whole
diff, fixtures included.

The review skips a fork's or a bot's PR, and its skipped check passes. A
maintainer reviews a fork's or Dependabot's PR by hand before queueing it; a
fork's author can resolve their own threads, so read each thread's
`resolvedBy` and its reply.

### Dispositions

Every finding reaches exactly one terminal state in its review thread: FIXED ·
REFUTED (cite the mechanism, per AGENTS.md; add one where none exists. Before
adding code for a finding, establish its case is reachable: when a test or
sweep shows it isn't, that test is the mechanism and no defensive code lands) ·
RE-SCOPED → #N (real and INTRODUCED — or first made reachable — by this
change, and bigger than the PR: split it off into #N; a redesign that brings
the finding into scope ends FIXED) · FOLLOW-UP → #N (real and PRE-EXISTING,
and not FIXED in place
— in place fits a small defect inside code this change already touches,
pulling in no new must-run item — so it is fixed in #N; a defect in another session's
tree cites that session's PR). A
disposition is the reply that resolves the thread, STARTING with its state:
`FIXED: …` · `REFUTED: … — <mechanism>` · `RE-SCOPED → #N: …` ·
`FOLLOW-UP → #N: …`, where #N is a PR other than this one: open, merged, or
closed under [the open-PR cap](../AGENTS.md#workflow) with the fix on its branch. A
re-flag of an already-dispositioned finding replies with the original's
disposition (link it). "Acknowledged" and "surfaced" are not states. Sweep at
the FINAL merge head; check WHICH commit a bot re-flag was raised against
before re-litigating.

### Convergence contract

- **Churn budget** — a diff whose added + modified lines exceed ~1500 is split
  (stacked PRs) before review. Pure deletions are exempt once censused; a
  change that both adds and deletes at scale is two PRs.
- **Deletion census** — before deleting N members of a class, the full list
  and its criterion land in the first commit or the PR body (#943).
- **Fixes are never put off.** Each fold takes every accepted finding,
  whatever code it names, into ONE commit: a defect this change introduced is
  FIXED in it, since a clean-up left for later rarely lands
  ([Google](https://google.github.io/eng-practices/review/reviewer/pushback.html#cleaning-it-up-later)).
  The loop converges because a re-review judges only the PR's own change since
  the last reviewed head (the [shared review](https://github.com/pixtuoid/.github/blob/main/.github/workflows/claude-review.yml)'s
  scope), so a fold's findings are the fold's. A blocking issue confirmed in a
  fold STOPS the loop: revert the fold and re-land smaller, or re-scope.
- **A fold's behavior change**, a user-visible string included (a doc
  comment clap renders as `--help` is one), ships a test that fails without
  it; the rest of a fold is a revert, a deletion, a
  comment or doc change, or a refactor the existing tests cover. Anything
  else reverts the fold.
- **A fix round adds no gate** beyond a refutation's mechanism — any other
  wanted check is its own PR, asserting
  facts in its own layer (a Rust fact from Rust, never a Python regex over
  `.rs`).

## Adding a new agent CLI

The registration steps (4–7, 9) and step 12's roster literals are test-forced —
skipping one fails `just test`. Step 8 is forced only for hook-only sources;
step 10 by the theme guards; steps 1–3, 11 and step 12's `#[test]` are on you.

1. **Verify the wire format against the CLI's actual source/releases first** —
   transcript location, line shape, hooks, session identity; pin every fact
   to an upstream file/version. **Audit its HOME RESOLVER per axis in the
   same pass** — PROBE the installed artifact rather than trusting docs; an
   unmirrored axis is fail-silent: the watcher polls a directory the CLI
   never writes and the office stays empty (#880). Resolver axes are
   deliberately NOT drift-watched — re-run the probe matrix when the CLI majors.
   A custom root gets ONE `pub fn <cli>_home()`, called by both the watcher's
   `default_paths()` and the installer's `default_config_path()` so they
   can't disagree.
2. **Write the source module** — `crates/pixtuoid-core/src/source/<name>.rs`:
   `SOURCE_NAME`, a `LineDecoder` fn (one JSONL line → `Vec<AgentEvent>`), a
   label deriver, unit tests per event mapping. Format knowledge lives HERE.
3. **Implement the `Source` trait** (an async `run(self, tx)` watching +
   decoding until the session universe ends). **Hook-only CLI?** Skip the
   decoder, trait, and step 7: `transcript: None` in the registry row, format
   knowledge in a `hook.custom` decoder (it must claim EVERY event), and do
   step 8 instead.
4. **Add ONE `SourceDescriptor` row** in `source/registry.rs` — label prefix,
   decoder, hook keying, `tool_id_key` (verify against a CAPTURED tool call,
   not a neighbour — kimi's `ToolCall` cost a source its tool ids), truthful
   capability flags, `verified_version` + `version_probe`. Lifecycle policy
   derives from the flags; you do **not** edit the reducer.
5. The descriptor's `name` **is the roster** — `registered_source_names()`
   projects `REGISTRY`; the conformance suite then requires a fixture, and
   `harness/hermetic/mod.rs` a `PROFILES` row or a `NOT_HERMETIC` reason. The
   `sources --json` golden (`crates/pixtuoid/tests/snapshots/cli/sources.json`)
   must list it: `SNAPSHOTS=overwrite just test -p pixtuoid --test cli_json`.
6. **Record the fixture** — the test steps in
   [`crates/pixtuoid-core/tests/AGENTS.md`](../crates/pixtuoid-core/tests/AGENTS.md)
   (a RECORDED SessionStart scenario via `just capture-fixture`), then
   `cargo insta review`.
7. **Wire it into `runtime/driver.rs::build_source_set`** (the one
   construction site; the registry drives the guard test, not the spawning).
8. **If the CLI has hooks**, add an `install/` target: a `Target` row whose
   `Primary` is either the CLI's config (`merge_install`/`merge_uninstall` + a
   `verify_schema` fn mirroring its format) or a plugin file pixtuoid owns
   whole, plus the registered-events↔decoder-arms guard.
9. **Add a row to `site/src/sources.json`** (`status`, per-OS `platforms`), then `just gen-readme`. Pinned to `registered_source_names()`
   by `supported_sources_manifest.rs`.
10. **Add the per-source badge hue** — a `SourceColors` field + value in EVERY
    theme file + `badge_color` in the manifest row; the coverage, legibility
    and site-bridge tests fail until it exists.
11. **Drift-watch in the same PR**: the source module's `UPSTREAM`, named in
    its registry row — a `Watch` beside each decoded set, unless REVIEW.md's
    **Wire format** check settles it with a breadcrumb or an adjudicated
    `EXEMPT` row — and the installer's, named in its `Target` row, a `Watch`
    over each hook set it registers and each name a shipped template reads
    (`each_template_read_is_watched` holds the two together, both ways where
    the template is scanned). A source whose dependence lives wholly in its installer is
    `Upstream::Installer`. The types are in `source/upstream.rs`, and
    `just drift-watch` runs them. A value our shipped artifacts bake in is a
    `Watch` too, its `ours` read off the artifact rather than copied
    (OpenClaw's `default_gateway_port`).
12. **Three roster literals in three test binaries** (a scoped run misses
    them): the row-by-row byte pin in `corpus_check.rs`; `TOOL_ID_KEY_UNPROVEN`
    in `tests/sources/captures.rs`; a case row + `#[test]` in
    `crates/pixtuoid/tests/wire_to_pixels.rs`.

## License

By contributing, you agree your contributions are licensed under the same terms
as the project.
