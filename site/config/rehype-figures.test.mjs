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

const all = (node, test, out = []) => {
  if (test(node)) out.push(node);
  for (const child of node.children ?? []) all(child, test, out);
  return out;
};
const withClass = (name) => (n) => n.properties?.className?.includes(name);

test('laps play each row as a bar refilled once a lap, slowed until the fastest is watchable', () => {
  const tree = run([
    marker('laps'),
    nl,
    table(['Runner', 'Lap'], ['Before', '43.4 ms'], ['After', '9.9 ms']),
  ]);
  const fig = figure(tree);
  assert.deepEqual(fig.properties.className, ['fig--laps']);
  assert.match(toText(fig), /100× slow motion/);
  const laps = all(fig, withClass('fig-lap'));
  assert.deepEqual(
    laps.map((lap) => lap.properties.style),
    ['--lap:4.34s', '--lap:0.99s']
  );
  // A still reader sees the same comparison: each bar is as long as its lap.
  assert.deepEqual(
    all(fig, (n) => n.tagName === 'rect').map((rect) => rect.properties.width),
    [100, 22.8]
  );
  assert.equal(
    all(fig, (n) => n.tagName === 'input' && n.properties.type === 'checkbox').length,
    1
  );
});

test('a laps cell that is not a time in ms fails the build', () => {
  assert.throws(
    () => run([marker('laps'), nl, table(['Runner', 'Lap'], ['Before', '1.24 MB'])]),
    /figure "laps": "1\.24 MB" is not a time in ms/
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
  assert.deepEqual(
    panes.map((pane) => toText(pane).replace(/\s+/g, ' ').trim()),
    ['The whole frame 6.8 million', 'Only what changed 7,936']
  );
  // The first pass washes the frame; the second washes a few boxes inside it.
  const washes = panes.map((pane) => all(pane, withClass('fig-pane__wash')).length);
  assert.equal(washes[0], 1);
  assert.ok(washes[1] > 1);
});

test('repaint needs exactly its two passes', () => {
  assert.throws(
    () => run([marker('repaint'), nl, table(['Pass', 'Pixels'], ['Only one', '1'])]),
    /figure "repaint" compares two passes, found 1/
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
