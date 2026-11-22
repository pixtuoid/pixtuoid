# Contributing to pixtuoid

Thanks for your interest! PRs are welcome, especially **new themes**, sprite
and decoration polish, and **`Source` adapters** for agent CLIs we don't
support yet. The README lists the agent CLIs already wired up, and the OpenClaw
gateway.

Before you start, read [`AGENTS.md`](../AGENTS.md) at the repo root, and the
nested `AGENTS.md` of each crate you touch. They hold the load-bearing
architecture invariants and conventions. Many things that look like bugs are
documented, intentional design, and AGENTS.md says what to read before
changing one.

## Build & test

You need a recent stable Rust toolchain and [`mise`](https://mise.jdx.dev),
[activated](https://mise.jdx.dev/getting-started.html#activate-mise): its
`mise install` brings `just` and the other dev tools. On Linux you also need
`lld`, `pkg-config` and the ALSA headers (`apt install lld pkg-config
libasound2-dev`), and, with no GPU driver, Mesa's software Vulkan for the GPU
tests (`apt install mesa-vulkan-drivers`).

```bash
just              # list recipes
just preflight    # the pre-push gate; `full` adds some of CI's other Rust recipes (`just --list` shows which)
just fmt          # auto-format
just test         # the suite, under cargo-nextest (`just setup-tools`)
just test -p <crate> <filter>   # fast loop while iterating on one crate
```

> **Don't expect clippy to warm `test`'s build** — its check-mode (rmeta)
> builds carry over only build scripts and proc-macros, so iterate with one of
> them.

Activate the git hooks once per clone: `git config core.hooksPath .githooks`
(`pre-commit` runs `just fmt-check`, `pre-push` runs `just preflight`).

`just hermetic [--cli pinned|latest] [--record] [source [scenario]]` runs each
profiled CLI against a scripted model (aimock), then its install's lifecycle
and a daemon's live tier, all with a headless pixtuoid. The full tier gates on
it at each `verified_version`; the nightly runs it at each CLI's latest.

## Releasing

### Versioning

Before 1.0:

- **A patch (`0.y.Z`)** is bug fixes and polish only: no new public API,
  nothing breaks.
- **A minor (`0.Y.z`)** is everything else: a new user-facing feature, or any
  breaking change to a published crate's API.

Two halves of them are applied by machine on the release PR, never per PR:

- **A feature bumps the minor.** release-plz derives the level from the
  commit log, and `features_always_increment_minor` in `release-plz.toml`
  makes a feature bump the minor.
- **Nothing breaks on a patch.** release-plz's own `cargo-semver-checks` run
  raises the bump to the next minor on its own when it detects a break, and
  the release PR's body reports it. It is not a CI job; `just semver`
  reproduces its verdict locally. Never weaken a lint to dodge the bump.

### Cutting the release

[release-plz](https://release-plz.dev) owns every version number and the tag;
`release.yml` still owns every publish. A person starts each of the four
steps:

1. **Dispatch** `release-plz.yml` from Actions, on `main`. It opens
   `chore(release): vX.Y.Z` from a `release-plz-*` branch, with the workspace
   version, every path-dep requirement, `Cargo.lock` and `CHANGELOG.md`
   rewritten.
2. **Review it like any PR, but merge it by hand.** The merge queue refuses
   it: the queue would squash it onto whatever `main` has become by then,
   which `release-merge` refuses after the merge.
   - **If `main` moved, re-dispatch; never "Update branch".** Only a dispatch
     recomputes `CHANGELOG.md` for the new commits, and the merge commit
     "Update branch" adds counts as a human's, so the next dispatch closes
     this PR and opens a new number. Merged behind `main`, or with `main`
     merged or rebased in, the PR fails `release-plz.yml`'s `release-merge`
     and publishes nothing.
   - **Raise the bump** with `cargo set-version --workspace X.Y.Z`
     (cargo-edit), and push, only for a break `cargo-semver-checks` can't see.
   - **A user-facing change that touched no packaged file** (`npm/`,
     `release.yml`'s packaging) is missing from the generated notes: add its
     line to `CHANGELOG.md` by hand, as the last commit before merging.
   - **If release-plz closes and re-opens the PR**, which it does when the
     branch carries non-bot commits, a commit you pushed to it, such as a
     raised bump, stays behind: `git cherry-pick` it onto the new branch.
   - No committed frame carries the version (`BOARD_BRAND`), so a release PR
     needs no `just gen`.
3. **Merge it** (squash). That merge is the *irreversible* step:
   - the `release` job publishes every crate to crates.io over OIDC, and
     creates `vX.Y.Z` and a DRAFT GitHub release carrying the changelog;
   - the tag then fires `release.yml`, which builds every target in its build
     matrix, attaches them, publishes the draft, and publishes the npm
     packages;
   - the crates.io and npm jobs each wait for an approval from the `release`
     environment's reviewer, crates.io first;
   - the tag also starts a homebrew-core autobump.
4. **If the dispatch's run summary showed homebrew-core's formula differing**,
   then once the tag exists, send each hunk this tree's `pixtuoid.rb` made as
   a PR there, and bring each hunk homebrew-core made into `pixtuoid.rb` here.

### What guards a release

- **The crates.io upload waits for CI.** It runs in the `release` job on the
  merge push, which first waits for that commit's own `ci-gate`: a failure or
  a timeout publishes nothing, and re-running the workflow after a passing
  `ci.yml` re-run resumes it. The wait is in the workflow because the queue
  can land other PRs while a release PR is open, so no merge-time check
  proves the tree it publishes.
- **Credentials.** `release-plz.yml`'s `release-pr` and `release` jobs
  authenticate with `RELEASE_PLZ_TOKEN`, a fine-grained PAT scoped to this
  repository with Contents and Pull requests read/write; the workflow's
  header says why it can't be the automatic token. Publishing uses **OIDC
  trusted publishing**, so CI carries no registry tokens: each crate's and
  each package's Trusted Publisher, naming the environment `release`, must
  exist before the tag
  ([#216](https://github.com/pixtuoid/pixtuoid/issues/216)).

### Homebrew

The tag also publishes **outside** this repo. homebrew-core's formula is
`autobump: true` and builds from the tag's tarball at once, with DEFAULT
features on macOS *and* Linux: the one configuration our release never builds.

- [`pixtuoid.rb`](../.github/actions/packaging-build/pixtuoid.rb) is the
  formula we hold them to. packaging-build runs Homebrew's own
  `brew install --build-from-source` and `brew test` of it in the full tier
  (`main` and the merge queue), sandboxes included. A new system-library
  dependency, or a `test do` this tree breaks (see the "homebrew-core
  contract" comments), fails there and is fixed in that file.
- A dispatch warns with the diff wherever homebrew-core's copy differs, and
  never fails: the owner, who sent a test update before its release, was told
  to release first (no public record). Step 4 settles it.
- Don't try to preempt BrewTestBot. The formula is on homebrew-core's
  autobump list, so `brew bump-formula-pr pixtuoid` refuses by policy, and
  the bot opens the PR itself on its next scheduled run
  ([`autobump.yml`](https://github.com/Homebrew/homebrew-core/blob/main/.github/workflows/autobump.yml)).
  Watch THAT PR's CI, and step in only if it fails.

### A release that bumps `remote::VERSION`

It goes out with the iOS App, in this order:

1. Once the bump is on `main`, build the App against that rev through
   [`[patch.crates-io]`](https://doc.rust-lang.org/cargo/reference/overriding-dependencies.html#working-with-an-unpublished-minor-version)
   and submit it with ["Manually release this version"](https://developer.apple.com/help/app-store-connect/manage-your-apps-availability/select-an-app-store-version-release-option),
   rebuilding it if `VERSION` moves again before the tag.
2. Once it is Pending Developer Release, cut the release.
3. Release the App once the tag's homebrew-core bump PR merges. App Store apps
   on iPhone ["automatically update by default"](https://support.apple.com/en-us/HT202180),
   so an App out sooner tells most of its users to update a CLI that
   `brew upgrade` can't deliver until BrewTestBot's next run and that PR's CI.

Two gaps remain, each leaving the App asking for its own update until that
arrives: a CLI installed another way before the brew merge, and the "up to 24
hours" Apple's release-option page gives a release to reach the App Store.

## The arc loop

An **arc** is design → build → gate → wrap, in the nine steps below.

Non-trivial work runs as an arc. Work is non-trivial when it has any of:

- a new feature, config key, flag, seam or module;
- a documented sharp edge;
- 3 or more *counted files*. A counted file is a changed file outside tests,
  fixtures, snapshots, lockfiles, `.md`, binary assets and what a `just gen*`
  recipe regenerates.

A typo, a docs fix, a mechanical move, or a one-file fix with its pinning test
is trivial and needs no arc.

1. **Pick** a [Roadmap](https://github.com/orgs/pixtuoid/projects/1) item, an
   issue or a draft.
2. **Grill the design.** Decide the open questions one at a time, each with a
   recommended answer, before writing code.
3. **Design gate**, before the build. Four lenses, so slop dies in design:
   - **Best-practice search**: confirm the idiomatic way against real docs
     online, never memory.
   - **Adversarial design review**: red-team the design before code exists.
   - **Deepening**: would deleting this concentrate complexity or just move
     it? Does the change deepen a module or add a shallow one? A refactor it
     warrants is in scope.
   - **Reuse**: which layer lets every surface read this feature, which
     existing surfaces take it now, and why the rest don't.
4. **Spec.** Write it in `docs/superpowers/specs/` (local and git-ignored),
   planned against [`REVIEW.md`](../REVIEW.md)'s lenses. The PR body states
   the change's claims, and the review's Claims lens holds it to them.
5. **Mock gate**, for taste or visual work only: ratify the after visual before
   code (the `beautify-decoration` skill).
6. **Build**, test first, as [the org rules](../AGENTS.md#org-rules)' TDD says.
7. **Local review**: the org's local-review skill, when
   [the org rules](../AGENTS.md#org-rules)' Local review says.
8. **Merge gate**: [the gate](#the-merge-gate).
9. **Wrap**: a retro. A durable lesson becomes a mechanism (a test, a gate) or
   a line on the narrowest rule it amends, never an agent's private memory,
   which nobody reviews and nothing executes.

Only three steps have a check behind them; the session keeps every other step
itself:

- step 8, through the org's [`.mergify.yml`](https://github.com/pixtuoid/.github/blob/main/.mergify.yml)
  `queue_conditions`;
- step 6's tests, through the review's Test teeth;
- step 7, through the head it ended on, which the PR body names
  ([the org rules](../AGENTS.md#org-rules)).

### Parallel sessions

- **One `git worktree` and one cargo target per branch.** A target shared
  across branches swaps uplifted examples and builds one branch's types into
  another.
- **Check `df -h /` before parallel builds.** A target runs to several GB.
- **Remove a PR's worktree and local branch once it merges.**
- **Add to an open PR first.** A change to a surface an open PR already
  touches goes into that PR.
- **Keep the queue busy.** It checks one batch at a time (the org queue's
  `max_parallel_checks`), so queue every PR as soon as what no check can
  confirm holds ([the merge gate](#the-merge-gate)). Dequeue one only when it
  would jump a green priority PR.

## Pull requests

### The merge gate

A PR merges only when, at its final head:

- every [`.mergify.yml`](https://github.com/pixtuoid/.github/blob/main/.mergify.yml)
  `queue_conditions` check is green;
- and what no check can confirm holds:
  - every review of that head is posted and read;
  - every finding's thread is resolved by its [disposition](#dispositions);
  - no confirmed blocking finding is open;
  - the must-run results are in the PR body.

Two special cases:

- A bot's finding posted only in its review body gets a thread.
- A refuted blocking finding needs no push. Resolve its thread with the
  disposition and re-run the review (`gh run rerun <id> --failed`); its
  verifier judges the refutation once.

#### Queueing

- Comment `@mergifyio queue` once what no check can confirm holds. The command
  waits for the `queue_conditions`
  ([queue](https://docs.mergify.com/commands/queue/)).
- A push cancels the command or dequeues the PR, so comment again after one.
- A session or the owner queues a bot's PR only after reading what no review
  sees: `media-regen.yml`'s images, and `hermetic-nightly.yml`'s whole diff,
  fixtures included.

#### How Mergify merges

- It tests up to `batch_size` PRs together on a draft PR
  (`mergify/merge-queue/…`) running the full tier, then merges the PRs
  themselves.
- An up-to-date PR alone at the front merges on its own CI instead
  ([direct merge](https://docs.mergify.com/merge-queue/direct-merge/)).
- It never updates a PR's own branch
  ([batches](https://docs.mergify.com/merge-queue/batches/): "the original PRs
  are the ones merged"), so the bots' statuses on the PR's head are the ones
  its `queue_conditions` read.

#### A fork's or a bot's PR

The CI review skips a fork's or a bot's PR, and its skipped check passes.

- A maintainer reviews a fork's or Dependabot's PR by hand before queueing it,
  holding it to its claims and must-run results.
- A fork's PR owes no local review. The review run on its commits, below,
  stands in for it, or, when its diff touches a workflow, the hand review does.
- A fork's author can resolve their own threads, so read each thread's
  `resolvedBy` and its reply.
- **To run the review on a fork's commits**, first read its diff as code CI
  runs with this repo's permissions (`ci.yml`'s `id-token: write`), and check
  that it touches no workflow. Then:
  1. Open the commits as a same-repo PR.
  2. Close that PR unmerged: merging it would take the author's credit.

  Its checks share the fork PR's commit, so force-cancelling its other runs
  shows them cancelled on the fork PR too.

### Dispositions

Every finding reaches exactly one terminal state. A thread's disposition is
the reply that resolves it, and the reply starts with its state:

| State | Rule | Reply starts with |
| --- | --- | --- |
| FIXED | Fixed in this PR. | `FIXED: …` |
| REFUTED | Cite the mechanism, per AGENTS.md; add one where none exists. | `REFUTED: … — <mechanism>` |
| RE-SCOPED → #N | Real, introduced or first made reachable by this change, and bigger than the PR: split it off into #N. | `RE-SCOPED → #N: …` |
| FOLLOW-UP → #N | Real and reachable at the base, and not fixed in place: fixed in #N. | `FOLLOW-UP → #N: …` |

- **Reachable first.** Before adding code for a finding, establish that its
  case is reachable. When a test or sweep shows it isn't, that test is the
  mechanism, and no defensive code lands.
- **A redesign** that brings a RE-SCOPED finding into scope ends FIXED.
- **In place**, so FIXED rather than FOLLOW-UP, fits a small defect inside
  code this change already touches, pulling in no new must-run item. A breach
  of a written rule there is fixed in place whatever it pulls in (AGENTS.md's
  org rules: Refactor).
- **Another session's tree**: a FOLLOW-UP for a defect there cites that
  session's PR.
- **#N** is a PR other than this one: open, merged, or closed under
  [the open-PR cap](../AGENTS.md#workflow) with the fix on its branch.
- **A re-flag** of an already-dispositioned finding replies with the
  original's disposition, linked.
- "Acknowledged" and "surfaced" are not states.
- Sweep at the final merge head. Before re-litigating a bot's re-flag, check
  which commit it was raised against.

### Convergence contract

A **fold** is the one commit that takes a review round's accepted findings.

- **Churn budget**: split a diff whose added + modified lines exceed ~1500
  into stacked PRs before review.
  - Pure deletions are exempt once censused.
  - A change that both adds and deletes at scale is two PRs.
- **Deletion census**: before deleting N members of a class, land the full
  list and its criterion in the first commit or the PR body (#943).
- **Fixes are never put off.**
  - Each fold takes every accepted finding, whatever code it names.
  - A defect this change introduced is FIXED in it, since a clean-up left for
    later rarely lands
    ([Google](https://google.github.io/eng-practices/review/reviewer/pushback.html#cleaning-it-up-later)).
  - The loop converges because a re-review judges only the PR's own change
    since the last reviewed head (the
    [shared review](https://github.com/pixtuoid/.github/blob/main/.github/workflows/claude-review.yml)'s
    scope), so a fold's findings are the fold's.
  - A blocking issue confirmed in a fold stops the loop: revert the fold and
    re-land smaller, or re-scope.
- **A fold's behavior change ships a test that fails without it.** A
  user-visible string is a behavior change, a doc comment clap renders as
  `--help` included.
  - The rest of a fold is a revert, a deletion, a comment or doc change, or a
    refactor the existing tests cover.
  - Anything else reverts the fold.
- **A fix round adds no gate** beyond a refutation's mechanism. Any other
  wanted check is its own PR, asserting facts in its own layer: a Rust fact
  from Rust, never a Python regex over `.rs`.

## Adding a new agent CLI

No test forces steps 1–3; they are on you. A check fails without each later
step, and the step names it.

1. **Verify the wire format against the CLI's actual source and releases
   first**: transcript location, line shape, hooks, session identity. Pin
   every fact to an upstream file and version.
   - **Audit its home resolver per axis in the same pass.** Probe the
     installed artifact rather than trusting docs. An unmirrored axis fails
     silently: the watcher polls a directory the CLI never writes, and the
     office stays empty (#880).
   - No tier checks an axis past the home variable a hermetic profile
     sandboxes, so re-probe it when the CLI ships a new major version.
   - A custom root gets one `pub fn <cli>_home()`, called by both the
     watcher's `default_paths()` and the installer's `default_config_path()`
     so they can't disagree.
   - No test can ask the questions below, so the PR body answers each with an
     upstream cite:
     - **Where can hooks live?** A plugin or a dir the CLI scans keeps
       pixtuoid out of the user's file (step 8). Name each route checked; a
       merge's `no_owned_route` cites why none runs unattended.
     - **What rewrites that file?** A CLI that saves its whole config can drop
       a sentinel, so ownership keys on what survives (`install/jcode.rs`'s
       `managed_path`).
     - **Which events fire for a session nobody sees?** A resume, a reload or
       a per-connection placeholder: each is a phantom sprite if it registers
       (`source/jcode.rs`'s `turn_start` arm).
     - **How does a hook get its payload?** When not on stdin, the shim needs
       an arm and `shim_stamp_parity.rs` a pin, and it forwards only what a
       decoder reads.
2. **Write the source module**, `crates/pixtuoid-core/src/source/<name>.rs`:
   `SOURCE_NAME`, a `LineDecoder` fn (one JSONL line → `Vec<AgentEvent>`), a
   label deriver, and unit tests for mappings no fixture reaches. Format
   knowledge lives here.
3. **Implement the `Source` trait**: an async `run(self, tx)` that watches and
   decodes until the session universe ends.
   - **Hook-only CLI?** Skip the decoder, the trait and step 7. Put
     `transcript: None` in the registry row and the format knowledge in a
     `HookDecoding::ClaimsAll` decoder, and do step 8 instead.
4. **Add the module's `DESCRIPTOR`** to `source/registry.rs`'s `REGISTRY`.
   `just test` fails without it.
   - `display_name`, as upstream's README writes it.
   - The label prefix, the decoder, and `HookDecoding`. Verify a `Shared`
     row's `tool_id_key` against a captured tool call: kimi's `ToolCall` cost
     a source its tool ids.
   - Truthful capability flags. Lifecycle policy derives from them; you do
     **not** edit the reducer.
   - `verified_version` and `version_probe`.
5. **The descriptor's `name` is the roster**: `registered_source_names()`
   projects `REGISTRY`. `just test` then requires:
   - a fixture, by the conformance suite;
   - a `PROFILES` row, or a `NOT_HERMETIC` reason and feed, by
     `harness/hermetic/mod.rs`;
   - the name in the `sources --json` golden
     (`crates/pixtuoid/tests/snapshots/cli/sources.json`):
     `SNAPSHOTS=overwrite just test -p pixtuoid --test cli_json`.
6. **Record the fixture and add the tests**:
   [the test steps](../crates/pixtuoid-core/tests/AGENTS.md#adding-a-new-agent-cli--the-test-steps).
   The conformance suite fails without it.
7. **Wire it into `runtime/driver.rs::build_source_set`**, the one
   construction site. The registry drives the guard test, not the spawning,
   and `just test` fails without it.
8. **If the CLI has hooks, add an `install/` target**: a `Target` row whose
   `Primary` is a plugin file pixtuoid owns whole or, only when no such route
   runs unattended, the CLI's config (`merge_install`/`merge_uninstall` and a
   `verify_schema` fn mirroring its format). Add the
   registered-events↔decoder-arms guard too. `just test` fails without this
   step for a hook-only source only.
9. **Add a row to `site/src/sources.json`**, with the fields its neighbours
   carry (`name` is the `display_name`). Add that name to the bug form's
   `agent-cli` dropdown, then run `just gen-readme`.
   - `supported_sources_manifest.rs` pins the row to the registry
     (`just test`).
   - The `bug-form-lists-every-supported-cli` contract pins the dropdown to
     the rows (`just ci-observability`).
10. **Add the per-source badge hue**: a `SourceColors` field, its value in
    every theme file, and `badge_color` in the manifest row. The coverage,
    legibility and site-bridge tests fail until it exists.
11. **Add the roster literal in a second test binary**: a case row in
    `crates/pixtuoid/tests/wire_to_pixels.rs`'s `agent_cases()`. `just test`
    fails without it, but a scoped run misses it.

## License

By contributing, you agree your contributions are licensed under the same terms
as the project.
