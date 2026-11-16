import { test } from 'node:test';
import assert from 'node:assert/strict';
import rehypeRepoLinks from './rehype-repo-links.mjs';
import { h } from 'hastscript';
import { DOC_DIR, REPO_BLOB } from './github.mjs';

const DOCS = [
  { file: 'ARCHITECTURE.md', route: 'architecture/' },
  { file: 'HOW-WE-BUILD.md', route: 'how-we-build/' },
];
const rewritten = (href) => {
  const a = h('a', { href });
  rehypeRepoLinks({ docs: DOCS, base: '/' })({ type: 'root', children: [a] });
  return a.properties.href;
};

test('a link to a rendered doc stays on the site, anchor kept', () => {
  assert.equal(rewritten('HOW-WE-BUILD.md'), '/how-we-build/');
  assert.equal(rewritten('ARCHITECTURE.md#data-flow'), '/architecture/#data-flow');
  assert.equal(rewritten('./ARCHITECTURE.md'), '/architecture/');
});

test("any other repo-relative link goes to the file on GitHub, resolved from the doc's directory", () => {
  assert.equal(rewritten('../AGENTS.md#org-rules'), `${REPO_BLOB}/AGENTS.md#org-rules`);
  assert.equal(rewritten('images/demo.gif'), `${REPO_BLOB}/${DOC_DIR}/images/demo.gif`);
  assert.equal(rewritten('../crates/ARCHITECTURE.md'), `${REPO_BLOB}/crates/ARCHITECTURE.md`);
});

test('an in-page anchor and an absolute URL are left alone', () => {
  assert.equal(rewritten('#the-arc-loop'), '#the-arc-loop');
  assert.equal(rewritten('https://pixtuoid.dev/'), 'https://pixtuoid.dev/');
  assert.equal(rewritten('//pixtuoid.dev/x'), '//pixtuoid.dev/x');
});
