// @pixtuoid-openclaw-plugin
//
// Forwards OpenClaw gateway daemon-presence signals to pixtuoid's `pixtuoid-hook`
// shim, which relays them to the running pixtuoid office.
//
// PRIVACY (load-bearing): build the shim payload from an explicit ALLOWLIST of
// timing/id fields ONLY — NEVER message content, prompts, or file paths. The
// `allowConversationAccess` grant only un-gates `before_agent_run`/`agent_end`
// firing; it does NOT sanitize the payload — this allowlist is the sanitizer.
//
// NEVER BLOCK THE GATEWAY. `before_agent_run` is an AWAITED, FAIL-CLOSED decision
// gate on the user's own turn — a throw or a hang past its decision timeout
// discards their prompt — so these rules hold here:
//   1. the shim is spawned DETACHED + unref'd and NOTHING is awaited;
//   2. every handler is try/catch'd and can never throw;
//   3. the decision hook returns an EXPLICIT `{ outcome: "pass" }`.
// Rule 3 is deliberate: `undefined` passes only because a filter keeps upstream's
// written `undefined → block` arm unreachable. Any EXTRA key is rejected as a
// malformed decision and fails closed, so keep that object at exactly one key.
//
// GATEWAY IDENTITY (`gatewayPort`): OpenClaw supports several isolated gateways
// per host, each on its own base port, so the port is the runtime identity
// pixtuoid keys a mascot on. `gateway_start`'s REAL bound port is the only
// authoritative source — a `--port` override reaches NEITHER
// `api.config.gateway.port` NOR `OPENCLAW_GATEWAY_PORT` — so it is adopted from
// any hook that carries one. The registration-time resolution below is only the
// fallback for a plugin hot reload, which re-runs `register` WITHOUT replaying
// `gateway_start`; after one of those on a `--port`-overridden gateway the hover
// permanently misidentifies which gateway it is. Accepted residual.
//
// Deliberately NO `import { resolveGatewayPort } from "openclaw/plugin-sdk/core"`:
// this file is dropped into OpenClaw's state dir, whose path chain contains no
// `node_modules/openclaw` for a globally installed CLI, so a bare-specifier
// import would fail module resolution and take the WHOLE plugin down; its
// `@import`s are comments only tsc reads.

/** @import { OpenClawPluginApi, OpenClawPluginDefinition } from "openclaw/plugin-sdk/plugin-entry" */
/** @import { Fields } from "./plugin-types.js" */

import { spawn } from "node:child_process";

const HOOK_PATH = "{{HOOK_PATH_JSON}}";

/** @typedef {Parameters<OpenClawPluginApi["on"]>[1]} Handler */
/** @typedef {Fields<Parameters<Handler>[0]>} HookEvent */
/** @typedef {Fields<Parameters<Handler>[1]>} HookContext */

// The ONLY fields forwarded: the run pairing key and ids the decoder reads, never
// content (`messages`, `prompt`, `error`). `success` stays behind too: the verdict
// below is the run's outcome, so nothing downstream can read the ambiguous one.
const ALLOW = /** @type {const} */ (["runId", "sessionId"]);

// OpenClaw's own `DEFAULT_GATEWAY_PORT` (`src/config/paths.ts`), un-importable
// from here (see the module note).
const DEFAULT_GATEWAY_PORT = 18789;
const MAX_TCP_PORT = 65535;

// The gateway's resolved port — the mascot's identity. Seeded at registration,
// then UPGRADED in place the first time a hook hands us the real bound port.
let gatewayPort = DEFAULT_GATEWAY_PORT;

/**
 * @param {unknown} n
 * @returns {n is number}
 */
function validPort(n) {
  return Number.isInteger(n) && /** @type {number} */ (n) > 0 && /** @type {number} */ (n) <= MAX_TCP_PORT;
}

// Mirrors upstream's `parseGatewayPortEnvValue` form-for-form: bare digits,
// bracketed IPv6 `[host]:port`, or a single-colon `host:port`. A plain
// `Number.parseInt` is NOT a valid shortcut — it stops at the first non-digit, so
// `OPENCLAW_GATEWAY_PORT=127.0.0.1:18902` parsed to `127`, silently keying the
// mascot to a gateway that does not exist.
/** @param {string | undefined} raw */
function portFromEnvValue(raw) {
  const trimmed = (raw ?? "").trim();
  if (!trimmed) return null;
  if (/^\d+$/.test(trimmed)) return validPort(Number(trimmed)) ? Number(trimmed) : null;
  const bracketedIpv6 = trimmed.match(/^\[[^\]]+\]:(\d+)$/);
  if (bracketedIpv6) {
    const n = Number(bracketedIpv6[1]);
    return validPort(n) ? n : null;
  }
  const firstColon = trimmed.indexOf(":");
  if (firstColon <= 0 || firstColon !== trimmed.lastIndexOf(":")) return null;
  const suffix = trimmed.slice(firstColon + 1);
  if (!/^\d+$/.test(suffix)) return null;
  return validPort(Number(suffix)) ? Number(suffix) : null;
}

/** @param {OpenClawPluginApi["config"] | undefined} config */
function resolvePort(config) {
  const fromEnv = portFromEnvValue(process.env.OPENCLAW_GATEWAY_PORT);
  if (fromEnv !== null) return fromEnv;
  const fromConfig = config && config.gateway && config.gateway.port;
  if (validPort(fromConfig)) return fromConfig;
  return DEFAULT_GATEWAY_PORT;
}

// Within one process the bound port never changes, so this only ever corrects
// the registration-time fallback.
/**
 * @param {HookEvent | undefined} ev
 * @param {HookContext | undefined} ctx
 */
function notePort(ev, ctx) {
  const observed = [ev && ev.port, ctx && ctx.port];
  for (const p of observed) {
    if (validPort(p)) gatewayPort = p;
  }
}

// The runs whose LATEST model call failed — on the embedded runner the only place a
// provider failure shows (openclaw src/agents/embedded-agent-runner/run/
// attempt.model-diagnostic-lifecycle.ts).
/** @type {Set<string>} */
const failedRuns = new Set();

// A run upstream ends without `agent_end` never clears its entry, so past this
// many the ledger drops its oldest failure.
export const MAX_TRACKED_RUNS = 64;

/** @param {HookEvent | undefined} ev */
function noteModelCall(ev) {
  try {
    if (!ev || typeof ev.runId !== "string") return;
    failedRuns.delete(ev.runId);
    // The user's own abort is no provider failure.
    if (ev.outcome === "error" && ev.failureKind !== "aborted") {
      failedRuns.add(ev.runId);
      if (failedRuns.size > MAX_TRACKED_RUNS) {
        const oldest = failedRuns.values().next().value;
        if (oldest !== undefined) failedRuns.delete(oldest);
      }
    }
  } catch (_) {
    // never throw into the gateway
  }
}

/**
 * @param {string} type
 * @param {HookEvent | undefined} ev
 * @param {HookContext | undefined} ctx
 */
function forward(type, ev, ctx) {
  try {
    notePort(ev, ctx);
    /** @type {Record<string, unknown>} */
    const payload = { type };
    for (const k of ALLOW) {
      // Pull from ctx first (where ids live), else the event — but NEVER spread
      // the whole event (which carries messages/prompt).
      const v = ctx && ctx[k] !== undefined ? ctx[k] : ev && ev[k];
      if (v !== undefined) payload[k] = v;
    }
    // pixtuoid keys one mascot per port, so a host running two gateways renders
    // two independent lobsters rather than one collapsed presence.
    payload.gatewayPort = gatewayPort;
    // Stamped on EVERY event, not just gateway_start, so a MID-ATTACH or reconnect
    // — where pixtuoid never observed gateway_start — can still adopt the live pid
    // for its abrupt-down watch. The plugin runs IN the gateway process.
    payload._pid = process.pid;
    // The run's verdict. `success: false` alone cannot call a gateway degraded:
    // upstream builds it as `!aborted && !promptError`, so a user CANCELLING a turn
    // reads `false` too, and only a prompt error carries `error` (on the event; the
    // context never does) — its mere PRESENCE, as a bare boolean because the string
    // can embed prompt content. A provider failure is no prompt error, so its
    // `agent_end` carries no `error` and reads like a clean end; the run's last
    // model call adds it.
    if (type === "agent_end") {
      const runId = typeof payload.runId === "string" ? payload.runId : "";
      const promptError = !!ev && ev.success === false && ev.error !== undefined;
      payload.errored = promptError || failedRuns.has(runId);
      failedRuns.delete(runId);
    }

    const proc = spawn(HOOK_PATH, ["--source", "openclaw"], {
      stdio: ["pipe", "ignore", "ignore"],
      detached: true,
    });
    proc.on("error", () => {});
    proc.stdin.on("error", () => {});
    proc.stdin.write(JSON.stringify(payload) + "\n");
    proc.stdin.end();
    proc.unref(); // detached — the awaited hook never waits on it
  } catch (_) {
    // never throw — a thrown error in an awaited decision hook is fail-closed
  }
}

// A FRESH object per decision, never one shared module-level literal: any key the
// gateway stamps onto it would otherwise persist into every LATER turn, and an
// extra key is rejected as malformed — fail-closed forever after one mutation.
// Deliberately NOT `Object.freeze` on a shared literal: this is an ES module and
// therefore strict-mode, so a consumer's stamp would THROW inside UPSTREAM's code,
// outside the try/catch that wraps only our own handler.
function pass() {
  return { outcome: /** @type {const} */ ("pass") };
}

// An unknown hook name on this OpenClaw version is skipped, never thrown.
/** @param {() => void} register */
function observe(register) {
  try {
    register();
  } catch (_) {
    /* skip */
  }
}

/** @satisfies {OpenClawPluginDefinition} */
export default {
  id: "pixtuoid",
  name: "Pixtuoid",
  /** @param {OpenClawPluginApi} api */
  register(api) {
    gatewayPort = resolvePort(api && api.config);
    observe(() => api.on("gateway_start", (ev, ctx) => forward("gateway_start", ev, ctx)));
    observe(() => api.on("gateway_stop", (ev, ctx) => forward("gateway_stop", ev, ctx)));
    observe(() => api.on("session_start", (ev, ctx) => forward("session_start", ev, ctx)));
    observe(() => api.on("session_end", (ev, ctx) => forward("session_end", ev, ctx)));
    // The ONE awaited DECISION hook: its verdict is NEVER derived from the
    // detached spawn (see the never-block note above).
    observe(() =>
      api.on("before_agent_run", (ev, ctx) => {
        forward("before_agent_run", ev, ctx);
        return pass();
      }),
    );
    observe(() => api.on("agent_end", (ev, ctx) => forward("agent_end", ev, ctx)));
    // Registered but never forwarded: it only feeds the verdict `agent_end` carries.
    observe(() => api.on("model_call_ended", (ev) => noteModelCall(ev)));
  },
};
