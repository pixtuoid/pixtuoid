// Contract test for `crates/pixtuoid/src/install/opencode_plugin.ts`: drives the
// rendered plugin the way opencode's promise host does, over a scripted event
// stream and a real recorder.

import assert from "node:assert/strict";
import test from "node:test";
import { POSIX, assertShimContract, recorded, render, row } from "./lib/plugin-harness.mjs";

const SOURCE = "opencode";
const { registers, data_fields: dataFields = [], turn_key: turnKey = "" } = row(SOURCE);

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

/**
 * `data` holding every field: each path reads back as itself, so a payload
 * holds exactly the paths the plugin copies, whichever those are.
 * @param {string} [prefix]
 * @returns {any}
 */
function everyField(prefix = "") {
  return new Proxy(
    {},
    {
      get: (_, key) =>
        key === "toJSON" ? () => prefix.slice(0, -1) : typeof key === "string" ? everyField(`${prefix}${key}.`) : undefined,
    },
  );
}

test("copies the decoded fields and nothing more, whatever an event carries", { skip: !POSIX }, async (t) => {
  const { module, outFile } = await render(t, SOURCE);
  const cleanup = await run(module, host([{ type: "session.tool.success", data: everyField() }]));
  const [payload] = await recorded(outFile, 1);
  await cleanup();
  assert.deepEqual(paths(payload.data).sort(), [...dataFields].sort());
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

/**
 * @param {string} sessionID
 * @param {number | undefined} seq
 */
const started = (sessionID, seq) => ({
  type: "session.execution.started",
  durable: seq === undefined ? undefined : { aggregateID: sessionID, seq, version: 1 },
  data: { sessionID },
});
/**
 * @param {string} type
 * @param {string} sessionID
 * @param {number} seq
 */
const settled = (type, sessionID, seq) => ({
  type: `session.execution.${type}`,
  durable: { aggregateID: sessionID, seq, version: 1 },
  data: { sessionID },
});

test("each settle names its start's seq, and one whose start went unseen names none", { skip: !POSIX }, async (t) => {
  const { module, outFile } = await render(t, SOURCE);
  const cleanup = await run(
    module,
    host([
      settled("failed", "ses_a", 2),
      started("ses_a", 4),
      started("ses_b", 9),
      { type: "session.tool.input.started", data: { sessionID: "ses_a", id: "c1", name: "bash" } },
      settled("succeeded", "ses_a", 7),
      settled("interrupted", "ses_b", 11),
      settled("succeeded", "ses_b", 12),
    ]),
  );
  const got = (await recorded(outFile, 7)).map((p) => [p.type, p.data.sessionID, p[turnKey]]);
  await cleanup();
  assert.deepEqual(got, [
    ["session.execution.failed", "ses_a", undefined],
    ["session.execution.started", "ses_a", 4],
    ["session.execution.started", "ses_b", 9],
    ["session.tool.input.started", "ses_a", undefined],
    ["session.execution.succeeded", "ses_a", 4],
    ["session.execution.interrupted", "ses_b", 9],
    ["session.execution.succeeded", "ses_b", undefined],
  ]);
});

test("a start with no durable seq names no turn, nor does its settle", { skip: !POSIX }, async (t) => {
  const { module, outFile } = await render(t, SOURCE);
  const cleanup = await run(
    module,
    host([started("ses_a", 3), started("ses_a", undefined), settled("succeeded", "ses_a", 5)]),
  );
  const got = (await recorded(outFile, 3)).map((p) => p[turnKey]);
  await cleanup();
  assert.deepEqual(got, [3, undefined, undefined]);
});
