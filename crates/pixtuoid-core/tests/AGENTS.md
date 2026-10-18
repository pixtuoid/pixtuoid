# pixtuoid-core/tests — agent guide

Integration tests organized **by capability/layer**; the per-CLI dimension
lives where the actual variation is — the source fixtures. Each top-level
`tests/*.rs` or `tests/<area>/main.rs` is one test binary; a multi-file area
MUST be `<area>/main.rs`, because a top-level `<area>.rs` is a crate root whose
`mod foo;` resolves to a SIBLING `tests/foo.rs`:

```
tests/
├── sources/main.rs      the source/decode layer
│   ├── captures.rs      THE walk (`every_capture()`) + every provenance RULE, in Rust
│   │                    so the rules ride `just test` on all three platforms. ONE
│   │                    enumeration, no mirror: with two populations a fix lands
│   │                    on one and misses the other. `conformance.rs` imports its
│   │                    tree helpers, so dropping `mod captures;` fails to COMPILE.
│   ├── decode/          cross-CLI decoder unit tests; its fixtures/{hooks,jsonl}/ are
│   │                    hand-built decoder inputs, NOT captures (`harness::captures` skips it)
│   ├── conformance.rs   per-source SessionStart→tool snapshot harness (insta), a
│   │                    `harness::Drive` shell; also pins first-sight seed ↔ decoder keying
│   ├── manager.rs       SourceManager spawn/health
│   ├── codewhale/       subagent lifecycle, with its OWN single-owner
│   │                    fixtures/hook-payloads.jsonl (NOT scanned)
│   ├── claude/ codex/ cursor/ grok/ opencode/ reasonix/  DELEGATING captures — a child is
│   │                    an independent session (two sprites), so these can't live under the one-AgentId rule
│   ├── copilot/         the refusal endings (`-p` auto-deny, Esc cancel), each resolving its wait
│   ├── omp/             the bridge-extension rounds (#951) — hook keys FOLD
│   │                    platform-dependently, so expectations derive through the
│   │                    fold, not the platform-invariant conformance goldens
│   ├── delegation/      the NAME-KEYED family (a tool literally called `task` — `subagent`
│   │                    for dsh): copilot, omp, dsh — one rule, one table
│   ├── hermetic/        `harness::hermetic`'s aimock lock and per-CLI scripts: data, no module
│   ├── snapshots/       insta snaps (sources__conformance__<source>__<scenario>)
│   └── fixtures/<source>/<scenario>/  conformance scenarios ONLY — dir name MUST be a
│                        registered source; provenance.json REQUIRED (fixtures/README.md)
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
├── transport/main.rs    #[cfg(unix)] socket / #[cfg(windows)] pipe
├── render/main.rs       blit + format (+ sprite fixtures)
└── socket_path_parity.rs · supported_sources_manifest.rs ·
    proof_fixture_disjointness.rs · pinned_by_claims.rs
                         FLAT + publish-excluded (see Cargo.toml's `exclude`; `sources/` is excluded whole)
```

Data scopes to the binary that reads it — a module-owned fixture lives with
its module. One cross-crate reader: `pixtuoid/tests/wire_to_pixels.rs` roots
at `sources/` (not `sources/fixtures/`) because it needs the two-sprite
captures the one-AgentId rule cannot host.

## The one pipeline

Real wire bytes ride `pixtuoid_core::harness::Drive` (dev-only `harness`
feature): `conformance.rs`, `sources/{grok,cursor,opencode,copilot,delegation}`,
`pixtuoid/tests/wire_to_pixels.rs`, and the on-demand
tools (`decoder_fuzz`, `corpus_check`). A shell supplies bytes and asserts;
it does NOT re-roll decode→reduce and never re-derives the first-sight seed's
`AgentId` (that comes from the source's row in `source/registry.rs`; a driver
keyed off anything else registers NOTHING).
`benches/decode_reduce.rs` SYNTHESIZES its lines — a bench-shaped fixture
under `sources/fixtures/` would be mis-scanned.

## Adding a new agent CLI — the test steps

1. **Always:** `tests/sources/fixtures/<registered-source>/<scenario>/` — at
   minimum a RECORDED SessionStart scenario (`just capture-fixture`) with the
   `provenance.json` the recorder writes; a hook-only source's first recorded
   scenario also drops its `NO_WIRE_EVIDENCE_YET` entry. `conformance.rs`
   auto-discovers the dir; `supported_sources_manifest` forces the manifest
   row; `cargo insta review` accepts the snapshot. Dir name = registered
   source name (`claude-code`, not `claude`); transcript-bearing fixtures are
   driven WITH the first-sight seed, so a wrong `id_from_path` fails here
   instead of shipping two sprites for one session.
2. **Always:** the two roster literals in
   [CONTRIBUTING's add-a-CLI checklist](../../../docs/CONTRIBUTING.md#adding-a-new-agent-cli)
   (its last step).
3. **Only for unique behavior** (subagent hooks, custom lifecycle): a
   `tests/sources/<cli>/` module registered in `sources/main.rs`. Plain CLIs
   (antigravity) need none.
