// `<!-- figure: <kind> -->` above a GFM table marks a figure: GitHub hides the comment
// (docs.github.com/en/get-started/writing-on-github/getting-started-with-writing-and-formatting-on-github/basic-writing-and-formatting-syntax#hiding-content-with-comments)
// and shows the table, and this build turns the same table into the figure, so a
// doc holds one copy of its data. Wing.astro styles the markup.
import { toText } from 'hast-util-to-text';
import { visit } from 'unist-util-visit';
import { h, s } from 'hastscript';

// The marker is still a `raw` node here: remark-rehype keeps HTML raw
// (`allowDangerousHtml`), and Astro runs rehype-raw only after the site's
// rehypePlugins (@astrojs/markdown-remark's dist/index.js:55-80, at
// package-lock's version).
const MARKER = /^<!--\s*figure:\s*([\w-]+)\s*-->$/;

const isElement = (node, tagName) => node?.type === 'element' && node.tagName === tagName;

const sectionRows = (tableNode, tagName) =>
  tableNode.children
    .find((c) => isElement(c, tagName))
    ?.children.filter((c) => isElement(c, 'tr')) ?? [];

function rows(kind, tableNode, columns) {
  const [head] = sectionRows(tableNode, 'thead');
  const names = (head?.children ?? [])
    .filter((c) => isElement(c, 'th'))
    .map((c) => toText(c).trim().toLowerCase());
  if (names.join('|') !== columns.join('|')) {
    throw new Error(
      `figure "${kind}" needs the columns ${columns.join(' | ')}, found ${names.join(' | ') || 'none'}`
    );
  }
  return sectionRows(tableNode, 'tbody').map((tr) =>
    tr.children.filter((c) => isElement(c, 'td')).map((td) => td.children)
  );
}

// The track a bar fills, in the units of its `viewBox`: a width is then a percentage, and
// an attribute where a `style` would need its own CSP hash.
const TRACK = 100;

function amount(kind, cell) {
  const text = toText({ type: 'root', children: cell }).trim();
  const value = Number.parseFloat(text);
  if (Number.isNaN(value))
    throw new Error(`figure "${kind}": "${text}" does not start with a number`);
  return value;
}

const bar = (tag, cell, value, longest) =>
  h('p.fig-bar__row', [
    h('span.fig-bar__tag', tag),
    s(
      'svg.fig-bar__track',
      { viewBox: `0 0 ${TRACK} 1`, preserveAspectRatio: 'none', ariaHidden: 'true' },
      s('rect', { width: Math.round((value / longest) * TRACK * 10) / 10, height: 1 })
    ),
    h('span.fig-bar__num', cell),
  ]);

// Each kind draws its figure's content from the table it marks.
const KINDS = {
  bars: (table) =>
    h(
      'ul.fig__bars',
      rows('bars', table, ['measure', 'before', 'after']).map(([measure, before, after]) => {
        const [was, is] = [amount('bars', before), amount('bars', after)];
        const longest = Math.max(was, is);
        return h('li.fig-bar', [
          h('p.fig-bar__what', measure),
          bar('before', before, was, longest),
          bar('after', after, is, longest),
        ]);
      })
    ),
  cards: (table) =>
    h(
      'ul.fig__cards',
      rows('cards', table, ['role', 'acts by']).map(([role, acts]) =>
        h('li.fig-card', [h('p.fig-card__role', role), h('p.fig-card__acts', acts)])
      )
    ),
};

const FIGURE_KINDS = Object.keys(KINDS);

// The shell every figure shares: its class names its kind (Wing.astro frames
// `fig--diagram`; cards are styled through `.fig__cards`).
export const figure = (kind, children) => h('figure', { className: [`fig--${kind}`] }, children);

export default function rehypeFigures() {
  return (tree) => {
    visit(tree, 'raw', (node, i, parent) => {
      const kind = MARKER.exec(node.value.trim())?.[1];
      if (!kind || !parent) return;
      const draw = KINDS[kind];
      if (!draw)
        throw new Error(`unknown figure kind "${kind}" (known: ${FIGURE_KINDS.join(', ')})`);
      let j = i + 1;
      while (parent.children[j]?.type === 'text' && !parent.children[j].value.trim()) j++;
      const target = parent.children[j];
      if (!isElement(target, 'table')) {
        throw new Error(`figure "${kind}" needs a table right below its marker`);
      }
      parent.children.splice(i, j - i + 1, figure(kind, draw(target)));
      return i;
    });
  };
}
