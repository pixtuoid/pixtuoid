// @pixtuoid-dsh-plugin — pixtuoid's cordis plugin for DeepSeek Harness.
// Written by `pixtuoid connect dsh`; edits are overwritten on reconnect.
// Zero @deepseek-ai imports ON PURPOSE: everything arrives through `ctx`, so
// this single file mounts from any absolute path with no package install and
// no second cordis instance.
//
// Privacy allowlist: event type, session id, parent link (subagent only),
// cwd, tool name, tool call id, approval outcome/reason, token counts,
// model/effort names, and process.pid. Never prompts, messages, tool
// arguments, tool results, or model output.
const HOOK_PATH = "{{HOOK_PATH_JSON}}"

import { spawn } from "node:child_process"

export const name = "pixtuoid"
export const inject = ["agents"]

// Every listener is synchronous, returns nothing and never throws, and every
// channel but `agent/created` (below) is an emit, so this plugin cannot stall a
// tool call, a prompt, or shutdown.
export function apply(ctx) {
  // One shim at a time, in emit order: shims spawned together race to the
  // socket, and a decision that lands before its ask strands the slot requiring action.
  // The shim's own write watchdog bounds each turn of the queue.
  let queue = Promise.resolve()
  const send = (payload) => {
    try {
      payload._pid = typeof process !== "undefined" ? process.pid : undefined
      const line = JSON.stringify(payload) + "\n"
      queue = queue.then(
        () =>
          new Promise((settled) => {
            try {
              const proc = spawn(HOOK_PATH, ["--source", "dsh"], {
                stdio: ["pipe", "ignore", "ignore"],
              })
              proc.on("error", settled)
              proc.on("close", settled)
              proc.stdin.on("error", () => {})
              proc.stdin.end(line)
            } catch {
              settled()
            }
          }),
      )
    } catch {
      // A broken shim must never surface into dsh.
    }
  }

  // `parentSession` alone is upstream's seed-lineage field — an ordinary
  // user branch (`ctx.sessions.fork()`) stamps it too; only `origin:
  // "subagent"` marks real delegation (SessionHeader, subagent.md
  // listChildren), so a branched conversation must not render as a child.
  const base = (header) => ({
    sessionId: String(header.id),
    ...(header.cwd ? { cwd: header.cwd } : {}),
    ...(header.origin === "subagent" && header.parentSession
      ? { parentSession: String(header.parentSession) }
      : {}),
  })

  const started = new Set()
  const sendStart = (agent) => {
    try {
      const header = agent.session.header
      if (started.has(String(header.id))) return
      started.add(String(header.id))
      send({
        type: "session_start",
        ...base(header),
      })
    } catch {}
  }
  const sendEnd = (agent) => {
    try {
      const header = agent.session.header
      if (!started.delete(String(header.id))) return
      send({ type: "session_end", ...base(header) })
    } catch {}
  }

  // `agent/created` is @mode serial: dsh holds creation until it returns, and a throw fails the
  // agent's creation, so the listener only reads its payload inside sendStart's
  // try and returns nothing.
  ctx.on("agent/created", (payload) => sendStart(payload?.agent))
  ctx.on("agent/disposed", ({ agent }) => sendEnd(agent))

  // approval/decided carries only {id, outcome}; the asked side owns
  // callId/toolName, so remember the pair until it resolves.
  const asked = new Map()
  // Named, not a literal: this channel only carries the events its cases
  // forward, and `plugin_events` (install/dsh.rs) lists events, not carriers.
  const SESSION_EVENT = "session/event"
  ctx.on(SESSION_EVENT, (session, event) => {
    try {
      const header = session.header
      const d = event.data
      switch (event.type) {
        case "turn/end":
          send({ type: "turn_end", ...base(header) })
          break
        case "tool/call":
          send({
            type: "tool_call",
            ...base(header),
            ...(d.callId ? { callId: String(d.callId) } : {}),
            toolName: d.name,
          })
          break
        case "tool/result":
          send({
            type: "tool_result",
            ...base(header),
            ...(d.message?.toolCallId ? { callId: String(d.message.toolCallId) } : {}),
          })
          break
        case "approval/asked":
          asked.set(String(d.id), { callId: d.callId, toolName: d.toolName })
          send({
            type: "approval_asked",
            ...base(header),
            ...(d.callId ? { callId: String(d.callId) } : {}),
            toolName: d.toolName,
            ...(d.reason ? { reason: d.reason } : {}),
          })
          break
        case "approval/decided": {
          const pair = asked.get(String(d.id))
          asked.delete(String(d.id))
          send({
            type: "approval_decided",
            ...base(header),
            outcome: d.outcome,
            ...(pair?.callId ? { callId: String(pair.callId) } : {}),
            ...(pair?.toolName ? { toolName: pair.toolName } : {}),
          })
          break
        }
        case "assistant/message":
          if (d.usage) {
            send({
              type: "usage",
              ...base(header),
              inputTokens: d.usage.inputTokens,
              outputTokens: d.usage.outputTokens,
              ...(d.usage.cacheWriteTokens !== undefined
                ? { cacheWriteTokens: d.usage.cacheWriteTokens }
                : {}),
            })
          }
          break
        case "request/header": {
          const config = d.header?.config
          if (config) {
            send({
              type: "model",
              ...base(header),
              model: config.model,
              ...(config.reasoningEffort ? { reasoningEffort: config.reasoningEffort } : {}),
            })
          }
          break
        }
        default:
      }
    } catch {}
  })

  // The plugin's fiber may unwind BEFORE the agent registry's during tree
  // teardown, so agent/disposed is not guaranteed for every live agent —
  // sweep them here. Best-effort: an unflushed child at process exit is
  // covered by the pid watch on the Rust side.
  ctx.effect(() => () => {
    try {
      for (const agent of ctx.agents.list()) sendEnd(agent)
    } catch {}
  })
}
