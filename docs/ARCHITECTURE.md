# Architecture

How a running coding-agent session becomes a moving sprite in the office.

> This file is the **single source** for pixtuoid's architecture overview. It
> renders on the website at [`/architecture`](https://pixtuoid.dev/architecture/)
> and on GitHub (the diagram below is native Mermaid). Contributor-level
> detail lives in `AGENTS.md` and the code's own doc comments.

## The shape of it

pixtuoid is a Cargo workspace of **six crates** wired as a strict
**producer → reducer → renderer** pipeline:

- **`pixtuoid-core`** — the headless library: sources and decoders, the
  reducer + scene state. No terminal dependencies.
- **`pixtuoid-scene`** — the backend-agnostic render + simulation **engine**:
  the office world itself (`look::render`, the sprite format, the grid/walkable
  vocabulary, layout, pose, walk physics and pathfinding, themes, pets).
  Terminal- AND window-free **by crate boundary** — compiler-enforced, not
  just a lint.
- **`pixtuoid`** — the binary: the CLI, the runtime wiring, and two thin
  painters over the engine — the TUI renderer and the floating desktop
  window.
- **`pixtuoid-web`** — the third painter: a publish-excluded wasm crate
  rendering the same engine into a browser `<canvas>` (the site's live hero),
  with `native` (core's async runtime) off so core compiles to wasm32.
- **`pixtuoid-web-music`** — the site's ♩ lofi stream, in a publish-excluded
  wasm of its own that only the site's music worker loads.
- **`pixtuoid-hook`** — a tiny shim your coding agent invokes per hook event:
  stdin JSON → a local IPC endpoint (Unix socket / named pipe), and it
  **always exits 0** so it can never block your agent.

Dependency direction is one-way: `pixtuoid-core ← pixtuoid-scene ←
{pixtuoid, pixtuoid-web, pixtuoid-web-music}`. The engine's frame entry (`look::render`) is the
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
  accTitle: How a coding session becomes a sprite
  accDescr: Your coding agent reports each step through pixtuoid-hook, a tiny shim, and pixtuoid also reads the transcript the agent writes. Both become events that update one picture of the office, who is there and what each is doing, which the engine draws in the terminal or a desktop window; pixtuoid serve can send it to a paired device. A gateway such as OpenClaw reports through the same shim and shows up as a wandering mascot instead of a desk.
  AG["Your coding agent<br/>Claude Code, Codex, …"]
  GW["A gateway<br/>OpenClaw"]
  HK["pixtuoid-hook<br/>a tiny shim"]
  TR["Transcript reader"]
  EV["Events<br/>what each session just did"]
  MS["Mascots<br/>one per running gateway"]
  ST["The office<br/>who is here and what each is doing"]
  EN["The engine draws a frame"]
  AG -->|each step| HK
  AG -.->|writes a transcript| TR
  GW -->|its status| HK
  HK -->|from an agent| EV
  HK -->|from a gateway| MS
  TR --> EV
  EV --> ST
  MS --> ST
  ST --> EN
  EN --> T1["Terminal"]
  EN --> T2["Desktop window"]
  ST -.-> SV["pixtuoid serve<br/>to a paired device"]
```

**Walking the pipeline:**

1. **Ingest.** A hook event rides the shim (enriched, watchdog-bounded, exit
   0) to `HookSocketListener`, where `decode_hook_object` turns it into
   `AgentEvent`s. A second instance's `HookRouter` subscribes to the socket's
   owner for the same lines, so each instance keeps its own reducer and
   viewport, and takes the socket when that owner exits. In parallel,
   `JsonlWatcher` tails each agent's transcript (a first-sight gate keeps
   historical sessions from resurrecting) and decodes lines via that source's
   own decoder.
2. **One channel.** Every source multiplexes onto a single
   `mpsc::Sender<(Transport, AgentEvent)>`; the `Transport` tag drives
   **hook-wins dedup**, so a hook and its transcript echo don't double-count.
3. **Reduce.** `Reducer::apply` folds events into a `SceneState`, and `Reducer::tick` sweeps
   its stale slots on the driver's `SWEEP_TICK_INTERVAL_SECS` tick; `SceneSink::publish`
   sends each changed scene as a fresh `Arc<SceneState>` on a `watch` channel.
4. **Render.** The renderer borrows the latest scene (O(1), no lock), paints
   it through the engine's terminal-agnostic pixel pass, then flushes it as
   terminal images where the terminal draws them, else half-block (`▀`) cells.

**The daemon lane.** A daemon creates no agent slot and writes no transcript:
the `HookRouter` decodes its payloads via the source's own `presence_decoder`
and pushes `PresenceMsg { key: DaemonInstanceKey, delta }` onto a **sibling
channel** — never the agent channel — merged by the `AgentId`-free
`apply_presence` into `SceneState::daemons`. N concurrent instances of one
gateway route to distinct mascots; a daemon has no per-session pid, so
*silence* (a decayed TTL) is its abrupt-down signal.

**Off the machine.** `pixtuoid serve` is this pipeline with no painter, the one
listener on a network: paired devices on the LAN read the scene over pinned TLS
as `pixtuoid_core::remote` lines, and may list the agent CLIs here and connect or
disconnect them. A line carries no desk: a device seats everyone itself and
renders with the same engine.

## Beyond this workspace

- **[pixtuoid/lofi](https://github.com/pixtuoid/lofi)**: the procedural music,
  the `pixtuoid-lofi` crate that `pixtuoid-scene` plays.
- **The pixtuoid App** (iPhone and iPad, coming soon): a painter outside this
  workspace. It pairs with a `pixtuoid serve` from the link `pixtuoid pair`
  prints, connects over TLS pinned to that link's key on the local network,
  finds the computer again by DNS-SD when its address changes, and draws
  `pixtuoid_core::remote` lines with `pixtuoid-scene`, both built in from one
  pinned revision of this repository.
- **[pixtuoid/.github](https://github.com/pixtuoid/.github)**: what every
  repository shares. [How we build](HOW-WE-BUILD.md) lists the repositories
  and how the work runs across them.

## Seams & invariants

Load-bearing — see `AGENTS.md` and the nested crate guides before changing:

- The **`Source` trait** is the only seam for a transcript-bearing agent CLI;
  per-source format knowledge lives in that source's own decoders. Hook-only
  CLIs are the documented exception: registry rows with `transcript: None`, a
  custom hook decoder, and an install target.
- **Cross-source facts live in ONE registry row** (its module's `DESCRIPTOR`):
  prefix, decoders, hook decoding, capability flags. The reducer derives
  lifecycle policy from the flags — it never matches CLI names.
- **Events flow through one tagged channel**; producers tag their own events.
- **Subagent supervision is a scope tree**: exit cascades down, liveness
  flows up, permission-blocked subagents are exempt from the stale sweep.
- **The walkable mask is the ground footprint only** — sprites may be
  visually larger than the tile their base occupies.

## Where to go next

- **Configure it:** [`docs/CONFIGURATION.md`](CONFIGURATION.md) ·
  [live `/config`](https://pixtuoid.dev/config/)
- **Contribute:** [`CONTRIBUTING.md`](CONTRIBUTING.md)
- **Agent/contributor detail:** the workspace `AGENTS.md` + the nested
  per-crate `AGENTS.md` files.
