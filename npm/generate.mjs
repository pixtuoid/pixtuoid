#!/usr/bin/env node
// Generate the per-platform `@pixtuoid/cli-*` npm packages + stamp the launcher.
//
// Usage: node npm/generate.mjs --version X.Y.Z --artifacts <dir> [--npm-dir <dir>]
// Writes into the npm/ tree in place; --npm-dir points it at a temp copy so the
// test doesn't mutate the tracked launcher.

import {
  existsSync,
  mkdirSync,
  copyFileSync,
  cpSync,
  chmodSync,
  readFileSync,
  writeFileSync,
  rmSync,
  statSync,
} from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";

const SCOPE = "@pixtuoid";
const BINS = ["pixtuoid", "pixtuoid-hook"];
// What the release archive carries beside the binaries (`just stage-notices`):
// the license and every embedded font's notice, which travel with each copy.
const NOTICES = ["LICENSE", "licenses"];

// Must stay equal to release.yml's build matrix; npm/generate.test.mjs pins that,
// so a missing target fails `just npm-check` instead of silently shipping nowhere.
// linux-x64 is a static musl build (portable to glibc too), so it takes NO `libc`
// gate; linux-arm64 is glibc, so `libc:["glibc"]` makes npm SKIP it on Alpine-arm64
// rather than install a binary that would crash there.
const TARGETS = [
  { rust: "aarch64-apple-darwin", pkg: "darwin-arm64", os: "darwin", cpu: "arm64" },
  { rust: "x86_64-apple-darwin", pkg: "darwin-x64", os: "darwin", cpu: "x64" },
  { rust: "x86_64-unknown-linux-musl", pkg: "linux-x64", os: "linux", cpu: "x64" },
  { rust: "aarch64-unknown-linux-gnu", pkg: "linux-arm64", os: "linux", cpu: "arm64", libc: ["glibc"] },
  { rust: "x86_64-pc-windows-msvc", pkg: "win32-x64", os: "win32", cpu: "x64" },
  { rust: "aarch64-pc-windows-msvc", pkg: "win32-arm64", os: "win32", cpu: "arm64" },
];

const {
  values: { version, artifacts, "npm-dir": NPM_DIR },
} = parseArgs({
  options: {
    version: { type: "string" },
    artifacts: { type: "string" },
    "npm-dir": { type: "string", default: dirname(fileURLToPath(import.meta.url)) },
  },
});
if (!version || !artifacts) {
  console.error("usage: node npm/generate.mjs --version X.Y.Z --artifacts <dir>");
  process.exit(1);
}
if (!/^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$/.test(version)) {
  console.error(`refusing to stamp a non-semver version: ${version}`);
  process.exit(1);
}

// Copy `NOTICES` from a target's extracted archive into `pkgDir`.
/**
 * @param {string} from
 * @param {string} pkgDir
 */
function copyNotices(from, pkgDir) {
  for (const notice of NOTICES) {
    const src = join(from, notice);
    if (!existsSync(src)) throw new Error(`missing ${notice} in ${from}`);
    cpSync(src, join(pkgDir, notice), { recursive: true });
  }
}

const launcherPath = join(NPM_DIR, "pixtuoid", "package.json");
const launcher = JSON.parse(readFileSync(launcherPath, "utf8"));

for (const t of TARGETS) {
  const isWin = t.os === "win32";
  const pkgName = `${SCOPE}/cli-${t.pkg}`;
  const pkgDir = join(NPM_DIR, SCOPE, `cli-${t.pkg}`);
  rmSync(pkgDir, { recursive: true, force: true });
  mkdirSync(pkgDir, { recursive: true });

  const files = [];
  for (const bin of BINS) {
    const exe = bin + (isWin ? ".exe" : "");
    const src = join(artifacts, t.rust, exe);
    if (!existsSync(src) || !statSync(src).isFile()) {
      throw new Error(`missing prebuilt binary for ${t.rust}: ${src}`);
    }
    const dst = join(pkgDir, exe);
    copyFileSync(src, dst);
    // the upload/download-artifact round-trip strips the exec bit — restore it.
    if (!isWin) chmodSync(dst, 0o755);
    files.push(exe);
  }
  copyNotices(join(artifacts, t.rust), pkgDir);
  files.push(...NOTICES);

  const pkg = {
    name: pkgName,
    version,
    description: `pixtuoid prebuilt binaries for ${t.pkg}`,
    license: launcher.license,
    repository: launcher.repository,
    homepage: launcher.homepage,
    os: [t.os],
    cpu: [t.cpu],
    ...(t.libc ? { libc: t.libc } : {}),
    files,
  };
  writeFileSync(join(pkgDir, "package.json"), JSON.stringify(pkg, null, 2) + "\n");
  launcher.optionalDependencies[pkgName] = version;
  console.log(`generated ${pkgName}@${version} (${files.join(", ")})`);
}

// The launcher ships no binary, but its license field names the fonts too.
copyNotices(join(artifacts, TARGETS[0].rust), dirname(launcherPath));
launcher.version = version;
writeFileSync(launcherPath, JSON.stringify(launcher, null, 2) + "\n");
console.log(`stamped launcher pixtuoid@${version} (${TARGETS.length} optionalDependencies)`);
