#!/usr/bin/env node
// Tests for generate.mjs — the per-platform package generator; ci-lint.yml's
// `npm-gen` job runs them via `just npm-check`.

import { test } from "node:test";
import assert from "node:assert/strict";
import {
  mkdtempSync,
  mkdirSync,
  writeFileSync,
  readFileSync,
  existsSync,
  rmSync,
} from "node:fs";
import { join, dirname } from "node:path";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const SCRIPT = join(dirname(fileURLToPath(import.meta.url)), "generate.mjs");

// The same matrix generate.mjs ships, transcribed independently so the test fails
// loudly if the production table is edited without updating the contract.
const TARGETS = [
  { rust: "aarch64-apple-darwin", pkg: "darwin-arm64", os: "darwin", cpu: "arm64", win: false },
  { rust: "x86_64-apple-darwin", pkg: "darwin-x64", os: "darwin", cpu: "x64", win: false },
  { rust: "x86_64-unknown-linux-musl", pkg: "linux-x64", os: "linux", cpu: "x64", win: false },
  { rust: "aarch64-unknown-linux-gnu", pkg: "linux-arm64", os: "linux", cpu: "arm64", win: false, libc: ["glibc"] },
  { rust: "x86_64-pc-windows-msvc", pkg: "win32-x64", os: "win32", cpu: "x64", win: true },
  { rust: "aarch64-pc-windows-msvc", pkg: "win32-arm64", os: "win32", cpu: "arm64", win: true },
];

// A release archive's notices as `just stage-notices` lays them out.
const NOTICE_FILES = ["LICENSE", join("licenses", "pixtuoid", "fonts", "OFL-Font.txt")];

/**
 * @param {import("node:test").TestContext} t
 * @param {{ skip?: string, skipNotices?: string }} [omit]
 */
function scaffold(t, { skip, skipNotices } = {}) {
  const dir = mkdtempSync(join(tmpdir(), "pixtuoid-gen-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));

  const npmDir = join(dir, "npm");
  mkdirSync(join(npmDir, "pixtuoid"), { recursive: true });
  writeFileSync(
    join(npmDir, "pixtuoid", "package.json"),
    JSON.stringify(
      {
        name: "pixtuoid",
        version: "0.0.0",
        license: "MIT",
        repository: { type: "git", url: "git+https://example/x.git" },
        homepage: "https://example/x",
        optionalDependencies: {},
      },
      null,
      2,
    ),
  );

  const artifacts = join(dir, "artifacts");
  for (const tg of TARGETS) {
    if (tg.rust === skip) continue;
    const tdir = join(artifacts, tg.rust);
    mkdirSync(tdir, { recursive: true });
    const ext = tg.win ? ".exe" : "";
    writeFileSync(join(tdir, "pixtuoid" + ext), "fake-tui");
    writeFileSync(join(tdir, "pixtuoid-hook" + ext), "fake-hook");
    if (tg.rust === skipNotices) continue;
    for (const f of NOTICE_FILES) {
      mkdirSync(dirname(join(tdir, f)), { recursive: true });
      writeFileSync(join(tdir, f), `notice ${f}`);
    }
  }
  return { dir, npmDir, artifacts };
}

/**
 * @param {string} npmDir
 * @param {string} artifacts
 * @param {string} version
 * @param {...string} extra
 */
function run(npmDir, artifacts, version, ...extra) {
  return spawnSync(
    process.execPath,
    [SCRIPT, "--version", version, "--artifacts", artifacts, "--npm-dir", npmDir, ...extra],
    { encoding: "utf8" },
  );
}

test("refuses a flag it does not know rather than ignore it", (t) => {
  const { npmDir, artifacts } = scaffold(t);
  const r = run(npmDir, artifacts, "1.2.3", "--bogus");
  assert.notEqual(r.status, 0);
  assert.match(r.stderr, /--bogus/);
});

test("stamps launcher + all 6 platform packages with the asymmetric libc gate", (t) => {
  const { npmDir, artifacts } = scaffold(t);
  const r = run(npmDir, artifacts, "1.2.3");
  assert.equal(r.status, 0, r.stderr);

  const launcher = JSON.parse(readFileSync(join(npmDir, "pixtuoid", "package.json"), "utf8"));
  assert.equal(launcher.version, "1.2.3", "launcher version stamped");

  for (const tg of TARGETS) {
    const name = `@pixtuoid/cli-${tg.pkg}`;
    assert.equal(launcher.optionalDependencies[name], "1.2.3", `${name} pinned in launcher`);

    const pkgPath = join(npmDir, "@pixtuoid", `cli-${tg.pkg}`, "package.json");
    assert.ok(existsSync(pkgPath), `${name} package.json written`);
    const pkg = JSON.parse(readFileSync(pkgPath, "utf8"));
    assert.equal(pkg.version, "1.2.3");
    assert.deepEqual(pkg.os, [tg.os]);
    assert.deepEqual(pkg.cpu, [tg.cpu]);

    if (tg.libc) assert.deepEqual(pkg.libc, tg.libc, `${name} declares libc`);
    else assert.equal("libc" in pkg, false, `${name} omits libc`);

    const ext = tg.win ? ".exe" : "";
    assert.deepEqual(
      pkg.files.sort(),
      ["pixtuoid" + ext, "pixtuoid-hook" + ext, "LICENSE", "licenses"].sort(),
    );
    for (const f of pkg.files) {
      assert.ok(existsSync(join(npmDir, "@pixtuoid", `cli-${tg.pkg}`, f)), `${f} copied`);
    }
    for (const f of NOTICE_FILES) {
      assert.equal(
        readFileSync(join(npmDir, "@pixtuoid", `cli-${tg.pkg}`, f), "utf8"),
        `notice ${f}`,
        `${name} carries ${f}`,
      );
    }
  }
  for (const f of NOTICE_FILES) {
    assert.ok(existsSync(join(npmDir, "pixtuoid", f)), `launcher carries ${f}`);
  }
});

test("throws when a target's archive lacks the notices", (t) => {
  const { npmDir, artifacts } = scaffold(t, { skipNotices: "x86_64-pc-windows-msvc" });
  const r = run(npmDir, artifacts, "1.2.3");
  assert.notEqual(r.status, 0);
  assert.match(r.stderr, /missing LICENSE in .*x86_64-pc-windows-msvc/);
});

test("the launcher publishes the notices it is given", () => {
  const launcher = JSON.parse(
    readFileSync(join(dirname(SCRIPT), "pixtuoid", "package.json"), "utf8"),
  );
  assert.ok(launcher.files.includes("LICENSE") && launcher.files.includes("licenses/"));
});

test("accepts a prerelease semver", (t) => {
  const { npmDir, artifacts } = scaffold(t);
  assert.equal(run(npmDir, artifacts, "0.6.0-rc.1").status, 0);
});

test("rejects a non-semver version without writing", (t) => {
  const { npmDir, artifacts } = scaffold(t);
  const r = run(npmDir, artifacts, "1.2");
  assert.equal(r.status, 1);
  assert.match(r.stderr, /non-semver/);
  const launcher = JSON.parse(readFileSync(join(npmDir, "pixtuoid", "package.json"), "utf8"));
  assert.equal(launcher.version, "0.0.0");
});

test("throws when a target's prebuilt binary is missing", (t) => {
  const { npmDir, artifacts } = scaffold(t, { skip: "x86_64-unknown-linux-musl" });
  const r = run(npmDir, artifacts, "1.2.3");
  assert.notEqual(r.status, 0);
  assert.match(r.stderr, /missing prebuilt binary for x86_64-unknown-linux-musl/);
});

test("release.yml build matrix targets == npm TARGETS (npm ships every built platform)", () => {
  const releaseYml = readFileSync(
    join(dirname(SCRIPT), "..", ".github", "workflows", "release.yml"),
    "utf8",
  );
  // Isolate the `build:` job — the `deb:` job carries its own smaller matrix.
  const buildStart = releaseYml.indexOf("\n  build:\n");
  assert.notEqual(buildStart, -1, "release.yml has a build: job");
  const afterBuild = releaseYml.slice(buildStart + 1);
  const nextJob = afterBuild.search(/\n  [a-z][\w-]*:\n/);
  const buildBlock = nextJob === -1 ? afterBuild : afterBuild.slice(0, nextJob + 1);
  const matrixTargets = [...buildBlock.matchAll(/^\s+- target:\s*(\S+)/gm)].map((m) => m[1]);
  assert.ok(
    matrixTargets.length >= 4,
    `parsed only ${matrixTargets.length} matrix targets — the build-job slice/regex drifted`,
  );
  assert.deepEqual(
    [...matrixTargets].sort(),
    TARGETS.map((t) => t.rust).sort(),
    "release.yml build matrix targets must match npm TARGETS — a new build target the " +
      "generator doesn't know ships NOWHERE on npm (silent). Update both.",
  );
});
