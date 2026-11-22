# Review rules

Read [`AGENTS.md`](AGENTS.md) first, then the nested `AGENTS.md` of each
directory the diff touches. These rules add to generic defect hunting.

## Scope

The org's [CI review](https://github.com/pixtuoid/.github/blob/main/.github/workflows/claude-review.yml)
runs two reviewers over this file, each with its contract's lenses:

- the correctness reviewer applies all of it but [Design](#design);
- the design reviewer applies all of it but [Correctness](#correctness).

The workflow finds those sections by their headings, so keep them.

The review only reads, so it never attempts a [must-run](#must-run) item, and
skipping one is not partial coverage.

## Do not flag

- Anything a [CI gate](.github/workflows/ci.yml) or clippy/rustfmt enforces,
  compiling included.
- Pure style.
- Behavior documented where it is constrained. Read the item's doc comment and
  the comments on the lines it governs first.
- Speculative defense in depth where a primary defense holds.
  - Every layer [Traps](#traps-the-obvious-reading-is-wrong-here) or
    [Security](#security) names is primary.
  - A concrete, reachable gap in an extra layer at a trust boundary is
    non-blocking.
- Risks that need unlikely or unreachable preconditions.
- Performance, unless measurable (the TUI repaints at `anim::PAINT_FPS`).
- A missing comment. The comment checks below own the rest.

## Lenses

### Correctness

- Flag new behavior without a test.
- Judge a fix against its reported scenario, ordering included, never against
  one rebuilt from the state the fix left.
- Name any CI-only gate the diff can turn red. `--lib` builds neither bin
  modules nor examples.
- **Siblings**: a guard, cap or validation added to some of a sibling set
  (per-source decoders, install targets, platform arms, twin call sites), or
  to one caller of a shared fn.
  - `rg` for the full set and every call site, and verify each.
  - A guard protects an outcome, so when it is timing-dependent, verify every
    ordering too (boot-then-write and write-then-boot).
  - Each member is guarded, guarded elsewhere, or exempt with its reason.
    #823's first-sight gate left `emit_registration`'s second caller
    unguarded.
- **Wire format**: check a decoder against the recorded upstream shape. Every
  literal a decoder reads is in a scenario's fixture, owed by its profile
  (`not_yet*`), or exempt with its reason (`hermetic::literals`'s census).
- **Manifest bridge**: check `site/src/*.json` and the generated schemas
  against their Rust source of truth.
- **Wiring** (the contract's lens), here: new flags and assets too; `_x`
  bindings and `pub` fields evade the lints.
- **Population**: for a gate, build flag or config key that selects a set
  (crates, features, targets, jobs, a gate's own tests), name the set before
  and after. A silent shrink is a defect (#1012, #1101, #1103, #1123).
- **Test teeth**:
  - Every test in what the review reads, and every test asserting an effect
    the diff removes or reroutes, fails when the code it names is wrong.
  - Refusal paths are pinned on both sides of every window, with offsets
    derived from the constant under test.
  - A new gate fires on the violation, is silent on the legitimate case, and
    names the real requirement.
  - No verdict turns on the time zone, the date or the time of day.
  - No test reads other ambient state: `HOME`, a developer's config, a fixed
    temp path (#1023, #1048).
- **Reducer, liveness, sweeps** (`crates/pixtuoid-core/src/state/`,
  `source/jsonl/liveness.rs`, `source/exit_watch.rs`, `source/daemon.rs`):
  trace the downstream interaction graph (rebind, sweeps, TTLs, cascade,
  dedup, polarity) and the provenance of every newly keyed signal.
- **Domain math**: geometry, sky, lighting (under
  `crates/pixtuoid-scene/src/`: `sky/`, `celestial.rs`, `lighting/`,
  `layout/`). Enumerate the domain invariants and re-derive each across the
  parameter space, edges included (#471, #1049, #1053).
- **A refactor**: a `refactor` PR, or a move or dedup across modules. Review
  it adversarially, leaning toward revert:
  - per consolidation, one reason to change per call site;
  - name the conversions that moved semantics, since a batch hides exactly
    one (#461);
  - no test leaves the suite unless the PR body names it: a `#[test]` removed
    or renamed, a `mod` dropped, a new `cfg` gate, a `[[test]]` target
    removed.
- **CI, the merge gate or release tooling** (`.github/workflows/`,
  `.github/actions/`, `.github/dependabot.yml`, `.mergify.yml`, `policy/`,
  `release-plz.toml`, `justfile`, `REVIEW.md`; a Dependabot `uses:` pin bump
  aside):
  - name every check, contract or rule the diff loosens;
  - trace each trigger (fork, bot actor, cancelled or superseded run) to a
    gate that fails closed.
- **Another CLI's config** (`crates/pixtuoid/src/install/`): enumerate every
  resolution axis and re-verify each against that CLI's upstream. Read the
  upstream here when it is on GitHub; when not, it is a must-run Fetch. Write
  ⊆ verify (#338).

#### Traps (the obvious reading is wrong here)

- **Creation polarity**: only a proof-of-life event creates or resurrects an
  entry; a death, exit or TTL signal for an absent id no-ops.
- **Lifecycle authority**: user- or model-controllable content (transcript
  text, message bodies, tool args) never drives a state transition; only
  structural markers and liveness signals do.
- **Upgrade path**: state a released version wrote survives. A fresh-install
  assumption that wipes an upgrader's config is blocking (#457).
- **Env**: a set-but-empty var reads as unset (`platform::path_env`/`text_env`).
- **Char-safe truncation**: user-visible text cuts on char or grapheme
  boundaries, never bytes.
- **Terminal egress** strips Cc controls and Cf bidi overrides (Trojan Source).
- **Denylists** enumerate the platform's documented set, never memory; an
  allowlist is preferred.

#### Security

A trust boundary is any of:

- the hook shim, or the socket/named-pipe transport;
- the `serve` listener and its pairing store;
- config writes or install targets;
- path, home, process, permission or credential handling;
- transcript, hook, JSONL, pack or asset ingestion.

When the diff touches none, the summary says so. Otherwise report only a
concrete attack or invariant-breaking sequence against:

1. **The shim and config writes**: AGENTS.md invariants 4–5, and
   `install/io.rs`'s lock, atomic-write and permission authority.
   - A config write is atomic, and never destructive on any error, skip or
     default arm: an existing but unparseable file is never rewritten, and a
     skip never strips pre-existing hooks.
   - A shim change (`crates/pixtuoid-hook/`) audits the whole shim for
     never-panic: `args_os()`, no slicing of untrusted bytes, bounded reads,
     `exit(0)` on every error path (#198).
2. **Socket / named pipe**: no path traversal, symlink attack, unbounded read
   or unsafe ownership assumption.
   - An endpoint is owner-only at creation: create it restricted, then rename
     it, never through a process-global umask.
   - A pre-existing endpoint is hostile.
   - On the hook's daemon side (`crates/pixtuoid-core/src/source/hook/`):
     arbitration cannot steal a live owner's socket; both the `unix.rs` and
     `windows.rs` arms hold (Windows runs only in CI, outside mutation
     testing); and a test pins each guard.
3. **Untrusted input** (hook payloads, transcripts, JSONL, paths, pack data, a
   network peer's bytes): sanitized where it enters, never at each use site;
   bounded, validated, and skipped without panicking.
4. **Credentials and subprocesses**: no secret in a command or log, no
   untrusted code in a secret-bearing process, and no argument spliced into a
   shell as code (justfile, script, workflow step: #1126).

### Design

1. **Consumers**: trace the two nearest consumers of every changed surface for
   contradiction. Where you object, propose replacement text.
2. **New data shapes**: name the identity or key space. Consolidate shared
   identity, not shared topic. Verify join keys against real production
   constants, never test fixtures.
3. **Layering**: mechanism calls route through the designated orchestrator. A
   new `pub` seam on a lower layer names why the orchestrator can't own it.
4. **Depth**: the change deepens a module rather than adding a shallow one, a
   pass-through layer or a parallel path. Deleting it would concentrate
   complexity, not just move it (#1660 closed for a shape no other item asked
   about).
5. **One way per problem**: a problem the repo already solves is solved that
   way. A second way is a finding naming the first. The repo's ways include:
   - config writes through `install/io.rs`;
   - a CLI through the `Source` seam;
   - wire bytes through `harness::Drive`;
   - logic in tested Rust rather than inline workflow shell, as #1660 showed.
6. **Proportion** (the contract's lens), here:
   - an API made `Option` across three painters for a width almost never hit
     is low ROI;
   - an invariant held by prose where a type could make the failure
     impossible (#1142), and a check outside
     [its own layer](docs/CONTRIBUTING.md#convergence-contract), are hard to
     maintain;
   - the lens's "delete it" spares a defense
     [Traps](#traps-the-obvious-reading-is-wrong-here) or
     [Security](#security) names, and the documented liveness ladders.
7. **Words** (the contract's lens), here:
   - names include `pub(crate)` items and modules;
   - docs elsewhere include `.github/` and `.claude/`;
   - one concept keeps one name across crates and painters (`rg` before
     coining one);
   - compass words are [screen-space](crates/pixtuoid-scene/AGENTS.md);
   - the naming guidelines are
     [Rust's](https://rust-lang.github.io/api-guidelines/naming.html).
8. **Sourced practice** (the contract's lens), here: AGENTS.md's
   [Evidence](AGENTS.md#conventions) bullet.
9. **A new dependency or workspace edge**: each crate it adds, transitive ones
   included, is justified, and a new workspace edge follows
   [AGENTS.md](AGENTS.md#layout)'s crate DAG.
10. **One source** (the contract's lens; AGENTS.md's
    [DRY](AGENTS.md#conventions)), over everything the review reads. A
    maintenance defect's failure scenario is the edit that would leave a copy
    wrong. Here, the search is weighted by divergence risk (#1042, #1155), and
    test fixtures stay inline.

## Must-run

These items need the change run, which the review can't do. The author runs
each one the change touches before the PR queues, and writes its result into
the PR body.

- A result holds for the head it ran at. A later push that changes what it
  checked runs it again; a merge of `main` or a comment-only change does not.
- The org's local-review skill ([AGENTS.md](AGENTS.md#org-rules)) runs them.

The items:

- **Render and look**:
  - generated art and clips: extract frames and read them, and census each
    clip's money shot, the action it is staged for (`scripts/gen-media.py`'s
    `poster_frame`). A PR's media diff is the overlays `just gen-media-check`
    writes to `target/gen-check-diff/`;
  - motion, pose and walk legs (under `crates/pixtuoid-scene/src/`: `walk/`,
    `pose/`, `pathfind/`, `physics.rs`): the snapshot example, or for
    lifecycle a `just hermetic` run's scene log (#61);
  - a string or layout a painter frames: the composed frame, since
    string-equality tests are blind to framing.
- **Walk** an interactive TUI flow end to end: first run, failure branches, the
  user with no CLI (#359).
- **Drive and measure** a public rendered page (`site/src/`,
  `integrations/raycast/src/`): WCAG in every interactive state, mobile pan,
  no-JS (#455).
- **Probe** a reducer, liveness or sweep change: a test that drives the
  interleaving the change touches.
- **Run live, or replay hermetically**, a new source or hook integration (a
  source's `DESCRIPTOR`), without the capture rig's convenience flags. Event
  shapes come from canonical upstream docs, never a fork.
- **Fetch** another CLI's upstream that is not on GitHub, for its config's
  resolution axes.
- **`cargo tree -e normal`** for a new dependency.
