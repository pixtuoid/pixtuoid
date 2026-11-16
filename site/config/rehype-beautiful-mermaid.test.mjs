import assert from 'node:assert/strict';
import test from 'node:test';

import { readFileSync } from 'node:fs';

import rehypeBeautifulMermaid from './rehype-beautiful-mermaid.mjs';

const SOURCE = `flowchart TB
  accTitle: pixtuoid data flow
  accDescr: A hook event flows into the reducer.
  A["hook"] --> B["Reducer::apply"]
`;

function codeBlock(lang, value) {
  return {
    type: 'element',
    tagName: 'pre',
    properties: {},
    children: [
      {
        type: 'element',
        tagName: 'code',
        properties: { className: [`language-${lang}`] },
        children: [{ type: 'text', value }],
      },
    ],
  };
}

function hasElement(node, tagName) {
  if (node.type === 'element' && node.tagName === tagName) return true;
  return (node.children ?? []).some((c) => hasElement(c, tagName));
}

function textOf(node) {
  if (node.type === 'text') return node.value;
  return (node.children ?? []).map(textOf).join('');
}

function run(tree) {
  rehypeBeautifulMermaid()(tree);
  return tree;
}

test('a ```mermaid block becomes one inline <svg> with no client-side script', () => {
  const tree = run({ type: 'root', children: [codeBlock('mermaid', SOURCE)] });
  assert.equal(tree.children.length, 1);
  const svg = tree.children[0];
  assert.equal(svg.tagName, 'svg');
  assert.ok(textOf(svg).includes('Reducer::apply'), 'node labels render as text');
  assert.equal(hasElement(svg, 'script'), false);
});

test('accTitle / accDescr become the <title>/<desc> the SVG is labelled by, not nodes', () => {
  const tree = run({ type: 'root', children: [codeBlock('mermaid', SOURCE)] });
  const svg = tree.children[0];
  const [title, desc] = svg.children;
  assert.equal(title.tagName, 'title');
  assert.equal(textOf(title), 'pixtuoid data flow');
  assert.equal(desc.tagName, 'desc');
  assert.equal(textOf(desc), 'A hook event flows into the reducer.');
  assert.equal(svg.properties.role, 'img');
  assert.equal(svg.properties.ariaLabelledBy, `${title.properties.id} ${desc.properties.id}`);
  assert.equal(textOf(svg).includes('accTitle'), false, 'the directive is not drawn as a node');
  assert.equal(textOf(svg).includes('accDescr'), false);
});

function stylesheetOf(svg) {
  return svg.children
    .filter((c) => c.tagName === 'style')
    .flatMap((s) => s.children.map((c) => c.value))
    .join('\n');
}

test('colors are the page theme: the root declares no --bg/--fg of its own', () => {
  const tree = run({ type: 'root', children: [codeBlock('mermaid', SOURCE)] });
  const svg = tree.children[0];
  assert.doesNotMatch(String(svg.properties.style ?? ''), /--(bg|fg)\s*:/);
  const sheet = stylesheetOf(svg);
  assert.match(sheet, /var\(--bg\)/);
  assert.match(sheet, /var\(--fg\)/);
  assert.doesNotMatch(sheet, /--(bg|fg)\s*:/, 'no root token moved into the sheet either');
});

test('the stylesheet fetches nothing: no @import, no URL (CSP style-src/font-src self)', () => {
  const tree = run({ type: 'root', children: [codeBlock('mermaid', SOURCE)] });
  const svg = tree.children[0];
  assert.doesNotMatch(stylesheetOf(svg), /@import|https?:/);
  const tree_ = JSON.stringify({ ...svg, properties: { ...svg.properties, xmlns: undefined } });
  assert.doesNotMatch(tree_, /https?:|url\((?!#)/, 'nothing in the tree points off-page');
});

function count(node, pred) {
  return (pred(node) ? 1 : 0) + (node.children ?? []).reduce((n, c) => n + count(c, pred), 0);
}

// The renderer degrades silently on malformed source (a dropped `end`, a
// mistyped arrow) — fewer shapes, no throw — and the doc-render guard only
// checks that an <svg> exists. The committed diagram is the population.
test('the committed architecture diagram renders every subgraph, node and edge it declares', () => {
  const doc = readFileSync(new URL('../../docs/ARCHITECTURE.md', import.meta.url), 'utf8');
  const source = /```mermaid\n([\s\S]*?)```/.exec(doc)[1];
  const tree = run({ type: 'root', children: [codeBlock('mermaid', source)] });
  const svg = tree.children[0];
  const withClass = (name) => (n) =>
    n.type === 'element' && (n.properties?.className ?? []).includes(name);
  assert.equal(count(svg, withClass('subgraph')), (source.match(/^\s*subgraph\s/gm) ?? []).length);
  assert.equal(count(svg, withClass('node')), new Set(source.match(/\b\w+(?=\[)/g)).size);
  assert.equal(
    count(svg, (n) => n.type === 'element' && n.tagName === 'polyline'),
    source.match(/->/g).length
  );
});

test('a non-mermaid code block is left alone', () => {
  const block = codeBlock('rust', 'fn main() {}');
  const tree = run({ type: 'root', children: [block] });
  assert.equal(tree.children[0], block);
});

test('a diagram without its accTitle and accDescr fails the build (WCAG 1.1.1)', () => {
  for (const src of [
    'flowchart LR\n  A --> B',
    'flowchart LR\n  accTitle: Only a title\n  A --> B',
    'flowchart LR\n  accDescr: Only a description\n  A --> B',
  ]) {
    assert.throws(
      () => run({ type: 'root', children: [codeBlock('mermaid', src)] }),
      /accTitle and accDescr/
    );
  }
});
