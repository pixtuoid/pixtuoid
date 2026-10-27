import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";

export const REPO_ROOT = fileURLToPath(new URL("../../", import.meta.url));
// Must match each bridge's `HOOK_PLACEHOLDER` in `crates/pixtuoid/src/install/`.
export const HOOK_PLACEHOLDER = '"{{HOOK_PATH_JSON}}"';
export const POSIX = process.platform !== "win32";

const POLL_DEADLINE_MS = 10_000;
const POLL_TICK_MS = 25;

// `file`'s non-empty lines once `done` holds of them: the shims write after the
// listeners return.
export async function poll(file, done) {
  const deadline = Date.now() + POLL_DEADLINE_MS;
  for (;;) {
    const text = await readFile(file, "utf8").catch(() => "");
    const got = text.split("\n").filter((l) => l.trim().length > 0);
    if (done(got)) return got;
    if (Date.now() > deadline) {
      throw new Error(`timed out polling ${file}; got ${got.length} line(s)`);
    }
    await new Promise((r) => setTimeout(r, POLL_TICK_MS));
  }
}
