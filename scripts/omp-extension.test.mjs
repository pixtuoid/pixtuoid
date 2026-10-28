// Contract test for `crates/pixtuoid/src/install/omp_extension.ts`: drives the
// rendered extension the way omp's loader does, with `Bun.spawn` as Bun
// documents it (bun.com/docs/runtime/child-process) over a real recorder.

import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import test from "node:test";
import { POSIX, assertRegisters, assertShimContract, recorded, render } from "./lib/plugin-harness.mjs";

const SOURCE = "omp";

Object.assign(globalThis, {
  Bun: {
    /**
     * @param {string[]} cmd
     * @param {{ stdin: Uint8Array }} options
     */
    spawn([file, ...args], { stdin }) {
      const child = spawn(file, args, { stdio: ["pipe", "ignore", "ignore"] });
      const exited = new Promise((resolve) => {
        child.on("close", resolve);
        child.on("error", () => resolve(1));
      });
      child.stdin.on("error", () => {});
      child.stdin.end(stdin);
      return { exited };
    },
  },
});

const ctx = {
  cwd: "/r",
  sessionManager: { getSessionFile: () => "/h/.omp/s.jsonl", getSessionId: () => "u" },
};

/**
 * The extension API surface it touches: `on`, recording each registration.
 * @param {any} module
 * @param {(name: string) => void} [refuse] throws for a name this omp lacks
 */
function load(module, refuse = () => {}) {
  /** @type {Record<string, (ev: unknown, ctx: unknown) => Promise<void>>} */
  const handlers = {};
  module.default({
    /** @param {string} name @param {(ev: unknown, ctx: unknown) => Promise<void>} handler */
    on(name, handler) {
      refuse(name);
      handlers[name] = handler;
    },
  });
  return handlers;
}

test("registers exactly the contract's events", async (t) => {
  const { module } = await render(t, SOURCE);
  assertRegisters(SOURCE, Object.keys(load(module)));
});

test("one event omp lacks leaves the others registered", async (t) => {
  const { module } = await render(t, SOURCE);
  const handlers = load(module, (name) => {
    if (name === "session_branch") throw new Error(`unknown event ${name}`);
  });
  assert.equal(Object.keys(handlers).length, Object.keys(load(module)).length - 1);
});

test("each event reaches the shim as its own type, with only the allowlist", { skip: !POSIX }, async (t) => {
  const { argvFile, module, outFile } = await render(t, SOURCE);
  const handlers = load(module);
  const secret = { messages: ["SECRET_MESSAGE"], arguments: { command: "SECRET_ARGUMENT" }, result: "SECRET_RESULT" };
  for (const name of Object.keys(handlers)) {
    await handlers[name](
      { ...secret, type: name, previousSessionFile: "/h/.omp/p.jsonl", toolCallId: "c1", toolName: "bash", reason: "why", approved: true },
      ctx,
    );
  }
  const payloads = await recorded(outFile);
  assert.deepEqual(
    payloads.map((p) => p.type),
    Object.keys(handlers),
  );
  const at = { sessionFile: "/h/.omp/s.jsonl", sessionId: "u", cwd: "/r", _pid: process.pid };
  const fields = { previousSessionFile: "/h/.omp/p.jsonl", toolCallId: "c1", toolName: "bash", reason: "why", approved: true };
  for (const p of payloads) assert.deepEqual(p, { type: p.type, ...at, ...fields });
  await assertShimContract(SOURCE, argvFile, payloads);
});

test("a broken session manager or shim never rejects into omp", async (t) => {
  const { module } = await render(t, SOURCE, { shim: "missing" });
  const handlers = load(module);
  const broken = {
    sessionManager: {
      getSessionFile() {
        throw new Error("gone");
      },
    },
  };
  for (const name of Object.keys(handlers)) await handlers[name]({ type: name }, broken);
});
