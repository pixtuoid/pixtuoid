# Architecture

How a running coding-agent session becomes a moving sprite in the office.

> This file is the **single source** for pixtuoid's architecture overview. It
> renders on the website at [`/architecture`](https://pixtuoid.dev/architecture)
> and on GitHub (the diagram below is native Mermaid). Contributor-level
> detail lives in `AGENTS.md` and the code's own doc comments.

## The shape of it

pixtuoid is a Cargo workspace of **five crates** wired as a strict
**producer → reducer → renderer** pipeline:

- **`pixtuoid-core`** — the headless library: sources and decoders, the
  reducer + scene state, the sprite format, the grid/walkable vocabulary. No
  terminal dependencies.
- **`pixtuoid-scene`** — the backend-agnostic render + simulation **engine**:
  the office world itself (`look::render`, layout, pose, walk
  physics and pathfinding, themes, pets). Terminal- AND window-free **by
  crate boundary** — compiler-enforced, not just a lint.
- **`pixtuoid`** — the binary: the CLI, the runtime wiring, and two thin
  painters over the engine — the TUI renderer and the floating desktop
  window.
- **`pixtuoid-web`** — the third painter: a publish-excluded wasm crate
  rendering the same engine into a browser `<canvas>` (the site's live hero),
  with `native` (core's async runtime) disabled so the pure
  decode/reduce core compiles to wasm32.
- **`pixtuoid-hook`** — a tiny shim your coding agent invokes per hook event:
  stdin JSON → a local IPC endpoint (Unix socket / named pipe), and it
  **always exits 0** so it can never block your agent.

Dependency direction is one-way: `pixtuoid-core ← pixtuoid-scene ←
{pixtuoid, pixtuoid-web}`. The engine's frame entry (`look::render`) is the
inversion point that keeps the core terminal-free — the same entry drives the
terminal, the desktop window, and the browser canvas, in either look.

A **`Source`** is an **Agent** — a transcript- or hook-bearing coding CLI
whose events become a **desk sprite** — or a **Daemon** — a long-running
gateway with no transcript and no desk, shown as one wandering mascot per
running instance. The OpenClaw gateway is the first daemon: it ambles when
idle, shuttles when a turn is in flight, sickens red when its backend
degrades, and walks out when it goes down.

## Data flow

```mermaid
flowchart TB
  accTitle: pixtuoid data flow
  accDescr: A hook or transcript event flows from a coding agent (Claude Code, Codex) through the pixtuoid-hook shim into pixtuoid-core, where a HookRouter demuxes agent payloads to the Transport-tagged reducer and SceneState.agents. A daemon gateway (OpenClaw) shares the same shim and socket but is routed instead to apply_presence and SceneState.daemons over a sibling channel that bypasses the reducer. The pixtuoid TUI renderer then paints the whole scene through the pixtuoid-scene engine's terminal-agnostic pixel pass and a half-block flush.
  CC["Claude Code / Codex<br/>(agent source)"]
  OC["OpenClaw gateway<br/>(daemon source)"]

  subgraph hook["pixtuoid-hook (shim)"]
    SH["enrich + forward<br/>WRITE_TIMEOUT-bounded · exit 0"]
  end

  subgraph core["pixtuoid-core (headless)"]
    L["HookRouter<br/>(shared socket: Unix / named pipe)"]
    D["decode_hook_payload"]
    J["JsonlWatcher · walk_jsonl"]
    R["Reducer::apply<br/>(Transport-tagged)"]
    AP["apply_presence<br/>(AgentId-free · bypasses Reducer)"]
    S["SceneState<br/>agents + daemons"]
    L -->|agent payload| D
    L -.->|"daemon payload<br/>is_daemon()"| AP
    D -->|"(Hook, AgentEvent)"| R
    J -->|"(Jsonl, AgentEvent)"| R
    R --> S
    AP -.->|"PresenceMsg{key, delta}<br/>sibling channel"| S
    R -.->|"scope tree:<br/>cascade ↓ · liveness ↑"| R
  end

  subgraph bin["pixtuoid (binary · TUI)"]
    W["watch&lt;Arc&lt;SceneState&gt;&gt;"]
    TR["TuiRenderer"]
    FL["flush · ½-block cells"]
  end

  subgraph scene["pixtuoid-scene (engine)"]
    PX["look::render<br/>(desks + mascots)"]
  end

  W --> TR --> PX --> FL

  CC -->|hook event| SH
  OC -->|hook event| SH
  SH --> L
  CC -.->|writes transcript JSONL| J
  S -. Arc per mutation .-> W
```

**Walking the pipeline:**

1. **Ingest.** A hook event rides the shim (enriched, watchdog-bounded, exit
   0) to `HookSocketListener`, where `decode_hook_payload` turns it into
   `AgentEvent`s. In parallel, `JsonlWatcher` tails each agent's transcript
   (a first-sight gate keeps historical sessions from resurrecting) and
   decodes lines via that source's own decoder.
2. **One channel.** Every source multiplexes onto a single
   `mpsc::Sender<(Transport, AgentEvent)>`; the `Transport` tag drives
   **hook-wins dedup**, so a hook and its transcript echo don't double-count.
3. **Reduce.** `Reducer::apply` folds events into a `SceneState` (stale
   sweeps on the driver's `SWEEP_TICK_INTERVAL_SECS` tick) and publishes a
   fresh `Arc<SceneState>` on a `watch` channel after every change.
4. **Render.** The renderer borrows the latest scene (O(1), no lock), paints
   it through the engine's terminal-agnostic pixel pass, then flushes pixel
   rows as half-block (`▀`) terminal cells.

**The daemon lane.** A daemon creates no agent slot and writes no transcript:
the `HookRouter` decodes its payloads via the source's own `presence_decoder`
and pushes `PresenceMsg { key: DaemonInstanceKey, delta }` onto a **sibling
channel** — never the agent channel — merged by the `AgentId`-free
`apply_presence` into `SceneState::daemons`. N concurrent instances of one
gateway route to distinct mascots; a daemon has no per-session pid, so
*silence* (a decayed TTL) is its abrupt-down signal.

## Seams & invariants

Load-bearing — see `AGENTS.md` and the nested crate guides before changing:

- The **`Source` trait** is the only seam for a transcript-bearing agent CLI;
  per-source format knowledge lives in that source's own decoders. Hook-only
  CLIs are the documented exception: registry rows with `transcript: None`, a
  custom hook decoder, and an install target.
- **Cross-source facts live in ONE registry row** (`SourceDescriptor`):
  prefix, decoders, hook decoding, capability flags. The reducer derives
  lifecycle policy from the flags — it never matches CLI names.
- **Events flow through one tagged channel**; producers tag their own events.
- **Subagent supervision is a scope tree**: exit cascades down, liveness
  flows up, permission-blocked subagents are exempt from the stale sweep.
- **The walkable mask is the ground footprint only** — sprites may be
  visually larger than the tile their base occupies.

## Where to go next

- **Configure it:** [`docs/CONFIGURATION.md`](CONFIGURATION.md) ·
  [live `/config`](https://pixtuoid.dev/config)
- **Contribute:** [`CONTRIBUTING.md`](CONTRIBUTING.md)
- **Agent/contributor detail:** the workspace `AGENTS.md` + the nested
  per-crate `AGENTS.md` files.
