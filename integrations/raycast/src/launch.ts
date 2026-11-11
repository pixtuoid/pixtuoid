// Apart from `pixtuoid.ts`, which imports `@raycast/api`, so `node:test` can drive it.
import { spawn } from "node:child_process";

/** How long a boot may stay silent before it counts as hung and is killed:
 *  Raycast bounds a no-view command's memory, not its time
 *  (https://developers.raycast.com/information/lifecycle), so a hung boot would
 *  leave Start Floating pending for good. The margin over a real boot is my
 *  choice, not a measured bound. */
export const READY_DEADLINE_MS = 30_000;

/** A launch whose child exited before it said it was up. */
export class LaunchFailedError extends Error {
  readonly code: number | null;

  constructor(code: number | null, report: string) {
    super(report || `pixtuoid exited ${code === null ? "on a signal" : `with code ${code}`} before its window opened`);
    this.name = "LaunchFailedError";
    this.code = code;
  }
}

/** `stderr` from its last line that starts with `errorPrefix` (the CLI's fatal
 *  report), else all of it: a boot's earlier notices would bury the reason. */
function errorReport(stderr: string, errorPrefix: string): string {
  const lines = stderr.split("\n");
  const at = lines.findLastIndex((line) => line.startsWith(errorPrefix));
  return lines.slice(Math.max(at, 0)).join("\n").trim();
}

/** Spawn `bin` detached, so it outlives Raycast, and settle once it says it is
 *  up: its first newline on stdout, s6's readiness protocol
 *  (https://skarnet.org/software/s6/notifywhenup.html). An exit first rejects
 *  with its code and its stderr from the last line that starts with
 *  `errorPrefix`; silence past `deadlineMs` kills it and rejects. Once up, the
 *  child is let go: no pipe or ref of ours keeps this process alive. */
export function launchUntilReady(
  bin: string,
  args: string[],
  errorPrefix: string,
  deadlineMs = READY_DEADLINE_MS,
): Promise<void> {
  return new Promise((resolve, reject) => {
    const child = spawn(bin, args, { detached: true, stdio: ["ignore", "pipe", "pipe"] });
    const deadline = setTimeout(() => {
      child.kill();
      reject(new Error(`pixtuoid did not open its window within ${deadlineMs / 1000} s`));
    }, deadlineMs);
    let stderr = "";
    child.stderr.setEncoding("utf8");
    child.stderr.on("data", (chunk: string) => (stderr += chunk));
    child.stdout.on("data", (chunk: Buffer) => {
      if (!chunk.includes("\n")) return;
      clearTimeout(deadline);
      child.stdout.destroy();
      child.stderr.destroy();
      child.unref();
      resolve();
    });
    // An `error` event with no listener is thrown (https://nodejs.org/api/events.html#error-events);
    // after a settle, a reject is a no-op.
    child.once("error", (e) => {
      clearTimeout(deadline);
      reject(e);
    });
    child.once("close", (code) => {
      clearTimeout(deadline);
      reject(new LaunchFailedError(code, errorReport(stderr, errorPrefix)));
    });
  });
}
