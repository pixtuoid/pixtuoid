# pixtuoid-core/tests — agent guide

Integration tests organized **by capability/layer**; the per-CLI dimension
lives where the actual variation is — the source fixtures. Each top-level
`tests/*.rs` or `tests/<area>/main.rs` is one test binary; a multi-file area
MUST be `<area>/main.rs`, because a top-level `<area>.rs` is a crate root whose
`mod foo;` resolves to a SIBLING `tests/foo.rs`:

```
tests/
├── sources/main.rs      the source/decode layer
│   ├── captures.rs      THE walk (`every_capture()`) + every corpus RULE, in Rust
│   │                    so the rules ride `just test` on all three platforms. ONE
│   │                    enumeration, no mirror: with two populations a fix lands
│   │                    on one and misses the other. `conformance.rs` imports its
│   │                    tree helpers, so dropping `mod captures;` fails to COMPILE.
│   ├── decode.rs        cross-CLI decoder unit tests; decode/fixtures/{hooks,jsonl}/ are
│   │                    hand-built decoder inputs, NOT captures (`harness::captures` skips it)
│   ├── conformance.rs   per-source SessionStart→tool snapshot harness (insta), a
│   │                    `harness::Drive` shell; also pins first-sight seed ↔ decoder keying
│   ├── manager.rs       SourceManager spawn/health
│   ├── merge.rs         a run's hook and transcript lines in every arrival order (`harness::merge`)
│   ├── <module>/        ONE home per registered source, named as its `source/` module:
│   │                    fixtures/<scenario>/ (provenance.json REQUIRED; rules in sources/README.md),
│   │                    snapshots/, hermetic/ (its aimock scripts and seeds), screens/
│   │                    (its TUI runs); conformance skips `harness::OUTSIDE_CONFORMANCE`
│   ├── <module>.rs      a source's unique behavior: claude_code codex codewhale cursor grok
│   │                    opencode reasonix (DELEGATING runs, two sprites), copilot (refusal
│   │                    endings, a failed child's own end), omp (bridge rounds whose hook
│   │                    keys FOLD per OS, #951)
│   ├── delegation.rs    the NAME-KEYED family (a tool literally called `task` — `subagent`
│   │                    for dsh and opencode): copilot, omp, dsh, opencode — one rule, one table
│   └── hermetic/aimock/ the scripted model's npm lock, shared by every profile
├── reducer/main.rs      state-machine behavior; shared builders + apply-DSL in main.rs
│                        (lifecycle · activity · tasks · liveness · display ·
│                        child_ledger · snapshot.rs, the full-scene insta golden #279)
├── watcher/main.rs      JsonlWatcher behavior; the poll-seam harness in main.rs
│                        (tailing · first_sight · liveness · unclaim · attach ·
│                        sources.rs — ALL SIX transcript sources bind+spawn, keep all
│                        six (#828), + the ONE fixture→Reducer fold: the only test that
│                        drives committed wire through a real Reducer)
├── native_watch.rs      tests needing the NATIVE watch backend: their own process, so
│                        `watcher`'s process-wide `fast_watch()` polling can't reach them
├── transport/main.rs    relay (both) · #[cfg(unix)] socket / #[cfg(windows)] pipe
├── render/main.rs       blit + format (+ sprite fixtures)
└── socket_path_parity.rs · supported_sources_manifest.rs · pinned_by_claims.rs ·
    lang_gates.rs · shim_stamp_parity.rs
                         FLAT + publish-excluded (see Cargo.toml's `exclude`; `sources/` is excluded whole)
```

Data scopes to the source it describes: a CLI's captures share its home,
whichever test reads them.

## The one pipeline

Real wire bytes ride `pixtuoid_core::harness::Drive` (dev-only `harness`
feature): `conformance.rs`, `merge.rs`, `sources/{grok,cursor,opencode,copilot,delegation}.rs`,
`pixtuoid/tests/wire_to_pixels.rs`, and the on-demand
`corpus_check`. A shell supplies bytes and asserts;
it does NOT re-roll decode→reduce and never re-derives the first-sight seed's
`AgentId` (that comes from the source's `DESCRIPTOR`; a driver
keyed off anything else registers NOTHING).
`benches/decode_reduce.rs` SYNTHESIZES its lines — a bench-shaped fixture
under a `sources/<module>/fixtures/` would be mis-scanned.

## Adding a new agent CLI — the test steps

1. **Always:** `tests/sources/<module>/fixtures/<scenario>/` — at
   minimum a RECORDED SessionStart scenario (`just capture-fixture`) with the
   `provenance.json` the recorder writes. `conformance.rs`
   auto-discovers the dir; `supported_sources_manifest` forces the manifest
   row; `cargo insta review` accepts the snapshot. Dir name = the source's
   `source/` module (`claude_code`, not `claude`); transcript-bearing fixtures are
   driven WITH the first-sight seed, so a wrong `id_from_path` fails here
   instead of shipping two sprites for one session.
2. **Always:** the roster literal in
   [CONTRIBUTING's add-a-CLI checklist](../../../docs/CONTRIBUTING.md#adding-a-new-agent-cli)
   (its last step).
3. **Only for unique behavior** (subagent hooks, custom lifecycle): a
   `tests/sources/<module>.rs` registered in `sources/main.rs`. Plain CLIs
   (antigravity) need none.
