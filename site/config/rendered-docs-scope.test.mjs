// A rendered doc is a site build input, yet whether a PR runs that build is
// decided by site.yml's `scope` path list, which no build reads.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { BLOG_DIR, postIds } from './blog.mjs';
import { DOC_DIR } from './docs.mjs';
import docs from '../src/docs.json' with { type: 'json' };

// What a post shows from the repo's docs directory: a path that climbs out of site/ to it.
const shown = (id) => {
  const post = readFileSync(new URL(`../${BLOG_DIR}/${id}.md`, import.meta.url), 'utf8');
  const climb = '../'.repeat(BLOG_DIR.split('/').length + 1);
  return [...post.matchAll(/\]\(([^)\s]+)\)/g)]
    .map(([, target]) => target)
    .filter((target) => target.startsWith(`${climb}${DOC_DIR}/`))
    .map((target) => target.slice(climb.length));
};

test("site.yml's scope paths name exactly the docs the site renders and what its posts show", () => {
  const scope = /id: scope\n\s+with:(?:\n\s+#.*)*\n\s+paths: (.+)/.exec(
    readFileSync(new URL('../../.github/workflows/site.yml', import.meta.url), 'utf8')
  );
  assert.ok(scope, "site.yml's `id: scope` step has no single-line `paths:` under `with:`");
  const listed = scope[1].split(' ').filter((p) => p.startsWith(`${DOC_DIR}/`));
  // Equality, not a subset: path-changed fails every later PR on a listed path
  // that no longer exists, so a renamed doc's old path must not outlive its roster entry.
  assert.deepEqual(
    listed.sort(),
    [...docs.map((d) => `${DOC_DIR}/${d.file}`), ...postIds().flatMap(shown)].sort()
  );
});
