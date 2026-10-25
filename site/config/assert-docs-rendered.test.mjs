import { test } from 'node:test';
import assert from 'node:assert/strict';
import { checkDocPages, MIN_BODY_CHARS } from './assert-docs-rendered.mjs';

const body = (n) => 'x'.repeat(n);
const doc = (inner) => `<article class="prose" data-astro-cid-x>${inner}</article>`;
const arch = (inner) => doc(`<svg id="mermaid-0" aria-roledescription="flowchart"></svg>${inner}`);

test('all good: every body present, /architecture has its <svg>', () => {
  const { failures, docPages } = checkDocPages([
    { name: 'config', html: doc(body(MIN_BODY_CHARS)) },
    { name: 'architecture', html: arch(body(MIN_BODY_CHARS)) },
  ]);
  assert.equal(docPages, 2);
  assert.deepEqual(failures, []);
});

test('a collapsed (empty) doc <article> body fails — the exact bug', () => {
  const { failures, docPages } = checkDocPages([{ name: 'config', html: doc('') }]);
  assert.equal(docPages, 1);
  assert.ok(failures.some((f) => f.includes('/config') && f.includes('render collapsed')));
});

test('/architecture without an inline <svg> fails (mermaid did not render)', () => {
  const { failures } = checkDocPages([{ name: 'architecture', html: doc(body(MIN_BODY_CHARS)) }]);
  assert.ok(failures.some((f) => f.includes('/architecture') && f.includes('no inline <svg>')));
});

test('no doc pages at all (selector drift) fails loudly, never vacuously passes', () => {
  const { failures, docPages } = checkDocPages([
    { name: 'x', html: '<article class="other">hi</article>' },
  ]);
  assert.equal(docPages, 0);
  assert.ok(failures.some((f) => f.includes('no doc pages found')));
});

test('the article selector tolerates EXTRA classes (\\bprose\\b, not sole-class)', () => {
  const { failures, docPages } = checkDocPages([
    {
      name: 'config',
      html: `<article class="prose max-w-none astro-abc">${body(MIN_BODY_CHARS)}</article>`,
    },
  ]);
  assert.equal(docPages, 1);
  assert.deepEqual(failures, []);
});

test('word boundary: a mere substring like "prosetype" is NOT treated as a doc page', () => {
  const { docPages } = checkDocPages([
    { name: 'x', html: `<article class="prosetype">${body(600)}</article>` },
  ]);
  assert.equal(docPages, 0);
});

test('a body one char under the floor fails', () => {
  const under = checkDocPages([{ name: 'config', html: doc(body(MIN_BODY_CHARS - 1)) }]);
  assert.ok(under.failures.some((f) => f.includes('render collapsed')));
});
