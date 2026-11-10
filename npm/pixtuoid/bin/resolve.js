"use strict";

// Shared platform → native-binary resolution for the two launcher shims: npm
// installs exactly the ONE `@pixtuoid/cli-<platform>-<arch>` optionalDependency
// that matches the host (filtered by each platform package's `os`/`cpu`),
// and we `require.resolve` the native binary out of it. No postinstall, no
// download.

const fs = require("fs");
const path = require("path");

/** @type {Record<string, string>} */
const PACKAGE = {
  "darwin arm64": "@pixtuoid/cli-darwin-arm64",
  "darwin x64": "@pixtuoid/cli-darwin-x64",
  "win32 x64": "@pixtuoid/cli-win32-x64",
  "win32 arm64": "@pixtuoid/cli-win32-arm64",
};

// The platform package release.yml builds for this host, or null where it
// builds none, so there is no download to point at either.
/** @returns {string | null} */
function hostPackage() {
  const key = process.platform + " " + process.arch;
  return Object.hasOwn(PACKAGE, key) ? PACKAGE[key] : null;
}

/** @param {string} bin */
function exeName(bin) {
  return bin + (process.platform === "win32" ? ".exe" : "");
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

  const pkg = hostPackage();
  if (!pkg) return null;
  try {
    return require.resolve(pkg + "/" + exeName(bin));
  } catch (_e) {
    return null;
  }
}

module.exports = { binaryPath, exeName, hostPackage };
