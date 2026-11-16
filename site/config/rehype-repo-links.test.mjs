import { test } from 'node:test';
import assert from 'node:assert/strict';
import rehypeRepoLinks from './rehype-repo-links.mjs';
import { REPO_BLOB } from './github.mjs';

const DOCS = [
  { file: 'ARCHITECTURE.md', route: 'architecture/' },
  { file: 'HOW-WE-BUILD.md', route: 'how-we-build/' },
];
const rewritten = (href) => {
  const a = { type: 'element', tagName: 'a', properties: { href }, children: [] };
  rehypeRepoLinks({ docs: DOCS, base: '/' })({ type: 'root', children: [a] });
  return a.properties.href;
};

test('a link to a rendered doc stays on the site, anchor kept', () => {
  assert.equal(rewritten('HOW-WE-BUILD.md'), '/how-we-build/');
  assert.equal(rewritten('ARCHITECTURE.md#data-flow'), '/architecture/#data-flow');
  assert.equal(rewritten('./ARCHITECTURE.md'), '/architecture/');
});

test('any other repo-relative link goes to the file on GitHub, resolved from docs/', () => {
  assert.equal(rewritten('../AGENTS.md#org-rules'), `${REPO_BLOB}/AGENTS.md#org-rules`);
  assert.equal(rewritten('images/demo.gif'), `${REPO_BLOB}/docs/images/demo.gif`);
  assert.equal(rewritten('../crates/ARCHITECTURE.md'), `${REPO_BLOB}/crates/ARCHITECTURE.md`);
});

test('an in-page anchor and an absolute URL are left alone', () => {
  assert.equal(rewritten('#the-arc-loop'), '#the-arc-loop');
  assert.equal(rewritten('https://pixtuoid.dev/'), 'https://pixtuoid.dev/');
});
