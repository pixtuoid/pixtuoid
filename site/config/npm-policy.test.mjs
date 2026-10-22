import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const siteRoot = new URL('../', import.meta.url);
const manifest = JSON.parse(readFileSync(new URL('package.json', siteRoot), 'utf8'));
const npmrc = new Set(
  readFileSync(new URL('.npmrc', siteRoot), 'utf8').split(/\r?\n/).filter(Boolean)
);

test('the install-script policy runs on the pinned npm generation', () => {
  assert.match(manifest.packageManager, /^npm@12\.\d+\.\d+$/);
  assert.equal(manifest.engines.npm, '>=12.0.0 <13');
  assert.ok(npmrc.has('engine-strict=true'));
  assert.ok(npmrc.has('strict-allow-scripts=true'));
});

test('install-script permissions stay at least privilege', () => {
  assert.deepEqual(manifest.allowScripts, {
    'esbuild@0.28.1': true,
    fsevents: false,
  });
});
