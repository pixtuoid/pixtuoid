# Source decode fixtures

Golden fixtures for the per-CLI decode + hook↔JSONL **coalescing** contract,
driven by `tests/sources/conformance.rs`.

**Record these; do not compose them.** `just hermetic --record <source>` records its profile's
scenarios; `just capture-fixture <source> <scenario> <cmd...>` the rest, profiled CLIs' billed captures too.

```text
just capture-fixture kimi tool-run kimi -p '{prompt}'
just capture-fixture openclaw lifecycle scripts/lib/capture-openclaw-lifecycle.sh '{prompt}'
```

A hand-written fixture pins whatever its author believed the wire looked like, and that belief is what the fixture then teaches every later reader.
The composed `cursor/tool-run` this replaced carried no `tool_use_id` and
strictly sequential tools; the capture that replaced it shows an id on every
`preToolUse` and tools that INTERLEAVE (#901 discovered the same gap the
expensive way). kimi's composed fixture went further and asserted the decoder's
own wrong field name back at it.

**Every scenario declares its own provenance**, in `provenance.json` beside the
payloads, because nothing IN the bytes separates a capture from a composition — a
redacted cwd and an invented one look alike. `origin` is one of:

| origin | required | means |
| --- | --- | --- |
| `recorded` | `cli`, `version`, `captured`, `command` | real wire bytes — a real CLI's against a scripted model where `aimock` and `script` name it — stripped of every subtree no decoder reads — unless `deidentified.method` is `none`, a reviewed per-capture exemption with its reason for a test that reads raw bytes the decoder drops; the recorder writes this file itself |
| `composed` | `note` | hand-written, and the note says how you can tell |
| `unknown` | `note` | predates the rule; nobody recorded where it came from |

**The recorder blanks what no decoder reads before the bytes land**, and
`deidentified` in the provenance says how many subtrees went. A plugin roster, a
global-instruction attachment, a stray home path all ride fields the decoders
never touch, so they leave without anyone naming them, and what stays decodes to
the same events. The allowlist is derived by probing the decoders, never listed,
so it cannot drift from them. `/Users/dev` remains the convention for a home
path a decoder DOES read, and a capture redacted by hand says so in its `note`:
without it a reader cannot tell an edited capture from an untouched one, which is
the distinction the whole mechanism exists to make. A field blanked today stays
blank in that fixture: a decoder later extended to read it meets the neutral value
there and its golden encodes "absent" — re-record the scenario when a decoder
grows a read. The other direction is cheap: `just restrip-fixtures` runs the same
strip over the committed corpus, offline and unbilled, so a field a decoder
STOPPED reading is one pass from gone. A capture that exists to pin a wire
premise the decoder drops on purpose — a test reads it off the raw bytes, which
no probe can see — declares `"deidentified": {"method": "none", "why": …}` and
both paths leave it alone; the gate refuses the exemption without its reason.

The table above is not the schema — [`provenance.schema.json`](provenance.schema.json)
is, and both readers (this table and
`every_capture_declares_a_valid_origin_with_its_required_fields`) read it. They
each carried their own copy until #929, and had drifted apart.

`every_capture_declares_a_valid_origin_with_its_required_fields` enforces the schema, and its
`NO_WIRE_EVIDENCE_YET` list — the hook-only sources with no recorded scenario at
all — only shrinks: recording one forces its entry out, and a new hook-only CLI
cannot join by default.

**The recorder is `pixtuoid-core/examples/capture_fixture.rs`** — Rust rather
than the shell script it replaces, for the reasons under "Why the recorder is a
Rust example" below.

**It records at the shim's OUTPUT**, via a `PIXTUOID_SOCKET` listener,
which is the seam that does not care how the payload reached the shim: codewhale
passes identity in `DEEPSEEK_*` env vars and the shim never reads stdin for it, so
recording the INPUT would have captured a file of empty payloads. Two consequences
for the bytes: a capture carries production's own `_shim_ts_ms` / `_pid` stamps
(fixtures older than this seam do not), and the CLI's installed config is never
touched — the hook it already has is the one that runs, so a DISCONNECTED source
is refused before the turn is spent rather than after.

**A gate needs its own sandbox and its own prompt.** `CAPTURE_SEED=<dir>` is
copied into the sandbox workspace before the run — the place a per-CLI ask rule
belongs, since CC only FIRES
`PermissionRequest` when a project rule makes the tool ask. `CAPTURE_PROMPT` replaces the shared prompt for
a scenario it cannot reach. Neither touches the user's own config.

**A module's own scenarios re-record in place.** The recorder writes to
`<source>/fixtures/<scenario>/` when a committed `provenance.json` is already
there (omp's bridge rounds), else to the conformance root — where a new
directory would be auto-scanned by `conformance.rs` with no committed bytes
beside it for the `.new` no-clobber rule to protect.

**Re-recording an existing scenario needs a hand** unless the tier records it:
`just hermetic --record` replaces a fixture's bytes whole, and git holds the diff.
`just capture-fixture` never overwrites committed bytes: it writes `<name>.new`
beside them to be diffed, not trusted, and since only `*.jsonl` and
`provenance.json` are read, one you do not promote costs a billed turn and changes
nothing. Diff the pair, then `mv` the `.new` over the old one or delete it.

**A capture is recorded only on Unix.** The corpus pins Unix realpaths and cwds,
so on Windows the recorder writes only a raw `--out` capture and
refuses to record or promote. Windows wire evidence therefore cannot be
re-recorded on demand — which is why `copilot/tool-run`, the one fixture
carrying a real Windows `cwd`, stays `unknown` rather than being replaced.

Only ONE edit is allowed after the recorder's own strip: redact PII the strip
left behind. Anything else and the capture stops being evidence. PII has
arrived as cursor's `user_email` key, as the owner column inside a captured
`ls -la` `tool_output` (kimi), and as codex's `world_state` inventory of every
skill on the host; the strip takes whatever rides a field no decoder reads, and
what it leaves sits inside a field one does — a `cwd`, a tool's command string —
so read a capture before committing it rather than trusting a key-name filter.
The recorder's `scan_for_pii` refuses the operator's OWN identity — `$HOME`/
`$USER`/`$LOGNAME` and the git name, the one set gitleaks cannot know — and
`just fixture-pii` re-scans the committed tree by value shape; neither reaches a
value it cannot name, which is why a capture still gets read. One invariant sits
over both, in `just test`: outside a capture pinned in `EXEMPT_FROM_STRIP`, no
identity key (`user_email`, `account_id`, `token`, …) may hold a value — the
strip guarantees it, and this asserts it on the bytes. Editing a fixture to put
one back reds that test; an exempt capture is where a human read is the only
check, which is why the exempt set is pinned as an exact list. No size heuristic can stand in for that read either:
legitimate copilot lines run to 39 KB, wider than the `world_state` dump that had
to go. Redacting a READ field is the expensive case — re-run the golden;
byte-identical means the redaction cost no evidence.

**Capture only what pixtuoid's OWN hook registration receives.** A machine can
carry other tools' hooks on the same events — a debug tee, another integration —
and a capture that scoops those up is a recording of somebody else's wire. Scope
it to the source's install-side list (`CURSOR_EVENTS` in `install/cursor.rs`, and
its siblings); an event outside that list never reaches the shim in production, so
a decoder that bails on it is correct and a fixture containing it is fiction.
Learned by getting it wrong: a capture off a 12-event debug tee looked like proof
that the decoder mishandled three events pixtuoid never even subscribes to.

The same trap has a second door: a CLI that SPAWNS another CLI. The OpenClaw
gateway runs its agent turn on a Claude Code backend, which inherits
`PIXTUOID_SOCKET` and sends its own unstamped CC hooks to the recorder — six of
the eight payloads in that capture were Claude Code's. Filter a capture to the
source's own stamp before committing it.

One list describes each CLI — `*_EVENTS` in `install/<cli>.rs`: which hooks we
register, and so what the CLI sends us.

An event the decoder handles but `*_EVENTS` never registers is a SHIPPING bug no
test can see, because every test asserts our own belief about the wire: CC's
permission gate is `PermissionRequest`, the decoder had read it for years, and
`install/claude.rs` registered four tool events without it — so a session parked
on a permission prompt rendered as working, indefinitely. Only a capture of a
gated run says which side is wrong.

Each fixture is a directory:

```
tests/sources/fixtures/<source>/<scenario>/
    <transcript>.jsonl     # JSONL transcript lines, fed to the source's LineDecoder
                           # (JSONL-bearing sources only — a hook-only row,
                           # transcript: None, ships NO transcript)
    hook-payloads.jsonl    # one hook payload per line, fed to decode_hook_payload
    # expected snapshot lives in tests/sources/snapshots/ (insta), generated on first run
```

A scenario ships the transports its source actually has: both files
(CC/Codex), transcript-only (antigravity — no hooks), or hook-payloads-only
(reasonix — hook-only, no watchable JSONL).

**A transcript whose id comes from its PARENT DIR must keep that dir.**
Flattened, the session id becomes the SCENARIO NAME, and
the hook-vs-JSONL coalesce assertion then compares a real id against a directory
name — `grok/permission-flow` passed for months only because its hooks were
composed to declare `sessionId: "permission-flow"`. So a recorded grok scenario
nests: `<scenario>/<session-uuid>/updates.jsonl`. The recorder keeps the fewest
parent dirs that leave the registry's own `id_deriver_for` id unchanged, and the
harness recurses to find it.

(This tree is **conformance-scanned ONLY** — `conformance.rs` asserts every dir
here is a registered source. Single-owner fixtures read by one module — decode's
`sources/decode/fixtures/`, codex's `sources/codex/fixtures/`, render's
`render/fixtures/` — live with their module, NOT here. See
[`tests/AGENTS.md`](../../AGENTS.md) for the governing principle.)

The harness, for each fixture dir:
1. decodes the transcript lines (via the source's `LineDecoder`) and the hook
   payloads (via `decode_hook_payload`),
2. snapshots the full decoded `AgentEvent` sequence (`insta`),
3. **asserts every decoded event shares ONE `AgentId`** — the coalescing
   contract. This is the bug class that keeps biting (hook and JSONL keying a
   session differently → two sprites).

`{{TRANSCRIPT_PATH}}` in a hook payload's `transcript_path` is replaced at
runtime with the fixture's transcript file path (for a hook-only scenario: the
scenario dir's relative path), so a CC hook (which coalesces on
`transcript_path`) lines up with its JSONL file. Codex carries it too — to
prove Codex *ignores* it and still coalesces on `session_id`.

**Adding a CLI:** [the test steps](../../AGENTS.md#adding-a-new-agent-cli--the-test-steps).

## Where a scenario's own story lives

Each scenario declares itself in its `provenance.json` `note`, the single
place, beside the bytes it describes: a catalogue of directories here would rot
the moment one is re-recorded under a better name.

Two scenarios carry a fact BEYOND their own decode, so they are named here where
someone changing the tree will see it:

- **`claude-code/proof-session/`** is read at RENDER time by the `snapshot
  --proof` site-media renderer (`scripts/media.json` job `proof`) — its
  timestamps ARE the clip's beat timings, so retiming this fixture re-times the
  committed `proof.webm`. Its visible strings are pinned disjoint from the
  statusline ticker's own FALLBACK corpus by `tests/proof_fixture_disjointness.rs`.
- **`copilot/tool-run/`** is the only fixture carrying a real Windows `cwd` (a
  path with a space and parens), which makes it the Windows arm of
  `registry_cwd_extractor_matches_each_sources_real_head_shape`. A macOS capture
  cannot reproduce it, which is why it stays `unknown` rather than being
  re-recorded.

## Why the recorder is a Rust example

`just capture-fixture` drives `pixtuoid-core/examples/capture_fixture.rs`. It
was a shell script first, and every defect that version accumulated was bash
semantics rather than recording logic: an empty array under `set -u`
(which stopped the CLI launching for anyone outside an agent session),
`pipefail` swallowing a lister's exit 3 after the turn was billed, `head -1`
taking SIGPIPE once a corpus passed ~700 files (same failure, same place, found
later), `stat -f %B` being macOS-only, and three world-writable temp paths.

None of them could fail in a selftest small enough to run in CI — which is the
argument, not the count. In Rust they are absent by construction: no arrays, no
pipelines, `Metadata::created()` for birth time, `TempDir` for a private 0700
sandbox — and the decisions are ordinary `#[test]`s that `just test` already
runs, rather than a bespoke `--selftest` wired into two gates.

That last clause is only true because `Cargo.toml` declares `[[example]]
test = true`. Without it an example is not a test target: the tests compile and
never run, which is how they shipped at first.
