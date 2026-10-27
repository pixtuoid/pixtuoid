// Drives a plugin template the way its host loads it, against a real recorder
// executable baked in as its shim, and holds it to the contract the Rust side
// writes (`plugin-contract.json`): the Rust tests can only see a template as
// text, so this is where a plugin's behaviour is executed.

import assert from "node:assert/strict";
import { chmod, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, extname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import contract from "../../crates/pixtuoid/src/install/plugin-contract.json" with { type: "json" };

/** @import { TestContext } from "node:test" */

const INSTALL_DIR = join(dirname(dirname(dirname(fileURLToPath(import.meta.url)))), "crates", "pixtuoid", "src", "install");
const POLL_DEADLINE_MS = 10_000;
const POLL_TICK_MS = 25;

export const POSIX = process.platform !== "win32";

/**
 * @param {string} source
 * @returns {{ source: string, file: string, registers: string[], consumes: string[], data_fields?: string[], turn_key?: string }}
 */
export function row(source) {
  const found = contract.plugins.find((p) => p.source === source);
  assert.ok(found, `plugin-contract.json has no ${source} row`);
  return found;
}

let bust = 0;

/**
 * `source`'s template rendered with a shim and imported fresh, so module state
 * never leaks between cases. The recorder appends each call's argv, stdin and
 * begin/end marks to files; `holdMs` keeps each call open so overlapping calls
 * interleave their marks; `"missing"` bakes a path that does not exist.
 * @param {TestContext} t
 * @param {string} source
 * @param {{ shim?: "recorder" | "missing", holdMs?: number }} [options]
 */
export async function render(t, source, { shim = "recorder", holdMs = 0 } = {}) {
  const root = await mkdtemp(join(tmpdir(), `pixtuoid-${source}-plugin-`));
  t.after(() => rm(root, { recursive: true, force: true }));
  const { file } = row(source);
  const template = await readFile(join(INSTALL_DIR, file), "utf8");
  assert.equal(template.split(contract.hook_placeholder).length, 2, "one hook placeholder, which the bake replaces");

  const outFile = join(root, "payloads.ndjson");
  const argvFile = join(root, "argv.txt");
  const traceFile = join(root, "trace.txt");
  const hookPath = join(root, shim === "missing" ? "absent-hook" : "recorder-hook");
  if (shim === "recorder") {
    const hold = holdMs ? `sleep ${holdMs / 1000}\n` : "";
    await writeFile(
      hookPath,
      `#!/bin/sh\necho begin >> ${JSON.stringify(traceFile)}\n${hold}` +
        `printf '%s\\n' "$*" >> ${JSON.stringify(argvFile)}\n` +
        `cat >> ${JSON.stringify(outFile)}\necho >> ${JSON.stringify(outFile)}\n` +
        `echo end >> ${JSON.stringify(traceFile)}\n`,
    );
    await chmod(hookPath, 0o755);
  }
  const entry = join(root, `index${extname(file)}`);
  await writeFile(entry, template.replace(contract.hook_placeholder, JSON.stringify(hookPath)));
  bust += 1;
  const module = await import(`${pathToFileURL(entry)}?v=${bust}`);
  return { argvFile, module, outFile, traceFile };
}

/**
 * `file`'s non-empty lines once `done` holds of them: a shim writes after the
 * handler that spawned it returns.
 * @param {string} file
 * @param {(lines: string[]) => boolean} done
 */
export async function poll(file, done) {
  const deadline = Date.now() + POLL_DEADLINE_MS;
  for (;;) {
    const text = await readFile(file, "utf8").catch(() => "");
    const lines = text.split("\n").filter((l) => l.trim().length > 0);
    if (done(lines)) return lines;
    if (Date.now() > deadline) throw new Error(`timed out polling ${file}; got ${lines.length} line(s)`);
    await new Promise((r) => setTimeout(r, POLL_TICK_MS));
  }
}

/**
 * The first `n` payloads the shim received.
 * @param {string} outFile
 * @param {number} n
 * @returns {Promise<any[]>} JSON, as the shim received it
 */
export async function recorded(outFile, n) {
  return (await poll(outFile, (lines) => lines.length >= n)).slice(0, n).map((l) => JSON.parse(l));
}

/**
 * The shim contract every plugin keeps: each call says which source it speaks
 * for, and each payload carries its host's pid.
 * @param {string} source
 * @param {string} argvFile
 * @param {Record<string, unknown>[]} payloads
 */
export async function assertShimContract(source, argvFile, payloads) {
  const argv = (await poll(argvFile, (lines) => lines.length >= payloads.length)).slice(0, payloads.length);
  assert.deepEqual(argv, Array(payloads.length).fill(`${contract.source_flag} ${source}`));
  for (const p of payloads) assert.equal(p[contract.pid_key], process.pid, `${JSON.stringify(p)} lacks the host pid`);
}

/**
 * What a plugin registered with its host is the contract's set exactly.
 * @param {string} source
 * @param {Iterable<string>} registered
 */
export function assertRegisters(source, registered) {
  assert.deepEqual([...registered].sort(), [...row(source).registers].sort());
}
