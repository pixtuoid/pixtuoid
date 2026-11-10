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
// generate.mjs names each package it publishes here, so this names the one
// npm installed.
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

// Returns null when no binary resolves — callers decide how to handle that
// (the TUI errors; the hook stays silent).
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

// generate.mjs stamps the launcher's optionalDependencies and their libc gates
// from TARGETS. A platform they don't name, or whose libc gate npm would refuse
// here, has no prebuilt package; one that passes but didn't resolve was left
// out by the install, which may omit optional dependencies
// (https://docs.npmjs.com/cli/v11/commands/npm-install#omit).
/**
 * @param {string} platform
 * @param {string} arch
 */
function isShipped(platform, arch) {
  const manifest = JSON.parse(fs.readFileSync(path.join(__dirname, "..", "package.json"), "utf8"));
  const name = packageName(platform, arch);
  /** @type {string[] | undefined} */
  const libc = manifest.platformLibc?.[name];
  return Object.hasOwn(manifest.optionalDependencies ?? {}, name) && (!libc || libc.includes(String(hostLibc())));
}

// npm-install-checks v9.0.0's reading of the host libc from Node's report
// (`lib/current-env.js:33-46`), the check npm gates a `libc` package by.
function hostLibc() {
  if (process.platform !== "linux") return undefined;
  // As npm does; @types/node doesn't declare the setting yet.
  Object.assign(process.report, { excludeNetwork: true });
  const report = /** @type {{ header?: { glibcVersionRuntime?: string }, sharedObjects?: string[] }} */ (
    process.report.getReport()
  );
  if (report.header?.glibcVersionRuntime) return "glibc";
  const musl = (/** @type {string} */ file) => file.includes("libc.musl-") || file.includes("ld-musl-");
  return Array.isArray(report.sharedObjects) && report.sharedObjects.some(musl) ? "musl" : null;
}

module.exports = { binaryPath, exeName, isShipped, packageName };
