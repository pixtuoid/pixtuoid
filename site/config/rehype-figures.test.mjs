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

const all = (node, test, out = []) => {
  if (test(node)) out.push(node);
  for (const child of node.children ?? []) all(child, test, out);
  return out;
};
const withClass = (name) => (n) => n.properties?.className?.includes(name);
const said = (node) => toText(node).replace(/\s+/g, ' ').trim();

test('a budget timeline starts a frame on its beat, or late when the one before overran', () => {
  const tree = run([
    marker('budget'),
    nl,
    table(
      ['Frame', 'Takes', 'Budget'],
      ['Before', '43.4 ms', '33 ms'],
      ['After', '9.9 ms', '33 ms']
    ),
  ]);
  const fig = figure(tree);
  assert.deepEqual(fig.properties.className, ['fig--budget']);
  const [before, after] = all(fig, withClass('fig-budget')).map((row) =>
    all(row, withClass('fig-budget__run')).map((r) => ({
      x: r.properties.x,
      late: r.properties.className.includes('fig-budget__run--late'),
    }))
  );
  // 43.4 ms against 33: every frame after the first starts where the last one ended.
  assert.deepEqual(before, [
    { x: 0, late: false },
    { x: 131.5, late: true },
    { x: 263, late: true },
    { x: 394.5, late: true },
  ]);
  // 9.9 ms against 33: one on every beat.
  assert.deepEqual(
    after,
    [0, 100, 200, 300].map((x) => ({ x, late: false }))
  );
  assert.equal(
    all(fig, (n) => n.tagName === 'input' && n.properties.type === 'checkbox').length,
    1
  );
  assert.match(said(fig), /Before 43\.4 ms/);
});

test('a budget cell that is not a time in ms fails the build', () => {
  assert.throws(
    () => run([marker('budget'), nl, table(['Frame', 'Takes', 'Budget'], ['x', 'fast', '33 ms'])]),
    /figure "budget": "fast" is not a time in ms/
  );
});

test('repaint shows its two passes side by side, each with what it repaints', () => {
  const tree = run([
    marker('repaint'),
    nl,
    table(['Pass', 'Pixels'], ['The whole frame', '6.8 million'], ['Only what changed', '7,936']),
  ]);
  const fig = figure(tree);
  assert.deepEqual(fig.properties.className, ['fig--repaint']);
  const panes = all(fig, withClass('fig-pane'));
  assert.deepEqual(panes.map(said), ['The whole frame 6.8 million', 'Only what changed 7,936']);
  // The first pass washes the frame; the second washes a few boxes inside it.
  const washes = panes.map((pane) => all(pane, withClass('fig-pane__wash')).length);
  assert.equal(washes[0], 1);
  assert.ok(washes[1] > 1);
});

test('tiles cut the same frame fine and coarse, and nothing in it moves', () => {
  const tree = run([
    marker('tiles'),
    nl,
    table(
      ['Tiles', 'Images'],
      ['Small tiles', 'more than the terminal keeps up with'],
      ['Big tiles', '400 at most']
    ),
  ]);
  const fig = figure(tree);
  const [fine, coarse] = all(fig, withClass('fig-pane__cuts')).map(
    (path) => path.properties.d.split('M').length - 1
  );
  assert.ok(fine > coarse);
  assert.equal(all(fig, (n) => n.tagName === 'input').length, 0);
  assert.deepEqual(all(fig, withClass('fig-pane')).map(said), [
    'Small tiles more than the terminal keeps up with',
    'Big tiles 400 at most',
  ]);
});

test('a two-pane figure needs exactly its two rows', () => {
  assert.throws(
    () => run([marker('repaint'), nl, table(['Pass', 'Pixels'], ['Only one', '1'])]),
    /figure "repaint" compares two, found 1/
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
