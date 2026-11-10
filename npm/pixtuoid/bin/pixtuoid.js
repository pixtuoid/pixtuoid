#!/usr/bin/env node
"use strict";

// Launcher shim for the `pixtuoid` TUI. Resolves the native binary from the
// platform package and execs it transparently. An interactive TUI has no
// latency/exit-0 contract, so this may report an error and exit non-zero.

const { binaryPath, isShipped, packageName } = require("./resolve");

const bin = binaryPath("pixtuoid");
if (!bin) {
  const { platform, arch } = process;
  process.stderr.write(
    isShipped(platform, arch)
      ? "pixtuoid: npm left out the optional dependency " +
          packageName(platform, arch) +
          "; reinstall with optional dependencies included: `npm install -g pixtuoid --include=optional`\n"
      : "pixtuoid: no prebuilt binary for " +
          platform +
          "-" +
          arch +
          "; install from source: `brew install pixtuoid` or `cargo install pixtuoid pixtuoid-hook`\n"
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
