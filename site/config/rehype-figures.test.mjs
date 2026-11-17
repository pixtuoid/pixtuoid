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
const figure = (tree) => tree.children.find((c) => c.tagName === 'figure');

test('cards turn each row into a role card that keeps its links, and the marker leaves', () => {
  const tree = run([
    marker('cards'),
    nl,
    table(CARDS, ['<a href="x">The CI review</a>', 'Posting a <code>verdict</code>']),
  ]);
  // The marker and the table are gone: the figure is the one copy on the page.
  assert.deepEqual(tree.children, [figure(tree)]);
  assert.deepEqual(figure(tree).properties.className, ['fig--cards']);
  const [cards] = figure(tree).children;
  assert.deepEqual(cards.properties.className, ['fig__cards']);
  const [role, acts] = cards.children[0].children;
  assert.deepEqual(role.properties.className, ['fig-card__role']);
  assert.equal(role.children[0].tagName, 'a');
  assert.deepEqual(acts.properties.className, ['fig-card__acts']);
  assert.equal(toText(acts), 'Posting a verdict');
});

const BARS = ['Measure', 'Before', 'After'];
const fills = (bar) =>
  bar.children
    .filter((c) => c.properties.className.includes('fig-bar__row'))
    .map((row) => row.children.find((c) => c.tagName === 'svg').children[0].properties.width);

test('bars draw each row as a before and an after bar, the longer one full width', () => {
  const tree = run([
    marker('bars'),
    nl,
    table(
      BARS,
      ['<a href="x">Night frame</a>', '43.4 ms', '9.9 ms'],
      ['Memory', '562.6 KB', '639.5 KB']
    ),
  ]);
  assert.deepEqual(figure(tree).properties.className, ['fig--bars']);
  const [faster, larger] = figure(tree).children[0].children;
  assert.equal(faster.children[0].children[0].tagName, 'a');
  assert.deepEqual(fills(faster), [100, 22.8]);
  assert.deepEqual(fills(larger), [88, 100]);
  // The numbers stay on the page as text: the bars are decoration beside them.
  assert.match(toText(faster), /before\s*43\.4 ms/);
  assert.match(toText(faster), /after\s*9\.9 ms/);
});

test('a bars cell that does not start with a number fails the build, naming it', () => {
  assert.throws(
    () => run([marker('bars'), nl, table(BARS, ['Frames', 'about 5', '1'])]),
    /figure "bars": "about 5" does not start with a number/
  );
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
  assert.match(code, /<figure class="fig--cards">/);
  assert.doesNotMatch(code, /figure: cards|<table/);
});
