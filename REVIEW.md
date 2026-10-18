# Review rules

Read [`AGENTS.md`](AGENTS.md), and the nested `AGENTS.md` of each directory the
diff touches, first; these rules add to generic defect hunting.

## Scope

The org's [shared review](https://github.com/pixtuoid/.github/blob/main/.github/workflows/claude-review.yml)
runs a correctness and a design reviewer, each over this file but the other's
section, so [Correctness](#correctness) and [Design](#design) keep those
headings. Both read each changed file whole at the head (a removed file has
only its hunks), against the base tree: a defect anywhere in it is in scope,
pre-existing when the diff did not introduce it. The review only reads, so it
never attempts a [must-run](#must-run) item, and skipping one is not partial
coverage; its summary names each must-run item the diff touches.

## Do not flag

- Anything a [CI gate](.github/workflows/ci.yml) or clippy/rustfmt
  enforces, compiling included; pure style.
- Behavior documented where it is constrained: read the item's doc comment and
  the comments on the lines it governs first.
- Speculative defense in depth where a primary defense holds. Every layer
  [Traps](#traps-the-obvious-reading-is-wrong-here) or [Security](#security)
  names is primary, and a concrete, reachable extra-layer gap at a trust
  boundary is non-blocking.
- Risks needing unlikely or unreachable preconditions; performance unless
  measurable (the TUI repaints at `anim::PAINT_FPS`).
- A missing comment (the comment checks below own the rest).

## Lenses

### Correctness

Hold the diff to each claim the PR body makes, flagging scope beyond what it
states and new behavior without a test, and a fix to its REPORTED scenario,
ordering included, never one rebuilt from the state the fix left. Name any
CI-only gate the diff can turn red (`--lib` builds neither bin modules nor
examples).

- **Siblings** — a guard/cap/validation added to some of a sibling set
  (per-source decoders, install targets, platform arms, twin call sites) or to
  one caller of a shared fn: `rg` and verify the full set and every call site,
  and, since a guard protects an OUTCOME, every ordering when it is
  timing-dependent (boot-then-write vs write-then-boot); each is guarded,
  guarded elsewhere, or exempt with its reason (`emit_first_sight`'s second
  caller left the first-sight gate dead).
- **Wire format** — a decoder or drift watch vs the recorded upstream
  shape; every name a decoder reads owes a drift watch, a breadcrumb, or an
  adjudicated `EXEMPT` row (`drift_watch`'s census); `just drift-watch`
  runs the watches.
- **Manifest bridge** — `site/src/*.json` and generated schemas vs their Rust
  source of truth.
- **Unwired additions** — every new field/flag/parameter/asset/gate has a live
  consumer in the same diff (`_x` bindings and `pub` fields evade the lints).
- **Population** — a gate, build flag or config key that selects a set
  (crates, features, targets, jobs, a gate's own tests): name the set before
  and after; a silent shrink is a defect (#1012, #1101, #1103, #1123).
- **Test teeth** — every new or changed test, every test a changed file holds,
  and every test asserting an effect the diff removes or reroutes, fails when
  the code it names is wrong. Refusal paths are pinned on both sides of every
  window, offsets derived from the constant under test; a new gate fires on the
  violation, stays silent on the legitimate case, and names the real
  requirement. No test reads ambient state: the local time zone, `HOME`, the
  developer's config, a fixed temp path (#1023, #1048).
- **Reducer, liveness, sweeps** (`crates/pixtuoid-core/src/state/`,
  `source/jsonl/liveness.rs`, `source/exit_watch.rs`, `source/daemon.rs`) —
  trace the downstream interaction graph (rebind, sweeps, TTLs, cascade,
  dedup, polarity) and the provenance of every newly keyed signal.
- **Domain math**: geometry, sky, lighting (`crates/pixtuoid-scene/src/`
  `sky/`, `celestial.rs`, `lighting/`, `layout/`) — enumerate the domain
  invariants and re-derive each across the parameter space, edges included
  (#471, #1049, #1053).
- **A refactor**: a `refactor` PR, or a move or dedup across modules —
  adversarial toward revert, per consolidation: one reason-to-change per call
  site; name the conversions that moved semantics, since a batch hides exactly
  one (#461). No test leaves the suite (a `#[test]` removed or renamed, a
  `mod` dropped, a new `cfg` gate, a `[[test]]` target removed) unless the PR
  body names it.
- **CI, the merge gate or release tooling** (`.github/workflows/`,
  `.github/actions/`, `.github/dependabot.yml`, `.mergify.yml`, `policy/`,
  `release-plz.toml`, `justfile`, `REVIEW.md`; a Dependabot `uses:` pin bump
  aside) — name every check, contract or rule the diff loosens; trace each
  trigger (fork, bot actor, cancelled or superseded run) to a gate that fails
  closed.
- **Another CLI's config** (`crates/pixtuoid/src/install/`) — enumerate every
  resolution axis and re-verify each against that CLI's upstream (read here
  when it is on GitHub, a must-run fetch when not); write ⊆ verify (#338).

#### Traps (the obvious reading is wrong here)

- **Creation polarity** — only a proof-of-LIFE event creates or resurrects an
  entry; a death/exit/TTL signal for an absent id no-ops.
- **Lifecycle authority** — user/model-controllable content (transcript text,
  message bodies, tool args) never drives a state transition; structural
  markers and liveness signals only.
- **Upgrade path** — state a released version wrote survives; a fresh-install
  assumption that wipes an upgrader's config is blocking (#457).
- **Env** — a set-but-empty var reads as unset (`platform::path_env`).
- **Char-safe truncation** — user-visible text cuts on char/grapheme
  boundaries, never bytes.
- **Terminal egress** strips Cc controls and Cf bidi overrides (Trojan Source).
- **Denylists** enumerate the platform's documented set, never memory; an
  allowlist is preferred.

#### Security

A trust boundary is the hook shim or socket/named-pipe transport; config
writes or install targets; path, home, process, permission or credential
handling; transcript, hook, JSONL, pack or asset ingestion. When the diff
touches none, the summary says so. Otherwise report only a concrete attack or
invariant-breaking sequence against:

1. The shim and config writes: AGENTS.md invariants 4–5 and `install/io.rs`'s
   lock, atomic-write and permission authority. A config write is atomic and
   never destructive on any error/skip/default arm: existing-but-unparseable is
   never rewritten, a skip never strips pre-existing hooks. A shim change
   (`crates/pixtuoid-hook/`) audits the WHOLE shim for never-panic:
   `args_os()`, no slicing of untrusted bytes, bounded reads, every error path
   `exit(0)` (#198).
2. Socket / named pipe: no path traversal, symlink attack, unbounded read or
   unsafe ownership assumption. An endpoint is owner-only at creation
   (create-restricted-then-rename, never a process-global umask), and a
   pre-existing one is hostile. On the hook's daemon side
   (`crates/pixtuoid-core/src/source/hook/`), arbitration cannot steal a live
   owner's socket, both `unix.rs`/`windows.rs` arms hold (Windows runs only in
   CI, outside mutation testing), and each guard is PINNED.
3. Untrusted input (hook payloads, transcripts, JSONL, paths, pack data):
   sanitized where it enters, never at each use site; bounded, validated,
   skipped without panicking.
4. Credentials and subprocesses: no secret in a command or log, no untrusted
   code in a secret-bearing process, no argument spliced into a shell as code
   (justfile, script, workflow step: #1126).

### Design

1. Trace the two nearest consumers of every changed surface for
   contradiction; propose replacement text where you object.
2. New data shapes: name the identity/key-space; consolidate shared identity,
   not shared topic; verify join keys against real production constants,
   never test fixtures.
3. Layering: mechanism calls route through the designated orchestrator; a
   new `pub` seam on a lower layer names why the orchestrator can't own it.
4. **Depth**: the change deepens a module rather than adding a shallow one, a
   pass-through layer or a parallel path; deleting it would concentrate
   complexity, not just move it (#1660 closed for a shape no other item asked
   about).
5. **One way per problem**: a problem the repo already solves is solved that
   way (config writes through `install/io.rs`, a CLI through the `Source`
   seam, wire bytes through `harness::Drive`, logic in tested Rust rather than
   inline workflow shell, as #1660 showed); a second way is a finding naming
   the first.
6. **Proportion**, each with the cheaper alternative, non-blocking
   unless it breaches AGENTS.md: **over-engineering** (YAGNI), a branch,
   helper, parameter, type, fallback arm or test machinery serving no
   reachable case; **low ROI**, code, API change or coupling out of proportion
   to its payoff (an API made `Option` across three painters for a width
   almost never hit); **hard to maintain**, needless indirection or layers,
   special-case branches, one change smeared across many files; an invariant
   held by prose where a type or the one source of truth could make the
   failure impossible (#1142); a check outside [its own
   layer](docs/CONTRIBUTING.md#convergence-contract). A mechanism the diff
   extends is judged as if new against AGENTS.md's [upkeep
   rule](AGENTS.md#conventions); one that fails it draws "delete it".
   A hand-rolled mechanism for a need a maintained library, the platform or
   the upstream tool already meets (AGENTS.md's reuse rule) draws "reuse X",
   citing a file of X read this run; one not read is marked unverified.
   A defense [Traps](#traps-the-obvious-reading-is-wrong-here) or
   [Security](#security) names, and the documented liveness ladders, stay.
7. **Words**: list, one by one, every name the diff adds or changes
   (`pub(crate)` and modules included), every comment and doc line in a
   changed file, and every comment and doc elsewhere, `.github/` and `.claude/`
   included, that names, counts or describes what the diff renames, moves or
   changes; judge each on all three, and give the count judged. Each finding
   cites what the word breaks or the edit that would leave it wrong;
   non-blocking unless it breaches AGENTS.md.
   - **Faithful**: it says what is true at head; a behavior change renames the
     item and rewrites what describes it.
   - **Clear**: a reader without this PR's context reads it right. One concept,
     one name across crates and painters (`rg` before coining), and no name
     for two concepts; compass words are [screen-space](crates/pixtuoid-scene/AGENTS.md).
   - **Concise**: the shortest name that stays clear, in the domain's existing
     word and Rust's [naming guidelines](https://rust-lang.github.io/api-guidelines/naming.html),
     never a placeholder; a comment earns its place by AGENTS.md's comment
     rules.
8. **Sourced practice**: [AGENTS.md](AGENTS.md#conventions)'s fetched-claims
   rule; a claim with no fetched source, or one that contradicts it, breaches it.
9. **A new dependency or workspace edge**: each crate it adds, transitive ones
   included, is justified, and a new workspace edge follows
   [AGENTS.md](AGENTS.md#layout)'s crate DAG.

#### Every changed file

A maintenance defect's failure scenario is the edit that would leave a copy
wrong.

1. **DRY** — every new fn/type/helper/const gets a whole-tree search for an
   existing implementation, weighted by divergence risk; every value a new line
   reads is the authority its consumer uses, not a sibling's (#1042, #1155).
   Test fixtures stay inline.

## Must-run

The review reads; these need the change run, so the author runs each one the
change touches before the PR queues and writes its result into the PR body. A
result holds for the head it ran at: a later push that changes what it checked
runs it again, a merge of `main` or a comment-only change does not. The
[`local-review`](.claude/skills/local-review/SKILL.md) skill runs them.

- **Render and look**: generated art and clips (extract frames and READ them;
  census the money shot; a PR's media diff is `just gen-media-check`'s
  `target/gen-check-diff/` overlays); motion, pose, walk-leg
  (`crates/pixtuoid-scene/src/` `walk/`, `pose/`, `pathfind/`, `physics.rs`:
  the snapshot example, or a `just hermetic` run's scene log for lifecycle,
  #61); a string or layout a painter frames (the COMPOSED frame;
  string-equality tests are blind to framing).
- **Walk** an interactive TUI flow end-to-end: first run, failure branches, the
  no-CLI user (#359).
- **Drive and measure** a public rendered page (`site/src/`,
  `integrations/raycast/src/`): WCAG in every interactive state, mobile pan,
  no-JS (#455).
- **Probe** a reducer, liveness or sweep change: a test that drives the
  interleaving the change touches.
- **Run live, or replay hermetically**, a new source or hook integration
  (`source/registry.rs`), without capture-rig convenience flags; event shapes
  from canonical upstream docs, never a fork.
- **Fetch** another CLI's upstream that is not on GitHub, for its config's
  resolution axes.
- **`cargo tree -e normal`** for a new dependency.
