"use strict";

// Shared platform → native-binary resolution for the two launcher shims: npm
// installs exactly the ONE `@pixtuoid/cli-<platform>-<arch>` optionalDependency
// that matches the host (filtered by each platform package's `os`/`cpu`/`libc`),
// and we `require.resolve` the native binary out of it. No postinstall, no
// download.

const fs = require("fs");
const path = require("path");

// The matrix is ASYMMETRIC, so there is no runtime gnu-vs-musl choice here:
// linux-x64 ships a STATIC musl build and declares no `libc`; linux-arm64 ships
// a glibc build and declares `libc:["glibc"]`, so npm skips it on musl/Alpine
// rather than installing a binary that would crash. The os/cpu/libc filtering
// happens at npm INSTALL time; here we just map.
/** @type {Record<string, string>} */
const PACKAGE = {
  "darwin arm64": "@pixtuoid/cli-darwin-arm64",
  "darwin x64": "@pixtuoid/cli-darwin-x64",
  "linux x64": "@pixtuoid/cli-linux-x64",
  "linux arm64": "@pixtuoid/cli-linux-arm64",
  "win32 x64": "@pixtuoid/cli-win32-x64",
  "win32 arm64": "@pixtuoid/cli-win32-arm64",
};

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

  const pkg = PACKAGE[process.platform + " " + process.arch];
  if (!pkg) return null;
  try {
    return require.resolve(pkg + "/" + exeName(bin));
  } catch (_e) {
    return null;
  }
}

module.exports = { binaryPath, exeName };
