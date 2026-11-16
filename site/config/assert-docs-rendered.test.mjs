import { test } from 'node:test';
import assert from 'node:assert/strict';
import { checkDocPages, mermaidFences, MIN_BODY_CHARS } from './assert-docs-rendered.mjs';

const body = (n) => 'x'.repeat(n);
const doc = (inner) => `<article class="prose" data-astro-cid-x>${inner}</article>`;
const SVG = '<svg role="img"></svg>';

test('all good: every body present, every declared diagram drawn', () => {
  const { failures, docPages } = checkDocPages(
    [
      { name: 'config', html: doc(body(MIN_BODY_CHARS)) },
      { name: 'architecture', html: doc(`${SVG}${body(MIN_BODY_CHARS)}`) },
    ],
    { config: 0, architecture: 1 }
  );
  assert.equal(docPages, 2);
  assert.deepEqual(failures, []);
});

test('a collapsed (empty) doc <article> body fails — the exact bug', () => {
  const { failures, docPages } = checkDocPages([{ name: 'config', html: doc('') }], { config: 0 });
  assert.equal(docPages, 1);
  assert.ok(failures.some((f) => f.includes('/config') && f.includes('render collapsed')));
});

test('a page with fewer <svg> than its doc declares mermaid blocks fails', () => {
  const { failures } = checkDocPages(
    [{ name: 'how-we-build', html: doc(`${SVG}${body(MIN_BODY_CHARS)}`) }],
    { 'how-we-build': 2 }
  );
  assert.ok(failures.some((f) => f.includes('/how-we-build: 1 inline <svg> for the 2')));
});

test('a page with more <svg> than its doc declares mermaid blocks fails', () => {
  const { failures } = checkDocPages(
    [{ name: 'config', html: doc(`${SVG}${SVG}${body(MIN_BODY_CHARS)}`) }],
    { config: 1 }
  );
  assert.ok(failures.some((f) => f.includes('/config: 2 inline <svg> for the 1')));
});

test('a Docs-layout page the roster does not list fails', () => {
  const { failures } = checkDocPages([{ name: 'stray', html: doc(body(MIN_BODY_CHARS)) }], {});
  assert.ok(failures.some((f) => f.includes('/stray') && f.includes('does not list')));
});

test('mermaidFences reads a tilde fence and an indented one', () => {
  assert.deepEqual(mermaidFences('~~~mermaid\nA\n~~~\n   ```mermaid\nB\n   ```\n'), ['A\n', 'B\n']);
});

test('mermaidFences returns the source of every mermaid block', () => {
  assert.deepEqual(
    mermaidFences('a\n```mermaid\nA --> B\n```\n```js\nx\n```\n```mermaid\nC\n```\n'),
    ['A --> B\n', 'C\n']
  );
});

test('no doc pages at all (selector drift) fails loudly, never vacuously passes', () => {
  const { failures, docPages } = checkDocPages(
    [{ name: 'x', html: '<article class="other">hi</article>' }],
    {}
  );
  assert.equal(docPages, 0);
  assert.ok(failures.some((f) => f.includes('no doc pages found')));
});

test('the article selector tolerates EXTRA classes (\\bprose\\b, not sole-class)', () => {
  const { failures, docPages } = checkDocPages(
    [
      {
        name: 'config',
        html: `<article class="prose max-w-none astro-abc">${body(MIN_BODY_CHARS)}</article>`,
      },
    ],
    { config: 0 }
  );
  assert.equal(docPages, 1);
  assert.deepEqual(failures, []);
});

test('word boundary: a mere substring like "prosetype" is NOT treated as a doc page', () => {
  const { docPages } = checkDocPages(
    [{ name: 'x', html: `<article class="prosetype">${body(600)}</article>` }],
    {}
  );
  assert.equal(docPages, 0);
});

test('a body one char under the floor fails', () => {
  const under = checkDocPages([{ name: 'config', html: doc(body(MIN_BODY_CHARS - 1)) }], {
    config: 0,
  });
  assert.ok(under.failures.some((f) => f.includes('render collapsed')));
});
