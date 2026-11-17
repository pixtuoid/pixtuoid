import { test } from 'node:test';
import assert from 'node:assert/strict';
import { checkPages, mermaidFences, MIN_BODY_CHARS } from './assert-docs-rendered.mjs';

const body = (n) => 'x'.repeat(n);
const doc = (inner) => `<article class="prose" data-astro-cid-x>${inner}</article>`;
const SVG = '<svg role="img"></svg>';

test('all good: every body present, every declared diagram drawn', () => {
  const { failures, pages } = checkPages(
    [
      { name: 'config', html: doc(body(MIN_BODY_CHARS)) },
      { name: 'architecture', html: doc(`${SVG}${body(MIN_BODY_CHARS)}`) },
    ],
    { config: 0, architecture: 1 }
  );
  assert.equal(pages, 2);
  assert.deepEqual(failures, []);
});

test('a collapsed (empty) doc <article> body fails — the exact bug', () => {
  const { failures, pages } = checkPages([{ name: 'config', html: doc('') }], { config: 0 });
  assert.equal(pages, 1);
  assert.ok(failures.some((f) => f.includes('/config') && f.includes('render collapsed')));
});

test('a page with fewer <svg role="img"> than its source declares mermaid blocks fails', () => {
  const { failures } = checkPages(
    [{ name: 'how-we-build', html: doc(`${SVG}${body(MIN_BODY_CHARS)}`) }],
    { 'how-we-build': 2 }
  );
  assert.ok(failures.some((f) => f.includes('/how-we-build: 1 <svg role="img"> for the 2')));
});

test('a decorative svg is no diagram', () => {
  const drawing = '<svg class="fig-budget__line" aria-hidden="true"></svg>';
  const { failures } = checkPages(
    [{ name: 'post', html: doc(`${drawing}${body(MIN_BODY_CHARS)}`) }],
    {
      post: 0,
    }
  );
  assert.deepEqual(failures, []);
});

test('a page with more <svg role="img"> than its source declares mermaid blocks fails', () => {
  const { failures } = checkPages(
    [{ name: 'config', html: doc(`${SVG}${SVG}${body(MIN_BODY_CHARS)}`) }],
    { config: 1 }
  );
  assert.ok(failures.some((f) => f.includes('/config: 2 <svg role="img"> for the 1')));
});

test('a rendered article no roster lists fails', () => {
  const { failures } = checkPages([{ name: 'stray', html: doc(body(MIN_BODY_CHARS)) }], {});
  assert.ok(failures.some((f) => f.includes('/stray') && f.includes('do not list')));
});

test('mermaidFences returns the source of every mermaid block, whatever its fence', () => {
  const md = [
    '```mermaid\nA --> B\n```',
    '```js\nx\n```',
    '~~~mermaid\nC\n~~~',
    '   ```mermaid\nD\n   ```',
  ].join('\n\n');
  assert.deepEqual(mermaidFences(md), ['A --> B', 'C', 'D']);
});

test('a mermaid fence quoted inside another code block is no diagram', () => {
  assert.deepEqual(mermaidFences('````text\n```mermaid\nA --> B\n```\n````\n'), []);
});

test('no pages at all (selector drift) fails loudly, never vacuously passes', () => {
  const { failures, pages } = checkPages(
    [{ name: 'x', html: '<article class="other">hi</article>' }],
    {}
  );
  assert.equal(pages, 0);
  assert.ok(failures.some((f) => f.includes('no pages found')));
});

test('the article selector tolerates EXTRA classes (\\bprose\\b, not sole-class)', () => {
  const { failures, pages } = checkPages(
    [
      {
        name: 'config',
        html: `<article class="prose max-w-none astro-abc">${body(MIN_BODY_CHARS)}</article>`,
      },
    ],
    { config: 0 }
  );
  assert.equal(pages, 1);
  assert.deepEqual(failures, []);
});

test('word boundary: a mere substring like "prosetype" is NOT treated as a doc page', () => {
  const { pages } = checkPages(
    [{ name: 'x', html: `<article class="prosetype">${body(600)}</article>` }],
    {}
  );
  assert.equal(pages, 0);
});

test('a body one char under the floor fails', () => {
  const under = checkPages([{ name: 'config', html: doc(body(MIN_BODY_CHARS - 1)) }], {
    config: 0,
  });
  assert.ok(under.failures.some((f) => f.includes('render collapsed')));
});

test('a page the rosters list that rendered no article fails', () => {
  const { failures } = checkPages([{ name: 'config', html: doc(body(MIN_BODY_CHARS)) }], {
    config: 0,
    'blog/a-post': 0,
  });
  assert.deepEqual(failures, ['/blog/a-post: in the rosters, but no rendered article']);
});

test('a page named after a key every object inherits is listed only if a roster lists it', () => {
  const { failures } = checkPages([{ name: 'constructor', html: doc(body(MIN_BODY_CHARS)) }], {});
  assert.ok(failures.some((f) => f.includes('/constructor') && f.includes('do not list')));
});
