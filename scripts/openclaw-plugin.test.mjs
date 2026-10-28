// Contract test for `crates/pixtuoid/src/install/openclaw_plugin.js`: drives the
// rendered module the way OpenClaw's plugin loader does, so the spawn path, the
// one that must never block the gateway, is exercised, not stubbed. The shim is
// spawned DETACHED, so delivery is asynchronous by design.

import assert from "node:assert/strict";
import test from "node:test";
import { POSIX, assertRegisters, assertShimContract, recorded, render, row } from "./lib/plugin-harness.mjs";

const SOURCE = "openclaw";
const { registers, consumes } = row(SOURCE);
const FORWARDED = registers.filter((hook) => !consumes.includes(hook));
// OpenClaw's own default — the plugin's last-resort fallback.
const DEFAULT_GATEWAY_PORT = 18789;

/**
 * `register` reads `OPENCLAW_GATEWAY_PORT` before the config, so each test owns
 * it: unset unless `envPort` names it, and restored once `t` ends.
 * @param {import("node:test").TestContext} t
 * @param {{ shim?: "recorder" | "missing", envPort?: string }} [options]
 */
async function renderPlugin(t, { envPort, ...options } = {}) {
  const original = process.env.OPENCLAW_GATEWAY_PORT;
  t.after(() => {
    if (original === undefined) delete process.env.OPENCLAW_GATEWAY_PORT;
    else process.env.OPENCLAW_GATEWAY_PORT = original;
  });
  if (envPort === undefined) delete process.env.OPENCLAW_GATEWAY_PORT;
  else process.env.OPENCLAW_GATEWAY_PORT = envPort;
  const rendered = await render(t, SOURCE, options);
  return { ...rendered, plugin: rendered.module.default };
}

/**
 * @param {any} plugin
 * @param {{ config?: unknown }} [options]
 */
function register(plugin, { config = {} } = {}) {
  /** @type {Record<string, (ev: any, ctx?: any) => any>} */
  const handlers = {};
  plugin.register({
    config,
    /** @param {string} name @param {(ev: any, ctx?: any) => any} handler */
    on(name, handler) {
      handlers[name] = handler;
    },
  });
  return handlers;
}

test("registers exactly the contract's hooks", async (t) => {
  const { plugin } = await renderPlugin(t);
  assertRegisters(SOURCE, Object.keys(register(plugin)));
});

test("the decision hook passes EXPLICITLY and the observers are void", async (t) => {
  const { plugin } = await renderPlugin(t);
  const handlers = register(plugin);
  // Exactly one key: an extra key is a malformed decision upstream and FAILS CLOSED.
  /** @type {any} */
  const decision = handlers["before_agent_run"]({ runId: "r" }, {});
  assert.deepEqual(decision, { outcome: "pass" });
  assert.deepEqual(Object.keys(decision), ["outcome"]);
  // A FRESH object per decision, not one shared module-level literal: under a shared
  // literal a key a consumer stamps on persists into every later turn, failing closed.
  Object.assign(decision, { reason: "a consumer stamped this" });
  const next = handlers["before_agent_run"]({ runId: "r2" }, {});
  assert.deepEqual(Object.keys(next), ["outcome"], "each decision must be a fresh object");
  for (const hook of registers.filter((h) => h !== "before_agent_run")) {
    assert.equal(handlers[hook]({}, {}), undefined, `${hook} must return nothing`);
  }
});

test("an unspawnable shim still passes the gate and never throws", async (t) => {
  // A moved binary dangles the baked shim path; a throw here discards the prompt.
  const { plugin } = await renderPlugin(t, { shim: "missing" });
  const handlers = register(plugin);
  assert.deepEqual(handlers["before_agent_run"]({ runId: "r" }, {}), { outcome: "pass" });
  for (const hook of registers) {
    assert.doesNotThrow(() => handlers[hook]({ runId: "r" }, { sessionId: "s" }));
  }
});

test("forwards ONLY the allowlist plus identity, never content", { skip: !POSIX }, async (t) => {
  const { module, outFile, plugin } = await renderPlugin(t);
  assert.equal(typeof module.default.register, "function", "loader needs a callable register");
  const handlers = register(plugin, { config: { gateway: { port: 19789 } } });

  handlers["before_agent_run"](
    {
      messages: [{ content: "SECRET_MESSAGE" }],
      prompt: "SECRET_PROMPT",
      runId: "event-run",
      sessionFile: "/SECRET/PATH",
      systemPrompt: "SECRET_SYSTEM",
    },
    { runId: "context-run", sessionId: "context-session" },
  );
  handlers["agent_end"](
    {
      error: "SECRET_ERROR",
      messages: [{ content: "SECRET_MESSAGE" }],
      runId: "event-run",
      sessionId: "event-session",
      success: false,
    },
    {},
  );

  // BY TYPE: the shims are detached, so their appends race — arrival order is
  // deliberately not part of the contract.
  const payloads = await recorded(outFile);
  assert.equal(payloads.length, 2);
  const byType = new Map(payloads.map((l) => [l.type, l]));
  const run = byType.get("before_agent_run");
  const end = byType.get("agent_end");
  assert.deepEqual(run, {
    type: "before_agent_run",
    // ctx wins over the event for ids.
    runId: "context-run",
    sessionId: "context-session",
    gatewayPort: 19789,
    _pid: process.pid,
  });
  assert.deepEqual(end, {
    type: "agent_end",
    runId: "event-run",
    sessionId: "event-session",
    errored: true,
    gatewayPort: 19789,
    _pid: process.pid,
  });
  const serialized = JSON.stringify([run, end]);
  assert.ok(!serialized.includes("SECRET"), `no content may leak: ${serialized}`);
});

test("the port observed on a hook outranks the config fallback", { skip: !POSIX }, async (t) => {
  // `--port` reaches neither `api.config.gateway.port` nor the environment, so
  // `gateway_start`'s `event.port` is the ONLY authoritative source.
  const { outFile, plugin } = await renderPlugin(t);
  const handlers = register(plugin, { config: { gateway: { port: 18789 } } });
  handlers["gateway_start"]({ port: 19790 }, { port: 19790 });
  handlers["session_start"]({ sessionId: "s1" }, { sessionId: "s1" });
  const payloads = await recorded(outFile);
  assert.equal(payloads.length, 2);
  const byType = new Map(payloads.map((l) => [l.type, l]));
  assert.equal(byType.get("gateway_start").gatewayPort, 19790, "the bound port wins");
  assert.equal(
    byType.get("session_start").gatewayPort,
    19790,
    "and is remembered for port-less hooks",
  );
});

test("port resolution defaults when neither env nor config names one", { skip: !POSIX }, async (t) => {
  const { outFile, plugin } = await renderPlugin(t);
  register(plugin)["session_start"]({ sessionId: "s" }, {});
  assert.deepEqual((await recorded(outFile)).map((l) => l.gatewayPort), [DEFAULT_GATEWAY_PORT]);
});

// A bare `Number.parseInt` matches upstream's parser on plain digits and diverges on
// every other form — it stops at the first non-digit, so `127.0.0.1:18902` became
// port `127`, keying the mascot to a gateway nobody was running. `CONFIG_PORT` is
// distinct from every expected value, so a fall-through can't be read as a parse.
const CONFIG_PORT = 20100;
for (const [raw, want, why] of /** @type {[string, number, string][]} */ ([
  ["18902", 18902, "bare digits"],
  ["  18902  ", 18902, "trimmed"],
  ["127.0.0.1:18902", 18902, "host:port — the parseInt regression (was 127)"],
  ["[::1]:18902", 18902, "bracketed IPv6"],
  ["18902abc", CONFIG_PORT, "trailing garbage is not a port (was 18902)"],
  ["a:b:18902", CONFIG_PORT, "two colons is not host:port"],
  ["70000", CONFIG_PORT, "above the TCP max"],
  ["0", CONFIG_PORT, "not positive"],
  ["", CONFIG_PORT, "empty"],
])) {
  test(`env port form: ${JSON.stringify(raw)} ⇒ ${want} (${why})`, { skip: !POSIX }, async (t) => {
    const { outFile, plugin } = await renderPlugin(t, { envPort: raw });
    register(plugin, { config: { gateway: { port: CONFIG_PORT } } })["session_start"](
      { sessionId: "s" },
      {},
    );
    assert.deepEqual((await recorded(outFile)).map((l) => l.gatewayPort), [want]);
  });
}

test("agent_end forwards the errored verdict, never the error string", { skip: !POSIX }, async (t) => {
  // Upstream builds `success` as `!aborted && !promptError`, so success:false alone
  // cannot tell a user CANCELLING a turn from a prompt error. Only a prompt error
  // carries `error`, so its PRESENCE is the signal; the string can embed prompt
  // content and must never leave the gateway.
  const { outFile, plugin } = await renderPlugin(t);
  const handlers = register(plugin, { config: { gateway: { port: 18789 } } });
  handlers["agent_end"](
    { runId: "r1", sessionId: "s1", success: false, error: "context overflow: my secret prompt" },
    {},
  );
  handlers["agent_end"]({ runId: "r2", sessionId: "s1", success: false }, {});
  handlers["agent_end"]({ runId: "r3", sessionId: "s1", success: true }, {});
  const rows = await recorded(outFile);
  assert.equal(rows.length, 3);
  const byRun = new Map(rows.map((r) => [r.runId, r]));

  assert.equal(byRun.get("r1").errored, true, "a prompt error is a real failure");
  assert.equal(byRun.get("r2").errored, false, "an abort carries no error → not degraded");
  assert.equal(byRun.get("r3").errored, false, "a clean run is no failure");
  for (const row of rows) {
    assert.ok(!("success" in row), `the ambiguous success stays in the gateway: ${JSON.stringify(row)}`);
    assert.equal(row.error, undefined, "the error STRING must never be forwarded");
    assert.ok(
      !JSON.stringify(row).includes("my secret prompt"),
      `no error text may leak: ${JSON.stringify(row)}`,
    );
  }
});

test("an out-of-range observed port is refused, keeping the resolved one", { skip: !POSIX }, async (t) => {
  // pixtuoid's decoder REJECTS an envelope whose port is unusable, so stamping a
  // bogus value here would silently drop the mascot.
  const { outFile, plugin } = await renderPlugin(t);
  const handlers = register(plugin, { config: { gateway: { port: 18999 } } });
  const bogus = [0, -1, 65_536, 1.5, "19001", null];
  for (const port of bogus) handlers["gateway_start"]({ port }, {});
  assert.deepEqual(
    (await recorded(outFile)).map((l) => l.gatewayPort),
    Array(bogus.length).fill(18999),
    "a bogus port was adopted",
  );
});

test("every forwarded hook keeps the shim contract", { skip: !POSIX }, async (t) => {
  // `--source openclaw` is how the shim stamps `_pixtuoid_source` — its ONLY
  // attribution channel, and the reason a payload reaches the daemon lane at all.
  const { argvFile, module, outFile, plugin } = await renderPlugin(t);
  assert.equal(module.default.id, "pixtuoid", "the export id must match the manifest id");
  const handlers = register(plugin);
  for (const hook of FORWARDED) handlers[hook]({ reason: "shutdown" }, {});
  const payloads = await recorded(outFile);
  assert.deepEqual(payloads.map((p) => p.type).sort(), [...FORWARDED].sort());
  await assertShimContract(SOURCE, argvFile, payloads);
});

// A failed provider's `agent_end` carries no `error` — it is no prompt error — so the
// run's LAST model call decides: a provider failure degrades, a later call in the run
// that completes clears it, and the user's own abort is none.
test("a run's last model call decides errored, and the call itself is never forwarded", { skip: !POSIX }, async (t) => {
  const { outFile, plugin } = await renderPlugin(t);
  const handlers = register(plugin, { config: { gateway: { port: 18789 } } });
  /**
   * @param {string} runId
   * @param {string} outcome
   * @param {string} [failureKind]
   */
  const call = (runId, outcome, failureKind) =>
    handlers["model_call_ended"]({ runId, callId: "c", outcome, failureKind }, {});
  call("down", "error");
  call("down", "error", "connection_reset");
  call("retried", "error");
  call("retried", "completed");
  call("aborted", "error", "aborted");
  call("clean", "completed");
  for (const runId of ["down", "retried", "aborted", "clean", "no-calls"]) {
    handlers["agent_end"]({ runId, sessionId: "s", success: true }, {});
  }
  const rows = await recorded(outFile);
  assert.deepEqual(
    rows.map((r) => r.type),
    Array(5).fill("agent_end"),
    "model_call_ended must never reach the shim",
  );
  const errored = Object.fromEntries(rows.map((r) => [r.runId, r.errored]));
  assert.deepEqual(errored, {
    down: true,
    retried: false,
    aborted: false,
    clean: false,
    "no-calls": false,
  });
});

test("a run's failure is its own, and agent_end forgets it", { skip: !POSIX }, async (t) => {
  const { outFile, plugin } = await renderPlugin(t);
  const handlers = register(plugin, { config: { gateway: { port: 18789 } } });
  handlers["model_call_ended"]({ runId: "a", outcome: "error" }, {});
  // Shims finish in any order, so each end is told apart by its sessionId.
  handlers["agent_end"]({ runId: "b", sessionId: "b", success: true }, {});
  handlers["agent_end"]({ runId: "a", sessionId: "a-first", success: true }, {});
  handlers["agent_end"]({ runId: "a", sessionId: "a-again", success: true }, {});
  const rows = await recorded(outFile);
  assert.equal(rows.length, 3);
  assert.deepEqual(Object.fromEntries(rows.map((r) => [r.sessionId, r.errored])), {
    b: false,
    "a-first": true,
    "a-again": false,
  });
});

test("the failed-call ledger is bounded, the least recently called run first", { skip: !POSIX }, async (t) => {
  // A run upstream ends without `agent_end` never clears its entry.
  const { module, outFile, plugin } = await renderPlugin(t);
  const handlers = register(plugin, { config: { gateway: { port: 18789 } } });
  const cap = module.MAX_TRACKED_RUNS;
  assert.ok(Number.isInteger(cap) && cap > 0);
  const failed = (/** @type {string} */ runId) => handlers["model_call_ended"]({ runId, outcome: "error" }, {});
  for (let i = 0; i < cap; i++) failed(`r${i}`);
  failed("r0"); // still calling: now the most recent
  failed(`r${cap}`);
  for (const runId of ["r0", "r1", "r2"]) {
    handlers["agent_end"]({ runId, success: true }, {});
  }
  const rows = await recorded(outFile);
  assert.equal(rows.length, 3);
  assert.deepEqual(
    Object.fromEntries(rows.map((r) => [r.runId, r.errored])),
    { r0: true, r1: false, r2: true },
    "one past the cap evicts only the run whose last call is oldest",
  );
});
