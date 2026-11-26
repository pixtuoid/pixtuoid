# Source decode fixtures

The golden fixtures for each CLI's decoding, and for the **coalescing**
contract between its hooks and its JSONL transcript, driven by
`tests/sources/conformance.rs`.

## Record them, never compose them

`just hermetic --record <source>` records the scenarios its profile scripts.
`just capture-fixture <source> <scenario> <cmd...>` records the rest, a
profiled CLI's billed captures included:

```text
just capture-fixture kimi tool-run kimi -p '{prompt}'
```

A hand-written fixture pins what its author believed the wire looked like,
and then teaches that belief to every later reader:

- The composed `cursor/fixtures/tool-run` carried no `tool_use_id`. The
  capture that replaced it shows an id on every `preToolUse`, and
  `cursor/fixtures/delegation` shows tools that INTERLEAVE (#901 found the
  same gap the expensive way).
- kimi's composed fixture asserted the decoder's own wrong field name back at
  it.

## Provenance

Every scenario declares where it came from, in a `provenance.json` beside its
payloads: nothing in the bytes tells a capture from a composition, since a
redacted cwd and an invented one look alike. `origin` is `recorded`, and no
other value parses:

| origin | required | means |
| --- | --- | --- |
| `recorded` | `cli`, `version`, `captured`, `command`, `deidentified` | Real wire bytes, a real CLI's against a scripted model where `aimock` and `script` name it, stripped of every subtree no decoder reads; the recorder writes this file itself. |

The table is not the schema: [`Provenance`](../../src/harness/provenance.rs)
is. A record it doesn't parse fails every test that walks the captures, and
`the_fixtures_readme_states_each_origin` holds this table to it.

A scenario's own story is its `provenance.json`'s `note`, beside the bytes it
describes. A catalogue of directories here would rot the moment one is
re-recorded under a better name.

## What the recorder does

The recorder is `pixtuoid-core/examples/capture_fixture.rs` (why it is Rust:
[below](#why-the-recorder-is-a-rust-example)). Its checks hold for what it
records into the tree; a raw `--out` capture keeps the run's bytes as they
came, with no stamp check, strip or scan.

- **It records at the shim's output**, through a `PIXTUOID_SOCKET` listener,
  the seam that doesn't care how a payload reached the shim. codewhale passes
  identity in its hook's environment variables, and the shim never reads
  stdin for it, so recording the shim's input would have captured empty
  payloads. A capture therefore carries production's own `_shim_ts_ms` and
  `_pid` stamps.
- **Every payload must be stamped with the scenario's source.** The CLI runs
  under a throwaway home that holds only the built pixtuoid's hook for it,
  and a payload any other hook sent refuses the capture. A CLI that spawns
  another can send that one's wire: the OpenClaw gateway runs its agent turn
  on a Claude Code backend, which inherits `PIXTUOID_SOCKET`, and six of the
  eight payloads in that capture were Claude Code's own unstamped hooks.
- **It blanks what no decoder reads** before the bytes land, and
  `deidentified` in the provenance says how many subtrees went. A plugin
  roster, a global-instruction attachment or a stray home path rides a field
  no decoder touches, so it leaves without anyone naming it, and what stays
  decodes to the same events. The allowlist is derived by probing the
  decoders, never listed, so it follows them, within the probe's limit below.
  It also keeps a transcript's write stamp where a `reads::WRITE_STAMPS` row
  names it, which `merge.rs` orders the two streams by, whether or not a
  decoder reads it, unless no decoder reads anything else on its line, which
  then blanks whole.
  - `/Users/dev` stays the convention for a home path a decoder DOES read. A
    capture redacted by hand says so in its `note`: without that, a reader
    can't tell an edited capture from an untouched one, the distinction the
    whole mechanism exists to make.
  - A field blanked today stays blank in that fixture. A decoder later
    extended to read it meets the neutral value, and its golden encodes
    "absent", so
    `a_committed_scenario_strips_to_its_events_and_bytes_and_blanked_no_field_a_decoder_reads`
    fails until the scenario is re-recorded. That test probes each blank leaf
    with one value of its type, so re-record by hand for a read inside a
    container the strip collapsed whole, or a read of one value only.
  - The other direction is cheap: `just restrip-fixtures` runs the same strip
    over the committed corpus, offline and unbilled, so a field a decoder
    STOPPED reading is one pass from gone.
- **A gate needs its own sandbox and its own prompt.** `CAPTURE_SEED=<dir>` is
  copied into the sandbox's workspace before the run, which is where a per-CLI
  ask rule belongs. `CAPTURE_PROMPT` replaces the shared prompt for a scenario
  it can't reach. Neither touches your own config. Claude Code's headless mode
  refuses a tool without firing `PermissionRequest`
  (`harness/hermetic/tui/mod.rs`), so its gate records through its TUI:
  `just hermetic --record claude-code`.
- **A scenario records into its source's one home**,
  `<module>/fixtures/<scenario>/`.
- **Re-recording a scenario that exists needs a hand**, unless the hermetic
  tier records it. The tier keeps a fixture whose record, `dropped` included,
  equals the run's but for `captured` and `sentinel_reads`, and whose bytes
  and `sentinel_reads` match the run's up to what `canonical_capture`
  (`harness::captures`) masks; otherwise `just hermetic --record` replaces
  both whole. `just capture-fixture` never overwrites committed bytes: it
  writes `<name>.new` beside them, to diff rather than trust. Only `*.jsonl`
  and `provenance.json` are read, so a `.new` you don't `mv` over the old
  changes nothing.
- **It records only on Unix.** The corpus pins Unix realpaths and cwds, so on
  Windows the recorder writes only a raw `--out` capture, and refuses to
  record or promote.

## The one edit allowed: PII

After the recorder's strip, the one edit allowed is redacting PII the strip
left behind. Any other edit and the capture stops being evidence.

- PII has arrived as cursor's `user_email` key, as the owner column of an
  `ls -la` captured in a `tool_output` (kimi), and as codex's `world_state`
  inventory of every skill on the host. The strip takes whatever rides a field
  no decoder reads; what it leaves sits inside a field one does, a `cwd` or a
  tool's command string. So read a capture before committing it, rather than
  trusting a filter on key names.
- The recorder's `scan_for_pii` refuses the operator's OWN identity (its
  `identity_needles`), the one set gitleaks can't know.
  `just fixture-pii` re-scans the committed tree by the shape of its values.
  Neither reaches a value it can't name, which is why a capture still gets
  read.
- Over both, `just test` asserts that no identity key (`user_email`,
  `account_id`, `token`, …) holds a value. The strip guarantees it, and the
  test checks it on the bytes: editing a fixture to put one back fails it.
- No size heuristic can stand in for a read either: legitimate copilot lines
  run to 39 KB, wider than the `world_state` dump that had to go.
- Redacting a field a decoder READS is the expensive case: run the golden
  again, and byte-identical means the redaction cost no evidence.

## Which events a CLI sends

One list describes each CLI: the `registers` entry of its row in
`install/target.rs`, the hooks pixtuoid registers, and so what the CLI sends. An event outside it never
reaches the shim in production, so a decoder that bails on it is correct.

An event the decoder handles but that list never registers is a SHIPPING bug
no test can see, because every test asserts our own belief about the wire.
Claude Code's permission gate is `PermissionRequest`: the decoder had read it
for years, but `install/claude.rs` registered four tool events without it, so
a session parked on a permission prompt showed as working, indefinitely. Only
a capture of a gated run says which side is wrong.

## A fixture's files

```
tests/sources/<module>/fixtures/<scenario>/
    <transcript>.jsonl     # JSONL transcript lines, fed to the source's LineDecoder
                           # (JSONL-bearing sources only — a hook-only row,
                           # transcript: None, ships NO transcript)
    hook-payloads.jsonl    # one hook payload per line, fed to decode_hook_payload
    provenance.json        # where the bytes came from (see Provenance); the recorder writes it
    # expected snapshot lives in tests/sources/<module>/snapshots/ (insta), generated on first run
```

A scenario ships the transports its run wrote: both files (Claude Code,
Codex), a transcript only (copilot, which has no hooks), or hook payloads only
(reasonix, which is hook-only, with no JSONL to watch; or a hermetic turn
whose `saves_transcript` is false).

**A transcript whose session id comes from its PARENT DIR keeps that dir.**
Flattened, the session id becomes the scenario's name, and the hook-vs-JSONL
coalescing assertion then compares a real id with a directory name:
`grok/permission-flow` passed for months only because its hooks were composed
to declare `sessionId: "permission-flow"`. So a recorded grok scenario nests,
`<scenario>/<session-uuid>/updates.jsonl`. The recorder keeps the fewest
parent dirs that leave the registry's own `id_deriver_for` id unchanged, and
the harness recurses to find it.

`conformance.rs` scans every capture except those
`harness::OUTSIDE_CONFORMANCE` lists, and each home must be a registered source's. Hand-built inputs,
`sources/decode/fixtures/`, live with their module: see
[`tests/AGENTS.md`](../AGENTS.md).

## What the harness checks

For each fixture dir, the harness:

1. decodes the transcript lines through the source's `LineDecoder`, and the
   hook payloads through `decode_hook_payload`;
2. snapshots the whole decoded `AgentEvent` sequence (`insta`);
3. **asserts every decoded event shares ONE `AgentId`**: the coalescing
   contract. This is the bug class that keeps biting, a hook and a JSONL file
   keying one session differently, and two sprites.

**Adding a CLI:** [the test steps](../AGENTS.md#adding-a-new-agent-cli--the-test-steps).

## Why the recorder is a Rust example

`just capture-fixture` drives `pixtuoid-core/examples/capture_fixture.rs`. It
was a shell script first, and every defect that version gathered was bash
semantics, not recording logic:

- an empty array under `set -u`, which stopped the CLI launching for anyone
  outside an agent session;
- `pipefail` swallowing a lister's exit 3 after the turn was billed;
- `head -1` taking SIGPIPE once a corpus passed ~700 files (the same failure,
  in the same place, found later);
- `stat -f %B`, which GNU stat reads as `--file-system`
  ([GNU stat](https://www.gnu.org/software/coreutils/manual/html_node/stat-invocation.html));
- three world-writable temp paths.

None of them could fail in a selftest small enough to run in CI, which is the
argument, not the count. In Rust they are absent by construction: no arrays,
no pipelines,
[`Metadata::created()`](https://doc.rust-lang.org/std/fs/struct.Metadata.html#method.created)
for birth time, and a fresh 0700 sandbox (`HeldDir::capture_sandbox`) with
`TempDir` scratch in place of fixed temp paths. And the decisions are ordinary
`#[test]`s that `just test` already runs, not a bespoke `--selftest` wired into
two gates.

That holds only because `Cargo.toml` declares `[[example]] test = true`,
whose comment says why; the tests first shipped without it.
