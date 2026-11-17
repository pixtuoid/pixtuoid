import assert from 'node:assert/strict';
import test from 'node:test';

import { readFileSync } from 'node:fs';

import { h } from 'hastscript';
import { toText } from 'hast-util-to-text';
import { visit } from 'unist-util-visit';
import rehypeBeautifulMermaid from './rehype-beautiful-mermaid.mjs';
import { mermaidFences } from './assert-docs-rendered.mjs';
import { DOC_DIR } from './docs.mjs';
import docs from '../src/docs.json' with { type: 'json' };

const SOURCE = `flowchart TB
  accTitle: pixtuoid data flow
  accDescr: A hook event flows into the reducer.
  A["hook"] --> B["Reducer::apply"]
`;

const codeBlock = (lang, value) =>
  h('pre', [h('code', { className: [`language-${lang}`] }, value)]);

function run(tree) {
  rehypeBeautifulMermaid()(tree);
  return tree;
}

function all(tree, test) {
  const hits = [];
  visit(tree, test, (node) => {
    hits.push(node);
  });
  return hits;
}
const tagged = (tagName) => ({ type: 'element', tagName });
const withClass = (name) => (n) =>
  n.type === 'element' && (n.properties?.className ?? []).includes(name);
const svgOf = (tree) => all(tree, tagged('svg'))[0];

test('a ```mermaid block becomes one figure holding an inline <svg>, with no client-side script', () => {
  const tree = run({ type: 'root', children: [codeBlock('mermaid', SOURCE)] });
  assert.equal(tree.children.length, 1);
  assert.equal(tree.children[0].tagName, 'figure');
  const svg = svgOf(tree);
  assert.ok(toText(svg).includes('Reducer::apply'), 'node labels render as text');
  assert.deepEqual(all(tree, tagged('script')), []);
});

test('accTitle / accDescr become the <title>/<desc> the SVG is labelled by, not nodes', () => {
  const tree = run({ type: 'root', children: [codeBlock('mermaid', SOURCE)] });
  const svg = svgOf(tree);
  const [title, desc] = svg.children;
  assert.equal(title.tagName, 'title');
  assert.equal(toText(title), 'pixtuoid data flow');
  assert.equal(desc.tagName, 'desc');
  assert.equal(toText(desc), 'A hook event flows into the reducer.');
  assert.equal(svg.properties.role, 'img');
  assert.equal(svg.properties.ariaLabelledBy, `${title.properties.id} ${desc.properties.id}`);
  assert.equal(toText(svg).includes('accTitle'), false, 'the directive is not drawn as a node');
  assert.equal(toText(svg).includes('accDescr'), false);
});

function stylesheetOf(svg) {
  return svg.children
    .filter((c) => c.tagName === 'style')
    .flatMap((s) => s.children.map((c) => c.value))
    .join('\n');
}

test('colors are the page theme: the root declares no --bg/--fg of its own', () => {
  const tree = run({ type: 'root', children: [codeBlock('mermaid', SOURCE)] });
  const svg = svgOf(tree);
  assert.doesNotMatch(String(svg.properties.style ?? ''), /--(bg|fg)\s*:/);
  const sheet = stylesheetOf(svg);
  assert.match(sheet, /var\(--bg\)/);
  assert.match(sheet, /var\(--fg\)/);
  assert.doesNotMatch(sheet, /--(bg|fg)\s*:/, 'no root token moved into the sheet either');
});

test('the stylesheet fetches nothing: no @import, no URL (CSP style-src/font-src self)', () => {
  const tree = run({ type: 'root', children: [codeBlock('mermaid', SOURCE)] });
  const svg = svgOf(tree);
  assert.doesNotMatch(stylesheetOf(svg), /@import|https?:/);
  const tree_ = JSON.stringify({ ...svg, properties: { ...svg.properties, xmlns: undefined } });
  assert.doesNotMatch(tree_, /https?:|url\((?!#)/, 'nothing in the tree points off-page');
});

// The renderer degrades silently on malformed source (a dropped `end`, a
// mistyped arrow) — fewer shapes, no throw — and the doc-render guard only
// counts each page's <svg>. Every committed diagram is the population.
for (const { file } of docs) {
  const doc = readFileSync(new URL(`../../${DOC_DIR}/${file}`, import.meta.url), 'utf8');
  for (const source of mermaidFences(doc)) {
    test(`${DOC_DIR}/${file}'s diagram renders every subgraph, node and edge it declares`, () => {
      const svg = svgOf(run({ type: 'root', children: [codeBlock('mermaid', source)] }));
      assert.equal(
        all(svg, withClass('subgraph')).length,
        (source.match(/^\s*subgraph\s/gm) ?? []).length
      );
      assert.equal(all(svg, withClass('node')).length, new Set(source.match(/\b\w+(?=\[)/g)).size);
      assert.equal(all(svg, tagged('polyline')).length, source.match(/->/g).length);
    });
  }
}

test('the diagram sits in a figure: a bar, then a tab-stop body holding it at its drawn size', () => {
  const tree = run({ type: 'root', children: [codeBlock('mermaid', SOURCE)] });
  const [figure] = tree.children;
  assert.deepEqual(figure.properties.className, ['fig--diagram', 'fig--window']);
  const [bar, body] = figure.children;
  assert.deepEqual(bar.properties.className, ['fig__bar']);
  assert.deepEqual(body.properties.className, ['fig__body']);
  assert.equal(body.properties.tabIndex, 0);
  const svg = svgOf(body);
  assert.ok(Number(svg.properties.width) > 0);
  assert.match(svg.properties.style, new RegExp(`min-width:${svg.properties.width}px$`));
});

test('the nodes the path starts and ends at are marked', () => {
  const src = `flowchart LR\n  accTitle: t\n  accDescr: d\n  A --> B --> C\n`;
  const svg = svgOf(run({ type: 'root', children: [codeBlock('mermaid', src)] }));
  const ends = svg.children.filter(withClass('fig__end')).map((n) => n.properties.dataId);
  assert.deepEqual(ends, ['A', 'C']);
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
