// Contract test for `crates/pixtuoid/src/install/dsh_plugin.mjs`: drives the
// rendered module the way dsh's cordis loader does, so the order the shims
// reach the socket in is exercised, not stubbed.

import assert from "node:assert/strict";
import test from "node:test";
import { POSIX, assertRegisters, assertShimContract, lines, recorded, render, row } from "./lib/plugin-harness.mjs";

const SOURCE = "dsh";
// Long enough that two shims spawned together would overlap in the trace.
const HOLD_MS = 100;

const header = { id: "s1", cwd: "/w" };
const session = { header };

/**
 * The cordis surface the plugin touches: `on`, `effect`, and the injected agents.
 * @param {any} module
 */
function apply(module) {
  /** @type {Map<string, Function>} */
  const handlers = new Map();
  module.apply({
    /** @param {string} name @param {Function} handler */
    on(name, handler) {
      handlers.set(name, handler);
    },
    effect() {},
    agents: { list: () => [] },
  });
  /** @param {string} type @param {unknown} data */
  const event = (type, data) => handlers.get("session/event")?.(session, { type, data });
  return { handlers, event };
}

// A session event of each type the plugin forwards, carrying what it reads.
/** @type {Record<string, unknown>} */
const FULL = {
  "tool/call": { callId: "c1", name: "bash" },
  "tool/result": { message: { toolCallId: "c1" } },
  "approval/asked": { id: "a1", callId: "c1", toolName: "bash", reason: "wider" },
  "approval/decided": { id: "a1", outcome: "unavailable" },
  "assistant/message": { usage: { inputTokens: 1, outputTokens: 2 } },
  "request/header": { header: { config: { model: "m" } } },
  "turn/end": { turn: 1 },
};

test("forwards each event of the contract once, and no other", { skip: !POSIX }, async (t) => {
  const { argvFile, module, outFile } = await render(t, SOURCE);
  const { handlers, event } = apply(module);
  const subscribed = [...handlers.keys()].filter((name) => name !== "session/event");
  const cases = row(SOURCE).registers.filter((name) => !subscribed.includes(name));
  assertRegisters(SOURCE, [...subscribed, ...cases]);

  handlers.get("agent/created")?.({ agent: { session } });
  event("user/message", { content: "SECRET_PROMPT" });
  for (const type of cases) event(type, FULL[type]);
  handlers.get("agent/disposed")?.({ agent: { session } });

  const payloads = await recorded(outFile);
  assert.equal(payloads.length, cases.length + 2, `one payload per case, the session's ends: ${JSON.stringify(payloads)}`);
  await assertShimContract(SOURCE, argvFile, payloads);
});

test("agent/created returns nothing, and events reach the shim in order, one at a time", { skip: !POSIX }, async (t) => {
  const { module, outFile, traceFile } = await render(t, SOURCE, { holdMs: HOLD_MS });
  const { handlers, event } = apply(module);

  assert.equal(
    handlers.get("agent/created")?.({ agent: { session } }),
    undefined,
    "agent/created's listener must return nothing",
  );
  event("approval/asked", { id: "a1", callId: "c1", toolName: "bash", reason: "wider" });
  event("approval/decided", { id: "a1", outcome: "unavailable" });
  event("tool/result", { message: { toolCallId: "c1" } });
  handlers.get("agent/disposed")?.({ agent: { session } });

  const types = (await recorded(outFile)).map((p) => p.type);
  assert.deepEqual(types, ["session_start", "approval_asked", "approval_decided", "tool_result", "session_end"]);
  assert.deepEqual(await lines(traceFile), types.flatMap(() => ["begin", "end"]));
});

test("forwards only the allowlist, never content", { skip: !POSIX }, async (t) => {
  const { module, outFile } = await render(t, SOURCE);
  const { handlers, event } = apply(module);

  handlers.get("agent/created")?.({
    agent: { session: { header: { ...header, title: "SECRET_TITLE", prompt: "SECRET_PROMPT" } } },
  });
  event("tool/call", { callId: "c1", name: "bash", input: { command: "SECRET_ARGUMENT" } });
  event("approval/asked", {
    id: "a1",
    callId: "c1",
    toolName: "bash",
    reason: "wider access",
    input: { command: "SECRET_ARGUMENT" },
  });
  event("approval/decided", { id: "a1", outcome: "unavailable", note: "SECRET_NOTE" });
  event("tool/result", { message: { toolCallId: "c1", content: [{ text: "SECRET_RESULT" }] } });
  event("assistant/message", {
    content: [{ type: "text", text: "SECRET_OUTPUT" }],
    usage: { inputTokens: 1, outputTokens: 2 },
  });
  event("request/header", {
    header: { config: { model: "m", system: "SECRET_SYSTEM" } },
    messages: ["SECRET_MESSAGE"],
  });
  event("turn/end", { turn: 1, reason: { kind: "error", error: { message: "SECRET_ERROR" } } });
  handlers.get("agent/disposed")?.({ agent: { session } });

  // Exact payloads: a key nobody planted fails too, and so does a payload.
  const at = { sessionId: "s1", cwd: "/w", _pid: process.pid };
  assert.deepEqual(await recorded(outFile), [
    { type: "session_start", ...at },
    { type: "tool_call", ...at, callId: "c1", toolName: "bash" },
    { type: "approval_asked", ...at, callId: "c1", toolName: "bash", reason: "wider access" },
    { type: "approval_decided", ...at, outcome: "unavailable", callId: "c1", toolName: "bash" },
    { type: "tool_result", ...at, callId: "c1" },
    { type: "usage", ...at, inputTokens: 1, outputTokens: 2 },
    { type: "model", ...at, model: "m" },
    { type: "turn_end", ...at },
    { type: "session_end", ...at },
  ]);
});

