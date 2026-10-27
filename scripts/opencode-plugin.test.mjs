// Contract test for `crates/pixtuoid/src/install/opencode_plugin.ts`. The Rust
// side can only grep the template as a STRING; this drives the RENDERED plugin
// the way opencode's promise-plugin host does, against a real recorder
// executable, so the turn each payload names is the one the plugin computed.

import assert from "node:assert/strict";
import { chmod, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { pathToFileURL } from "node:url";

import { HOOK_PLACEHOLDER, POSIX, REPO_ROOT, poll } from "./lib/plugin-harness.mjs";

const PLUGIN_SOURCE = join(REPO_ROOT, "crates", "pixtuoid", "src", "install", "opencode_plugin.ts");

async function renderPlugin(t) {
  const root = await mkdtemp(join(tmpdir(), "pixtuoid-opencode-plugin-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const source = await readFile(PLUGIN_SOURCE, "utf8");
  assert.equal(source.split(HOOK_PLACEHOLDER).length, 2, "one hook placeholder");

  const outFile = join(root, "payloads.ndjson");
  const hookPath = join(root, "recorder-hook");
  await writeFile(hookPath, `#!/bin/sh\ncat >> ${JSON.stringify(outFile)}\necho >> ${JSON.stringify(outFile)}\n`);
  await chmod(hookPath, 0o755);

  const entry = join(root, "pixtuoid.ts");
  await writeFile(entry, source.replace(HOOK_PLACEHOLDER, JSON.stringify(hookPath)));
  const module = await import(pathToFileURL(entry).href);
  return { plugin: module.default, outFile };
}

// The bus as `ctx.event.subscribe()` yields it: pushed events, in order.
function bus() {
  const queued = [];
  let wake;
  return {
    push(event) {
      queued.push(event);
      wake?.();
    },
    async *[Symbol.asyncIterator]() {
      for (;;) {
        if (queued.length > 0) {
          yield queued.shift();
          continue;
        }
        await new Promise((resolve) => (wake = resolve));
        wake = undefined;
      }
    },
  };
}

// The host surface the plugin touches: the event bus and a session's record.
function setup(plugin) {
  const events = bus();
  const cleanup = plugin.setup({
    event: { subscribe: () => events },
    session: { get: async () => ({}) },
  });
  return { events, cleanup };
}

const payloads = async (file, n) =>
  (await poll(file, (got) => got.length >= n)).slice(0, n).map((l) => JSON.parse(l));

const started = (sessionID, seq) => ({
  type: "session.execution.started",
  durable: seq === undefined ? undefined : { aggregateID: sessionID, seq, version: 1 },
  data: { sessionID },
});
const settled = (type, sessionID, seq) => ({
  type: `session.execution.${type}`,
  durable: { aggregateID: sessionID, seq, version: 1 },
  data: { sessionID },
});

test("each settle names its start's seq, and one whose start went unseen names none", { skip: !POSIX }, async (t) => {
  const { plugin, outFile } = await renderPlugin(t);
  const { events, cleanup } = setup(plugin);
  events.push(settled("failed", "ses_a", 2));
  events.push(started("ses_a", 4));
  events.push(started("ses_b", 9));
  events.push({ type: "session.tool.input.started", data: { sessionID: "ses_a", id: "c1", name: "bash" } });
  events.push(settled("succeeded", "ses_a", 7));
  events.push(settled("interrupted", "ses_b", 11));
  events.push(settled("succeeded", "ses_b", 12));

  const got = (await payloads(outFile, 7)).map((p) => [p.type, p.data.sessionID, p.turn]);
  assert.deepEqual(got, [
    ["session.execution.failed", "ses_a", undefined],
    ["session.execution.started", "ses_a", 4],
    ["session.execution.started", "ses_b", 9],
    ["session.tool.input.started", "ses_a", undefined],
    ["session.execution.succeeded", "ses_a", 4],
    ["session.execution.interrupted", "ses_b", 9],
    ["session.execution.succeeded", "ses_b", undefined],
  ]);
  await cleanup();
});

test("a start with no durable seq names no turn, nor does its settle", { skip: !POSIX }, async (t) => {
  const { plugin, outFile } = await renderPlugin(t);
  const { events, cleanup } = setup(plugin);
  events.push(started("ses_a", 3));
  events.push(started("ses_a", undefined));
  events.push(settled("succeeded", "ses_a", 5));

  const got = (await payloads(outFile, 3)).map((p) => p.turn);
  assert.deepEqual(got, [3, undefined, undefined]);
  await cleanup();
});
