import { test } from 'node:test';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import assert from 'node:assert/strict';
import rehypeRepoLinks from './rehype-repo-links.mjs';
import { h } from 'hastscript';
import { BLOG_DIR, SITE_DIR } from './blog.mjs';
import { DOC_DIR } from './docs.mjs';
import { REPO_BLOB } from './github.mjs';

const BLOG_DIR_FROM_ROOT = `${SITE_DIR}/${BLOG_DIR}`;
const PAGES = [
  { file: `${DOC_DIR}/ARCHITECTURE.md`, route: 'architecture/' },
  { file: `${DOC_DIR}/HOW-WE-BUILD.md`, route: 'how-we-build/' },
  { file: `${BLOG_DIR_FROM_ROOT}/other-post.md`, route: 'blog/other-post/' },
];
const REPO_ROOT = fileURLToPath(new URL('../../', import.meta.url));
const rewritten = (href, base = '/', from = `${DOC_DIR}/CONFIGURATION.md`) => {
  const a = h('a', { href });
  rehypeRepoLinks({ pages: PAGES, base })(
    { type: 'root', children: [a] },
    { path: join(REPO_ROOT, from) }
  );
  return a.properties.href;
};

test('a link to a rendered doc stays on the site, anchor kept', () => {
  assert.equal(rewritten('HOW-WE-BUILD.md'), '/how-we-build/');
  assert.equal(rewritten('ARCHITECTURE.md#data-flow'), '/architecture/#data-flow');
  assert.equal(rewritten('./ARCHITECTURE.md'), '/architecture/');
});

test("a link to a rendered doc carries the site's base path, slashed or not", () => {
  assert.equal(rewritten('HOW-WE-BUILD.md', '/pixtuoid/'), '/pixtuoid/how-we-build/');
  assert.equal(rewritten('HOW-WE-BUILD.md', '/pixtuoid'), '/pixtuoid/how-we-build/');
});

test("any other repo-relative link goes to the file on GitHub, resolved from the doc's directory", () => {
  assert.equal(rewritten('../AGENTS.md#org-rules'), `${REPO_BLOB}/AGENTS.md#org-rules`);
  assert.equal(rewritten('images/demo.gif'), `${REPO_BLOB}/${DOC_DIR}/images/demo.gif`);
  assert.equal(rewritten('../crates/ARCHITECTURE.md'), `${REPO_BLOB}/crates/ARCHITECTURE.md`);
});

test('a root-relative link resolves from the repo root, and a climb above it is clamped', () => {
  assert.equal(rewritten('/AGENTS.md'), `${REPO_BLOB}/AGENTS.md`);
  assert.equal(rewritten('../../x.md'), `${REPO_BLOB}/x.md`);
});

test('an in-page anchor and an absolute URL are left alone', () => {
  assert.equal(rewritten('#the-arc-loop'), '#the-arc-loop');
  assert.equal(rewritten('https://pixtuoid.dev/'), 'https://pixtuoid.dev/');
  assert.equal(rewritten('//pixtuoid.dev/x'), '//pixtuoid.dev/x');
});

test("a post's links resolve from the post's own directory, and one to a sibling stays on the site", () => {
  const post = `${BLOG_DIR_FROM_ROOT}/a-post.md`;
  assert.equal(rewritten('./other-post.md', '/', post), '/blog/other-post/');
  assert.equal(rewritten('./notes.txt', '/', post), `${REPO_BLOB}/${BLOG_DIR_FROM_ROOT}/notes.txt`);
  assert.equal(rewritten('../../../../docs/ARCHITECTURE.md', '/', post), '/architecture/');
});
