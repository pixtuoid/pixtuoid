#!/usr/bin/env node
// The launcher shims' own failure paths, where no native binary runs; the path
// that runs one is generate.test.mjs's, over the packages it generates.

import { test } from "node:test";
import assert from "node:assert/strict";
import { chmodSync, cpSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const BIN = join(dirname(fileURLToPath(import.meta.url)), "pixtuoid", "bin");

// The package npm installs on this host, named apart from resolve.js.
const HOST_PACKAGE = `@pixtuoid/cli-${process.platform}-${process.arch}`;

/**
 * The launcher alone in a scratch dir, its manifest stamped as generate.mjs
 * stamps it, with no platform package for `require.resolve` to find.
 * @param {import("node:test").TestContext} t
 * @param {Record<string, string>} [optionalDependencies]
 */
function launcher(t, optionalDependencies = {}) {
  const dir = mkdtempSync(join(tmpdir(), "pixtuoid-launcher-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  cpSync(BIN, join(dir, "bin"), { recursive: true });
  writeFileSync(join(dir, "package.json"), JSON.stringify({ name: "pixtuoid", optionalDependencies }));
  return dir;
}

/**
 * @param {string} dir
 * @param {string} shim
 * @param {Record<string, string>} [env]
 */
function run(dir, shim, env = {}) {
  return spawnSync(process.execPath, [join(dir, "bin", `${shim}.js`)], {
    input: "{}",
    env: { PATH: process.env.PATH, HOME: dir, ...env },
    encoding: "utf8",
  });
}

test("pixtuoid-hook exits 0 and prints nothing when no binary resolves", (t) => {
  for (const shipped of [{}, { [HOST_PACKAGE]: "1.2.3" }]) {
    const dir = launcher(t, shipped);
    for (const env of /** @type {Record<string, string>[]} */ ([{}, { PIXTUOID_BINARY: join(dir, "missing") }])) {
      const r = run(dir, "pixtuoid-hook", env);
      assert.equal(r.status, 0, JSON.stringify({ shipped, env }));
      assert.equal(r.stdout + r.stderr, "", JSON.stringify({ shipped, env }));
    }
  }
});

test(
  "pixtuoid-hook exits 0 and prints nothing when the native shim cannot start",
  { skip: process.platform === "win32" && "an exec bit is a unix permission" },
  (t) => {
    const dir = launcher(t);
    // Found by the override but not executable, so spawning it fails.
    writeFileSync(join(dir, "pixtuoid-hook"), "#!/bin/sh\n");
    chmodSync(join(dir, "pixtuoid-hook"), 0o644);
    const r = run(dir, "pixtuoid-hook", { PIXTUOID_BINARY: dir });
    assert.equal(r.status, 0);
    assert.equal(r.stdout + r.stderr, "");
  },
);

test("pixtuoid sends a platform no package ships for to a source install", (t) => {
  const r = run(launcher(t), "pixtuoid");
  assert.equal(r.status, 1);
  assert.match(r.stderr, new RegExp(`no prebuilt binary for ${process.platform}-${process.arch}`));
  assert.match(r.stderr, /brew install pixtuoid/);
  assert.match(r.stderr, /cargo install pixtuoid pixtuoid-hook/);
  assert.doesNotMatch(r.stderr, /releases/, "no release carries a build for it");
});

test("pixtuoid tells an install that skipped its platform's package to keep optional dependencies", (t) => {
  const r = run(launcher(t, { [HOST_PACKAGE]: "1.2.3" }), "pixtuoid");
  assert.equal(r.status, 1);
  assert.ok(r.stderr.includes(HOST_PACKAGE), r.stderr);
  assert.match(r.stderr, /--include=optional/, "an `omit=optional` in npmrc would omit it again");
  assert.doesNotMatch(r.stderr, /no prebuilt binary/);
});
