import { test } from 'node:test';
import assert from 'node:assert/strict';
import { fromHtml } from 'hast-util-from-html';
import { toText } from 'hast-util-to-text';
import { createMarkdownProcessor } from '@astrojs/markdown-remark';
import rehypeFigures from './rehype-figures.mjs';

const marker = (kind) => ({ type: 'raw', value: `<!-- figure: ${kind} -->` });
const nl = { type: 'text', value: '\n' };
const html = (s) => fromHtml(s, { fragment: true }).children[0];
const table = (head, ...rows) =>
  html(
    `<table><thead><tr>${head.map((h) => `<th>${h}</th>`).join('')}</tr></thead><tbody>${rows
      .map((r) => `<tr>${r.map((c) => `<td>${c}</td>`).join('')}</tr>`)
      .join('')}</tbody></table>`
  );
const CARDS = ['Role', 'Acts by'];
const run = (children) => {
  const tree = { type: 'root', children };
  rehypeFigures()(tree);
  return tree;
};
const byClass = (node, name) => {
  for (const c of node.children ?? []) {
    if (c.properties?.className?.includes(name)) return c;
    const deep = byClass(c, name);
    if (deep) return deep;
  }
};
const figure = (tree) => tree.children.find((c) => c.tagName === 'figure');

test('cards turn each row into a role card that keeps its links, and the marker leaves', () => {
  const tree = run([
    marker('cards'),
    nl,
    table(CARDS, ['<a href="x">The CI review</a>', 'Posting a <code>verdict</code>']),
  ]);
  // The marker and the table are gone: the figure is the one copy on the page.
  assert.deepEqual(tree.children, [figure(tree)]);
  assert.deepEqual(figure(tree).properties.className, ['fig', 'fig--cards']);
  const card = byClass(tree, 'fig__cards').children[0];
  assert.equal(byClass(card, 'fig-card__role').children[0].tagName, 'a');
  assert.equal(toText(byClass(card, 'fig-card__acts')), 'Posting a verdict');
});

test('an unknown kind fails the build', () => {
  assert.throws(() => run([marker('chart'), nl, table(CARDS)]), /unknown figure kind "chart"/);
});

test('a marker must sit right above what it marks', () => {
  assert.throws(
    () => run([marker('cards'), nl, html('<p>text</p>'), table(CARDS)]),
    /figure "cards" needs a table/
  );
});

test("a table without the kind's columns fails the build, naming them", () => {
  assert.throws(
    () => run([marker('cards'), nl, table(['who', 'what'], ['x', 'y'])]),
    /role \| acts by/
  );
});

test('any other HTML comment stays as it is', () => {
  const other = { type: 'raw', value: '<!-- prettier-ignore -->' };
  assert.equal(run([other, nl, table(CARDS)]).children[0], other);
});

test("through Astro's own markdown processor, a marked table becomes a figure", async () => {
  const proc = await createMarkdownProcessor({ rehypePlugins: [rehypeFigures] });
  const { code } = await proc.render(
    '<!-- figure: cards -->\n\n| Role | Acts by |\n| - | - |\n| a | b |\n'
  );
  assert.match(code, /<figure class="fig fig--cards">/);
  assert.doesNotMatch(code, /figure: cards|<table/);
});
