import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { LaunchFailedError, launchUntilReady } from "../src/launch.ts";

/** `launchUntilReady` of a node child running `script`. */
const launch = (script) => launchUntilReady(process.execPath, ["-e", script]);

// In a process of its own, which must exit while the child it launched runs on,
// as Raycast's does once the command returns.
test("a launch settles once its child says it is up, and the child outlives the launcher", () => {
  const pidFile = join(mkdtempSync(join(tmpdir(), "launch-")), "pid");
  const child =
    "require('fs').writeFileSync(process.argv[1], String(process.pid));" +
    "process.stdout.write('\\n'); setInterval(() => {}, 1000);";
  const launcher =
    `import { launchUntilReady } from ${JSON.stringify(new URL("../src/launch.ts", import.meta.url).href)};` +
    `await launchUntilReady(process.execPath, ["-e", ${JSON.stringify(child)}, ${JSON.stringify(pidFile)}]);`;
  const run = spawnSync(process.execPath, ["--input-type=module", "-e", launcher], { timeout: 10_000 });
  const pid = Number(readFileSync(pidFile, "utf8"));
  try {
    assert.equal(run.status, 0, `the launcher did not exit on its own: ${run.signal ?? run.stderr}`);
    assert.doesNotThrow(() => process.kill(pid, 0), "the child must outlive the launcher");
  } finally {
    // Detached, so a failed run would leave it running.
    try {
      process.kill(pid);
    } catch {
      // Already gone.
    }
  }
});

test("a child that exits before it is up fails the launch with its code and stderr", async () => {
  await assert.rejects(launch("process.stderr.write('Error: no adapter\\n'); process.exit(1);"), (e) => {
    assert.ok(e instanceof LaunchFailedError);
    assert.equal(e.code, 1);
    assert.match(e.message, /Error: no adapter/);
    assert.equal(e.outdated, false);
    return true;
  });
});

test("a binary that rejects the command line is one to update", async () => {
  await assert.rejects(
    launch("process.stderr.write(\"error: unexpected argument '--notify-ready'\\n\"); process.exit(2);"),
    (e) => {
      assert.ok(e instanceof LaunchFailedError);
      assert.equal(e.outdated, true);
      return true;
    },
  );
});

test("a binary that cannot be spawned fails the launch", async () => {
  await assert.rejects(launchUntilReady(join(tmpdir(), "no-such-pixtuoid"), []), { code: "ENOENT" });
});
