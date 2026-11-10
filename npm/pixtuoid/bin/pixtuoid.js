#!/usr/bin/env node
"use strict";

// Launcher shim for the `pixtuoid` TUI. Resolves the native binary from the
// platform package and execs it transparently. An interactive TUI has no
// latency/exit-0 contract, so this may report an error and exit non-zero.

const { binaryPath, hostPackage } = require("./resolve");

const bin = binaryPath("pixtuoid");
if (!bin) {
  const cargo = "`cargo install pixtuoid pixtuoid-hook`";
  process.stderr.write(
    "pixtuoid: no prebuilt binary for " +
      process.platform +
      "-" +
      process.arch +
      ".\n" +
      (hostPackage()
        ? "Install with " + cargo + ", or download a build:\n" +
          "https://github.com/pixtuoid/pixtuoid/releases/latest\n"
        : "Install with `brew install pixtuoid` or " + cargo + ".\n")
  );
  process.exit(1);
}

const result = require("child_process").spawnSync(bin, process.argv.slice(2), {
  stdio: "inherit",
  shell: false,
});
if (result.error) {
  process.stderr.write("pixtuoid: " + result.error.message + "\n");
  process.exit(1);
}
process.exit(result.status == null ? 1 : result.status);
