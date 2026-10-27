// @pixtuoid-omp-extension — managed by pixtuoid (connect/disconnect omp in pixtuoid's Sources panel: press s).
//
// omp's transcripts stay pixtuoid's durable authority; this bridge forwards
// ONLY what they can never carry — pre-persist presence, empty-session
// shutdown, and the tool-approval wait — into the `pixtuoid-hook` shim.
// It NEVER blocks omp: it registers NO gating handlers (omp's `tool_call`
// gate fails CLOSED on a throw/timeout), and every handler is try/catch'd.
// The ONE awaited thing is the shim itself — bounded by its own write-timeout
// watchdog, always exit 0 — because Bun.spawn has no detached mode and
// `session_shutdown` fires during process exit: an un-awaited child loses the
// race and the empty-session shutdown never arrives (capture-verified).
//
// Privacy allowlist: event type, session file/id (plus the previous session's
// file on a switch or branch), cwd, tool name, tool call id, approval
// state/reason, and process.pid. Never prompts, messages, tool arguments, tool
// results, or model output.
//
// HOOK_PATH is baked in at install time (a JSON-encoded absolute path). Safe
// to delete — disconnecting omp in pixtuoid's Sources panel replaces this
// with a removed-marker stub. Already-running omp sessions load extensions at
// startup, so they must restart to pick this up.
import type {
  ExtensionAPI,
  ExtensionContext,
  ExtensionFactory,
  SessionBranchEvent,
  SessionShutdownEvent,
  SessionStartEvent,
  SessionSwitchEvent,
  ToolApprovalRequestedEvent,
  ToolApprovalResolvedEvent,
} from "@oh-my-pi/pi-coding-agent"
import type { Fields } from "./plugin-types.js"

const HOOK_PATH: string = "{{HOOK_PATH_JSON}}"

type Observed = Fields<
  | SessionStartEvent
  | SessionSwitchEvent
  | SessionBranchEvent
  | SessionShutdownEvent
  | ToolApprovalRequestedEvent
  | ToolApprovalResolvedEvent
>

export default ((pi: ExtensionAPI) => {
  const forward = async (type: string, ev: Observed, ctx: ExtensionContext) => {
    try {
      const payload: Record<string, unknown> = { type }
      // The CONTEXT's session, not the events' own copy: ctx names the FILE
      // both transports key on, and the two agreed in every recorded round.
      try {
        payload.sessionFile = ctx.sessionManager.getSessionFile()
      } catch {}
      try {
        payload.sessionId = ctx.sessionManager.getSessionId()
      } catch {}
      if (typeof ctx.cwd === "string") payload.cwd = ctx.cwd
      if (typeof ev.previousSessionFile === "string") payload.previousSessionFile = ev.previousSessionFile
      if (typeof ev.toolCallId === "string") payload.toolCallId = ev.toolCallId
      if (typeof ev.toolName === "string") payload.toolName = ev.toolName
      if (typeof ev.reason === "string") payload.reason = ev.reason
      if (typeof ev.approved === "boolean") payload.approved = ev.approved
      // The omp process pid (extensions run in-process) — the PluginStamp
      // channel: focus and the abrupt-exit watch ride it.
      payload._pid = process.pid
      // Buffer stdin: no writable stream, no EPIPE window.
      const proc = Bun.spawn([HOOK_PATH, "--source", "omp"], {
        stdin: new TextEncoder().encode(JSON.stringify(payload)),
        stdout: "ignore",
        stderr: "ignore",
      })
      await proc.exited
    } catch {
      // Best-effort: a broken shim must never surface in omp.
    }
  }
  // Observe-only lifecycle and approval events; the decoder
  // (`pixtuoid-core/src/source/omp.rs`) claims exactly this set. Each
  // registration is try/catch'd so one unknown event name cannot take the
  // whole bridge down.
  const observe = (register: () => void) => {
    try {
      register()
    } catch {}
  }
  observe(() => pi.on("session_start", (ev, ctx) => forward("session_start", ev, ctx)))
  observe(() => pi.on("session_switch", (ev, ctx) => forward("session_switch", ev, ctx)))
  observe(() => pi.on("session_branch", (ev, ctx) => forward("session_branch", ev, ctx)))
  observe(() => pi.on("session_shutdown", (ev, ctx) => forward("session_shutdown", ev, ctx)))
  observe(() => pi.on("tool_approval_requested", (ev, ctx) => forward("tool_approval_requested", ev, ctx)))
  observe(() => pi.on("tool_approval_resolved", (ev, ctx) => forward("tool_approval_resolved", ev, ctx)))
}) satisfies ExtensionFactory
