# pixtuoid-core/tests — agent guide

Integration tests are organized by capability or layer; the per-CLI dimension
lives where the variation is, in the source fixtures.

- Each top-level `tests/*.rs` or `tests/<area>/main.rs` is one test binary.
- Make a multi-file area `<area>/main.rs`: a top-level `<area>.rs` is a crate
  root, so its `mod foo;` resolves to a sibling `tests/foo.rs`.
- Data scopes to the source it describes: a CLI's captures share its home,
  whichever test reads them.

```
tests/
├── sources/main.rs      the source/decode layer
│   ├── captures.rs      the walk (`every_capture()`) and every corpus rule, in Rust
│   │                    so the rules ride `just test` on all three platforms. One
│   │                    enumeration, no mirror: with two populations a fix lands
│   │                    on one and misses the other. `conformance.rs` imports its
│   │                    tree helpers, so dropping `mod captures;` fails to compile.
│   ├── decode.rs        cross-CLI decoder unit tests; decode/fixtures/{hooks,jsonl}/ are
│   │                    hand-built decoder inputs, not captures (`harness::captures` skips it)
│   ├── conformance.rs   per-source SessionStart→tool snapshot harness (insta), a
│   │                    `harness::Drive` shell; also pins first-sight seed ↔ decoder keying
│   ├── manager.rs       SourceManager spawn/health
│   ├── merge.rs         a run's hook and transcript lines in every arrival order (`harness::merge`)
│   ├── <module>/        one home per registered source, named as its `source/` module:
│   │                    fixtures/<scenario>/ (provenance.json required; rules in sources/README.md),
│   │                    snapshots/, hermetic/ (its aimock scripts and seeds), screens/
│   │                    (its TUI runs); conformance skips `harness::OUTSIDE_CONFORMANCE`
│   ├── <module>.rs      a source's unique behavior, where it has one; among them reasonix
│   │                    (delegating runs, two sprites), copilot (refusal endings, a failed
│   │                    child's own end), omp (bridge rounds whose hook keys go through
│   │                    `normalize_path_key`, which differs per OS, #951), kimi (a turn
│   │                    end registers its session)
│   ├── delegation.rs    the name-keyed family (a tool literally called `task`, or `subagent`
│   │                    for dsh and opencode): copilot, omp, dsh, opencode — one rule, one table
│   └── hermetic/aimock/ the scripted model's npm lock, shared by every profile
├── reducer/main.rs      state-machine behavior; shared builders + apply-DSL in main.rs
│                        (lifecycle · activity · tasks · liveness · display ·
│                        child_ledger · today · snapshot.rs, the full-scene insta golden #279)
├── watcher/main.rs      JsonlWatcher behavior; the poll-seam harness in main.rs
│                        (tailing · first_sight · liveness · reregister · attach ·
│                        wait, the waits shared with native_watch.rs ·
│                        sources.rs: every transcript-bearing source binds and spawns,
│                        keep a test for each (#828), plus a fixture through the Reducer ·
│                        today.rs: the day's count per source through the real
│                        `Source::run`, which `harness::Drive`, with no tailer, can't see)
├── native_watch.rs      tests needing the native watch backend: their own process, so
│                        `watcher`'s process-wide `fast_watch()` polling can't reach them
├── transport/main.rs    relay (both) · #[cfg(unix)] socket / #[cfg(windows)] pipe
└── socket_path_parity.rs · supported_sources_manifest.rs · pinned_by_claims.rs ·
    lang_gates.rs · shim_stamp_parity.rs · pair_link_site.rs · published_package.rs
                         flat + publish-excluded (see Cargo.toml's `exclude`; `sources/` is excluded whole)
```

## The one pipeline

Real wire bytes ride `pixtuoid_core::harness::Drive` (the dev-only `harness`
feature): every `tests/sources/` shell over captured bytes,
`pixtuoid/tests/wire_to_pixels.rs`, `fuzz/fuzz_targets/`, and the on-demand
`corpus_check`. A shell:

- supplies lines, reordered or cut, never decoded events, over time through a
  `harness::Fold`, and asserts;
- does not re-roll decode→reduce;
- never re-derives the first-sight seed's `AgentId`, which comes from the
  source's `DESCRIPTOR`: a driver keyed off anything else registers nothing.

## Adding a new agent CLI — the test steps

These are step 6 of
[CONTRIBUTING's add-a-CLI checklist](../../../docs/CONTRIBUTING.md#adding-a-new-agent-cli),
which holds every other step.

1. **Always: a recorded SessionStart scenario** in
   `tests/sources/<module>/fixtures/<scenario>/`, with the `provenance.json`
   the recorder writes. Record it with `just hermetic --record <source>`, else
   the billed `just capture-fixture <source> <scenario> <command…>`.
   - Name the dir after the source's `source/` module (`claude_code`, not
     `claude`).
   - `conformance.rs` auto-discovers the dir, and `cargo insta review` accepts
     the snapshot.
   - A transcript-bearing fixture is driven with the first-sight seed, so a
     wrong `id_from_path` fails here instead of shipping two sprites for one
     session.
2. **Only for unique behavior** (subagent hooks, custom lifecycle): a
   `tests/sources/<module>.rs`, registered in `sources/main.rs`. A plain CLI
   (antigravity) needs none.
