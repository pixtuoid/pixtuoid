// A rendered doc is a site build input, yet whether a PR runs that build is
// decided by site.yml's `scope` path list, which no build reads.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fromMarkdown } from 'mdast-util-from-markdown';
import { visit } from 'unist-util-visit';
import { BLOG_DIR, postFile, postIds } from './blog.mjs';
import { DOC_DIR } from './docs.mjs';
import docs from '../src/docs.json' with { type: 'json' };

// What a post shows from the repo's docs directory: an image whose path climbs out of site/ to it.
const shown = (id) => {
  const climb = '../'.repeat(BLOG_DIR.split('/').length + 1);
  const images = [];
  visit(
    fromMarkdown(readFileSync(new URL(`../${postFile(id)}`, import.meta.url), 'utf8')),
    ['image', 'definition'],
    (node) => images.push(node.url)
  );
  return images
    .filter((url) => url.startsWith(`${climb}${DOC_DIR}/`))
    .map((url) => url.slice(climb.length));
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
    [...new Set([...docs.map((d) => `${DOC_DIR}/${d.file}`), ...postIds().flatMap(shown)])].sort()
  );
});
