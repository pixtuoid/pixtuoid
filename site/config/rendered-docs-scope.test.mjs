// A rendered doc is a site build input, yet whether a PR runs that build is
// decided by site.yml's `scope` path list, which no build reads.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const read = (rel) => readFileSync(fileURLToPath(new URL(rel, import.meta.url)), 'utf8');

test("site.yml's scope paths name exactly the docs content.config.ts renders", () => {
  const config = read('../src/content.config.ts');
  const docs = [...config.matchAll(/pattern: '([^']+)', base: '\.\.\/docs'/g)].map(
    (m) => `docs/${m[1]}`
  );
  // A loader the regex cannot read would drop out of both sides of the comparison below.
  assert.equal(
    docs.length,
    config.match(/['"]\.\.\/docs\/?['"]/g)?.length,
    "every loader naming '../docs' in content.config.ts must be a one-line `pattern: '…', base: '../docs'`"
  );
  const scope = /id: scope\n\s+with:(?:\n\s+#.*)*\n\s+paths: (.+)/.exec(
    read('../../.github/workflows/site.yml')
  );
  assert.ok(scope, "site.yml's `id: scope` step has no single-line `paths:` under `with:`");
  const listed = scope[1].split(' ').filter((p) => p.startsWith('docs/'));
  // Equality, not a subset: path-changed fails every later PR on a listed path
  // that no longer exists, so a renamed doc's old path must not outlive its loader.
  assert.deepEqual(listed.sort(), docs.sort());
});
