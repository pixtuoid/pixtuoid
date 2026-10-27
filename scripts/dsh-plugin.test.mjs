// Contract test for `crates/pixtuoid/src/install/dsh_plugin.mjs`. The Rust side
// can only grep the template as a STRING; this drives the RENDERED module the way
// dsh's cordis loader does, against a REAL recorder executable, so the order the
// shims reach the socket in is exercised, not stubbed.

import assert from "node:assert/strict";
import { chmod, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { pathToFileURL } from "node:url";

import { HOOK_PLACEHOLDER, POSIX, REPO_ROOT, poll } from "./lib/plugin-harness.mjs";

const PLUGIN_SOURCE = join(REPO_ROOT, "crates", "pixtuoid", "src", "install", "dsh_plugin.mjs");

async function renderPlugin(t) {
  const root = await mkdtemp(join(tmpdir(), "pixtuoid-dsh-plugin-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const source = await readFile(PLUGIN_SOURCE, "utf8");
  assert.equal(source.split(HOOK_PLACEHOLDER).length, 2, "one hook placeholder");

  const outFile = join(root, "payloads.ndjson");
  const traceFile = join(root, "trace.txt");
  const hookPath = join(root, "recorder-hook");
  // Holds each call open, so two shims spawned together overlap in the trace.
  await writeFile(
    hookPath,
    `#!/bin/sh\necho begin >> ${JSON.stringify(traceFile)}\nsleep 0.1\n` +
      `cat >> ${JSON.stringify(outFile)}\necho end >> ${JSON.stringify(traceFile)}\n`,
  );
  await chmod(hookPath, 0o755);

  const entry = join(root, "pixtuoid-dsh.mjs");
  await writeFile(entry, source.replace(HOOK_PLACEHOLDER, JSON.stringify(hookPath)));
  const module = await import(pathToFileURL(entry).href);
  return { module, outFile, traceFile };
}

const header = { id: "s1", cwd: "/w" };
const session = { header };

// The cordis surface the plugin touches: `on`, `effect`, and the injected agents.
function apply(module) {
  const handlers = new Map();
  module.apply({
    on(name, handler) {
      handlers.set(name, handler);
    },
    effect() {},
    agents: { list: () => [] },
  });
  const event = (type, data) => handlers.get("session/event")(session, { type, data });
  return { handlers, event };
}

const lines = async (file, n) => (await poll(file, (got) => got.length >= n)).slice(0, n);

// Every payload once `agent/disposed`'s session_end, sent last, has landed: the
// queue is first in, first out, so nothing sent before it can still be missing.
const payloadsThroughEnd = async (file) =>
  (await poll(file, (got) => got.some((l) => JSON.parse(l).type === "session_end"))).map((l) =>
    JSON.parse(l),
  );

test("agent/created returns nothing, and events reach the shim in order, one at a time", { skip: !POSIX }, async (t) => {
  const { module, outFile, traceFile } = await renderPlugin(t);
  const { handlers, event } = apply(module);

  assert.equal(
    handlers.get("agent/created")({ agent: { session } }),
    undefined,
    "agent/created's listener must return nothing",
  );
  event("approval/asked", { id: "a1", callId: "c1", toolName: "bash", reason: "wider" });
  event("approval/decided", { id: "a1", outcome: "unavailable" });
  event("tool/result", { message: { toolCallId: "c1" } });
  handlers.get("agent/disposed")({ agent: { session } });

  const types = (await payloadsThroughEnd(outFile)).map((p) => p.type);
  assert.deepEqual(types, [
    "session_start",
    "approval_asked",
    "approval_decided",
    "tool_result",
    "session_end",
  ]);
  const trace = await lines(traceFile, 2 * types.length);
  assert.deepEqual(trace, types.flatMap(() => ["begin", "end"]));
});

test("forwards only the allowlist, never content", { skip: !POSIX }, async (t) => {
  const { module, outFile } = await renderPlugin(t);
  const { handlers, event } = apply(module);

  handlers.get("agent/created")({
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
  handlers.get("agent/disposed")({ agent: { session } });

  // Exact payloads: a key nobody planted fails too, and so does a payload.
  const at = { sessionId: "s1", cwd: "/w", _pid: process.pid };
  assert.deepEqual(await payloadsThroughEnd(outFile), [
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
