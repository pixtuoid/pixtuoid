#!/usr/bin/env node
"use strict";

// Launcher shim for `pixtuoid-hook` — the shim CC/Codex/Reasonix invoke on every
// hook event. INVARIANT #5 is sacred here: NEVER block the agent. So this shim
// ALWAYS exits 0, NEVER writes to stderr, and NEVER throws — on any failure it
// exits 0 silently (the visualizer just misses that event; the agent is never
// blocked). It forwards the JSON payload (stdin) to the native shim, whose send
// its `transport::WRITE_TIMEOUT` watchdog bounds.
//
// NOTE: a Node hop adds ~tens of ms in front of the native shim. That stays well
// within CC's hook tolerance, but the latency-optimal path is to have the
// Connection panel write the ABSOLUTE path of the sibling native pixtuoid-hook
// (zero Node hop) — tracked as a follow-up. Until then, do not "optimize" this
// shim into anything that could exit non-zero or print on error.

try {
  const { binaryPath } = require("./resolve");
  const bin = binaryPath("pixtuoid-hook");
  if (!bin) {
    process.exit(0);
  }
  require("child_process").spawnSync(bin, process.argv.slice(2), {
    stdio: "inherit",
    shell: false,
  });
  // Exit 0 UNCONDITIONALLY. The hook's exit code is not consumed by the
  // visualizer — only "never block the agent" matters — so invariant #5 holds
  // STRUCTURALLY here, not by depending on the native binary always exiting 0.
  process.exit(0);
} catch (_e) {
  process.exit(0);
}
