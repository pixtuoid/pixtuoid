// Apart from `pixtuoid.ts`, which imports `@raycast/api`, so `node:test` can drive it.
import { spawn } from "node:child_process";

/** clap's exit code for a command line it rejects (clap_builder 4.6.7
 *  `src/util/mod.rs:32` `USAGE_CODE`): what a binary older than a flag the
 *  launch passes exits with. */
const USAGE_EXIT = 2;

/** A launch whose child exited before it said it was up. */
export class LaunchFailedError extends Error {
  readonly code: number | null;

  constructor(code: number | null, stderr: string) {
    super(
      stderr.trim() ||
        `pixtuoid exited ${code === null ? "on a signal" : `with code ${code}`} before its window opened`,
    );
    this.name = "LaunchFailedError";
    this.code = code;
  }

  /** The binary predates the command line the launch passes. */
  get outdated(): boolean {
    return this.code === USAGE_EXIT;
  }
}

/** Spawn `bin` detached, so it outlives Raycast, and settle once it says it is
 *  up: its first newline on stdout, s6's readiness protocol
 *  (https://skarnet.org/software/s6/notifywhenup.html). An exit first rejects
 *  with its code and stderr. Once up, the child is let go: no pipe or ref of
 *  ours keeps this process alive. */
export function launchUntilReady(bin: string, args: string[]): Promise<void> {
  return new Promise((resolve, reject) => {
    const child = spawn(bin, args, { detached: true, stdio: ["ignore", "pipe", "pipe"] });
    let stderr = "";
    child.stderr.setEncoding("utf8");
    child.stderr.on("data", (chunk: string) => (stderr += chunk));
    child.stdout.on("data", (chunk: Buffer) => {
      if (!chunk.includes("\n")) return;
      child.stdout.destroy();
      child.stderr.destroy();
      child.unref();
      resolve();
    });
    // Without it a spawn failure is an uncaught event; after a settle, a no-op.
    child.once("error", reject);
    child.once("close", (code) => reject(new LaunchFailedError(code, stderr)));
  });
}
