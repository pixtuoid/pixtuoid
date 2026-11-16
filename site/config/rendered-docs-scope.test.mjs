// A rendered doc is a site build input, yet whether a PR runs that build is
// decided by site.yml's `scope` path list, which no build reads.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import docs from '../src/docs.json' with { type: 'json' };

test("site.yml's scope paths name exactly the docs the site renders", () => {
  const scope = /id: scope\n\s+with:(?:\n\s+#.*)*\n\s+paths: (.+)/.exec(
    readFileSync(new URL('../../.github/workflows/site.yml', import.meta.url), 'utf8')
  );
  assert.ok(scope, "site.yml's `id: scope` step has no single-line `paths:` under `with:`");
  const listed = scope[1].split(' ').filter((p) => p.startsWith('docs/'));
  // Equality, not a subset: path-changed fails every later PR on a listed path
  // that no longer exists, so a renamed doc's old path must not outlive its roster entry.
  assert.deepEqual(listed.sort(), docs.map((d) => `docs/${d.file}`).sort());
});
