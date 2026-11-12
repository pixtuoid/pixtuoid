import { test } from 'node:test';
import assert from 'node:assert/strict';
import process from 'node:process';
import { readFileSync } from 'node:fs';
import { E2E_STAR_COUNT, REPO, REPO_SLUG, fetchStarCount } from './github.mjs';
import { workspacePackageField } from './workspace-package.mjs';

const ok = (count) =>
  /** @type {any} */ ({ ok: true, json: async () => ({ stargazers_count: count }) });

test('a reachable API yields the raw count as a string', async () => {
  assert.equal(await fetchStarCount(async () => ok(342), undefined), '342');
});

test('a non-ok response yields null (offline build must not fail)', async () => {
  const notFound = /** @type {any} */ ({ ok: false, json: async () => ({}) });
  assert.equal(await fetchStarCount(async () => notFound, undefined), null);
});

test('a thrown fetch (offline / timeout) yields null', async () => {
  assert.equal(
    await fetchStarCount(async () => {
      throw new Error('offline');
    }, undefined),
    null
  );
});

test('a malformed body yields null, not "undefined"', async () => {
  const weird = /** @type {any} */ ({ ok: true, json: async () => ({}) });
  assert.equal(await fetchStarCount(async () => weird, undefined), null);
});

test('a provided token rides the authorization header', async () => {
  let seen;
  await fetchStarCount(async (_url, init) => {
    seen = init.headers.authorization;
    return ok(1);
  }, 'tok');
  assert.equal(seen, 'Bearer tok');
});

test('the fetch is bounded by an AbortSignal (the offline-build timeout)', async () => {
  let seenSignal;
  await fetchStarCount(async (_url, init) => {
    seenSignal = init.signal;
    return ok(1);
  }, undefined);
  assert.ok(seenSignal instanceof AbortSignal);
});

test('GH_STARS_E2E short-circuits the fetch with E2E_STAR_COUNT', async () => {
  const prev = process.env.GH_STARS_E2E;
  process.env.GH_STARS_E2E = '1';
  try {
    let called = false;
    const count = await fetchStarCount(async () => {
      called = true;
      return ok(1);
    }, undefined);
    assert.equal(count, E2E_STAR_COUNT);
    assert.equal(called, false);
  } finally {
    if (prev === undefined) delete process.env.GH_STARS_E2E;
    else process.env.GH_STARS_E2E = prev;
  }
});

test('a set-but-empty GH_STARS_E2E behaves as unset (degenerate-env rule)', async () => {
  const prev = process.env.GH_STARS_E2E;
  process.env.GH_STARS_E2E = '';
  try {
    assert.equal(await fetchStarCount(async () => ok(7), undefined), '7');
  } finally {
    if (prev === undefined) delete process.env.GH_STARS_E2E;
    else process.env.GH_STARS_E2E = prev;
  }
});

test("install.json's GitHub links name REPO_SLUG, the copy JSON can't import", () => {
  const json = readFileSync(new URL('../src/install.json', import.meta.url), 'utf8');
  const slugs = [...json.matchAll(/github\.com\/([\w.-]+\/[\w.-]+)/g)].map((m) => m[1]);
  assert.ok(slugs.length > 0, 'install.json names no GitHub repository');
  for (const slug of slugs) assert.equal(slug, REPO_SLUG);
});

test("REPO is Cargo.toml's `repository`, the URL the Rust crates link", () => {
  const cargo = readFileSync(new URL('../../Cargo.toml', import.meta.url), 'utf8');
  assert.equal(REPO, workspacePackageField(cargo, 'repository'));
});
