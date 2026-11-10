#!/usr/bin/env node
// The `pixtuoid` launcher's answer when no native binary resolves, on a host
// release.yml builds for and on one it doesn't.

import { test } from "node:test";
import assert from "node:assert/strict";
import { cpSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const BIN = join(dirname(fileURLToPath(import.meta.url)), "pixtuoid", "bin");

/**
 * Runs the launcher alone, with no platform package installed, as if on
 * `platform`/`arch`: a `--require` preload sets what resolve.js reads
 * (https://nodejs.org/api/cli.html#-r---require-module).
 * @param {import("node:test").TestContext} t
 * @param {string} platform
 * @param {string} arch
 */
function launch(t, platform, arch) {
  const dir = mkdtempSync(join(tmpdir(), "pixtuoid-launcher-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  cpSync(BIN, join(dir, "bin"), { recursive: true });
  const host = join(dir, "host.cjs");
  writeFileSync(
    host,
    `Object.defineProperty(process, "platform", { value: ${JSON.stringify(platform)} });\n` +
      `Object.defineProperty(process, "arch", { value: ${JSON.stringify(arch)} });\n`,
  );
  return spawnSync(process.execPath, ["--require", host, join(dir, "bin", "pixtuoid.js")], {
    env: { PATH: process.env.PATH, HOME: dir },
    encoding: "utf8",
  });
}

test("on a host no package ships for, pixtuoid names the installs that build one", (t) => {
  const r = launch(t, "linux", "x64");
  assert.match(r.stderr, /brew install pixtuoid/);
  assert.match(r.stderr, /cargo install pixtuoid pixtuoid-hook/);
  assert.doesNotMatch(r.stderr, /releases/, "release.yml builds nothing for this host");
});

test("on a shipped host whose package is missing, pixtuoid still offers the release build", (t) => {
  assert.match(launch(t, "darwin", "arm64").stderr, /releases\/latest/);
});
