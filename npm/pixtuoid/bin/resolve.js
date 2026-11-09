"use strict";

// Shared platform → native-binary resolution for the two launcher shims: npm
// installs exactly the ONE `@pixtuoid/cli-<platform>-<arch>` optionalDependency
// that matches the host (filtered by each platform package's `os`/`cpu`/`libc`),
// and we `require.resolve` the native binary out of it. No postinstall, no
// download.

const fs = require("fs");
const path = require("path");

// npm matches `os`/`cpu` against `process.platform`/`process.arch`
// (https://docs.npmjs.com/cli/v11/configuring-npm/package-json#os), and
// generate.mjs names each package it publishes here, so this names the one npm
// installed. One package per os/cpu pair means no runtime gnu-vs-musl choice.
/**
 * @param {string} platform
 * @param {string} arch
 */
function packageName(platform, arch) {
  return "@pixtuoid/cli-" + platform + "-" + arch;
}

/**
 * @param {string} bin
 * @param {string} [platform]
 */
function exeName(bin, platform = process.platform) {
  return bin + (platform === "win32" ? ".exe" : "");
}

// Returns null if this platform isn't shipped — callers decide how to handle
// that (the TUI errors; the hook stays silent).
/**
 * @param {string} bin
 * @returns {string | null}
 */
function binaryPath(bin) {
  // Dev / CI override: PIXTUOID_BINARY points at a directory holding the
  // binaries, or at the pixtuoid binary itself.
  const override = process.env.PIXTUOID_BINARY;
  if (override) {
    // One guarded statSync (not existsSync + statSync) — the two-syscall form
    // can throw ENOENT on a delete-between-calls race, and binaryPath() must
    // return null, never throw (the hook shim relies on it).
    let st = null;
    try {
      st = fs.statSync(override);
    } catch (_e) {
      st = null;
    }
    const dir = st && st.isDirectory() ? override : path.dirname(override);
    const p = path.join(dir, exeName(bin));
    if (fs.existsSync(p)) return p;
  }

  try {
    return require.resolve(packageName(process.platform, process.arch) + "/" + exeName(bin));
  } catch (_e) {
    return null;
  }
}

module.exports = { binaryPath, exeName, packageName };
