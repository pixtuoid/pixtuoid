// Contract test for `crates/pixtuoid/src/install/opencode_plugin.ts`: drives the
// rendered plugin the way opencode's promise host does, over a scripted event
// stream and a real recorder.

import assert from "node:assert/strict";
import test from "node:test";
import { POSIX, assertShimContract, recorded, render, row } from "./lib/plugin-harness.mjs";

const SOURCE = "opencode";
const { registers, data_fields: dataFields = [] } = row(SOURCE);

// Every field the decoder reads, nested as opencode nests it, beside content
// that must never leave.
const FULL = {
  sessionID: "ses_1",
  parentID: "ses_0",
  location: { directory: "/r", secret: "SECRET_PATH" },
  model: { id: "m", prompt: "SECRET_PROMPT" },
  id: "c1",
  name: "bash",
  action: "bash",
  source: { id: "s", input: "SECRET_ARGUMENT" },
  title: "SECRET_TITLE",
  content: "SECRET_OUTPUT",
};

/**
 * The host surface the plugin touches: an event stream that stays open until
 * the plugin aborts it, and the session store.
 * @param {unknown[]} events
 * @param {Record<string, unknown>} [sessions]
 */
function host(events, sessions = {}) {
  return {
    event: {
      /** @param {{ signal: AbortSignal }} options */
      async *subscribe({ signal }) {
        yield* events;
        await new Promise((resolve) => signal.addEventListener("abort", resolve));
      },
    },
    session: {
      /** @param {{ sessionID: string }} query */
      get: async ({ sessionID }) => sessions[sessionID],
    },
  };
}

/**
 * @param {any} module
 * @param {ReturnType<typeof host>} ctx
 */
async function run(module, ctx) {
  const cleanup = await module.default.setup(ctx);
  return /** @type {() => Promise<void>} */ (cleanup);
}

/**
 * Each path of `value`'s leaves, dotted.
 * @param {Record<string, unknown>} value
 * @returns {string[]}
 */
function paths(value, prefix = "") {
  return Object.entries(value).flatMap(([key, v]) =>
    v !== null && typeof v === "object"
      ? paths(/** @type {Record<string, unknown>} */ (v), `${prefix}${key}.`)
      : v === undefined
        ? []
        : [`${prefix}${key}`],
  );
}

test("forwards each event of the contract once, copying exactly the decoded fields", { skip: !POSIX }, async (t) => {
  const { argvFile, module, outFile } = await render(t, SOURCE);
  const events = [
    { type: "message.part.updated", data: FULL },
    ...registers.map((type) => ({ type, location: { directory: "/r", secret: "SECRET_PATH" }, data: FULL })),
  ];
  const cleanup = await run(module, host(events));
  const payloads = await recorded(outFile, registers.length);
  await cleanup();

  assert.deepEqual(
    payloads.map((p) => p.type),
    registers,
  );
  for (const p of payloads) {
    assert.deepEqual(paths(p.data).sort(), [...dataFields].sort());
    assert.deepEqual(p.location, { directory: "/r" });
    assert.ok(!JSON.stringify(p).includes("SECRET"), JSON.stringify(p));
  }
  await assertShimContract(SOURCE, argvFile, payloads);
});

test("a turn's start carries what its session's record adds", { skip: !POSIX }, async (t) => {
  const { module, outFile } = await render(t, SOURCE);
  const events = [{ type: "session.execution.started", data: { sessionID: "ses_1" } }];
  const cleanup = await run(module, host(events, { ses_1: { id: "ses_1", parentID: "ses_0", title: "SECRET_TITLE" } }));
  const [start] = await recorded(outFile, 1);
  await cleanup();
  assert.deepEqual(start.data, { sessionID: "ses_1", parentID: "ses_0", id: "ses_1" });
});

test("cleanup waits for what is in flight before it aborts the stream", { skip: !POSIX }, async (t) => {
  const { module, outFile } = await render(t, SOURCE);
  const events = registers.map((type) => ({ type, data: { sessionID: "ses_1" } }));
  const cleanup = await run(module, host(events));
  await cleanup();
  assert.equal((await recorded(outFile, registers.length)).length, registers.length);
});

test("an unspawnable shim or an unreadable session never rejects into opencode", async (t) => {
  const { module } = await render(t, SOURCE, { shim: "missing" });
  const ctx = host(registers.map((type) => ({ type, data: { sessionID: "ses_1" } })));
  ctx.session.get = async () => {
    throw new Error("gone");
  };
  const cleanup = await run(module, ctx);
  await cleanup();
});
