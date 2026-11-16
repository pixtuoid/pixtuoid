// `<!-- figure: <kind> -->` above a GFM table or a ```mermaid block marks a
// figure: GitHub hides the comment
// (docs.github.com/en/get-started/writing-on-github/getting-started-with-writing-and-formatting-on-github/basic-writing-and-formatting-syntax#hiding-content-with-comments)
// and shows the table or diagram, and this build turns the same block into the
// figure, so a doc holds one copy of its data. The output is complete static markup (a reader without JS sees the
// final state); Figures.astro's script only adds the step-through.
import { toText } from 'hast-util-to-text';
import { visit } from 'unist-util-visit';

// Astro keeps a doc's HTML comment as a `raw` node: it never parses raw HTML.
const MARKER = /^<!--\s*figure:\s*([\w-]+)\s*-->$/;

const el = (tagName, className, children = [], properties = {}) => ({
  type: 'element',
  tagName,
  properties: { ...(className && { className: [className] }), ...properties },
  children,
});
const text = (value) => ({ type: 'text', value });

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

const hasClass = (node, name) => node.properties?.className?.includes(name);

// Shrunk to a phone's width a ten-node diagram's labels are unreadable, so the
// body scrolls instead: the diagram never draws smaller than Mermaid laid it out.
function atDrawnSize(svg) {
  const style = [svg.properties.style, `min-width:${svg.properties.width}px`].filter(Boolean);
  svg.properties.style = style.join(';');
  return svg;
}

// The nodes edges only leave (where the path starts) or only enter (where it ends).
function markEnds(svg) {
  const edges = svg.children.filter((c) => hasClass(c, 'edge'));
  const from = new Set(edges.map((e) => e.properties.dataFrom));
  const to = new Set(edges.map((e) => e.properties.dataTo));
  for (const node of svg.children.filter((c) => hasClass(c, 'node'))) {
    const id = node.properties.dataId;
    if (from.has(id) !== to.has(id)) node.properties.className.push('fig-flow__end');
  }
  return svg;
}

const KINDS = {
  cards: {
    needs: 'table',
    build(node) {
      const cards = rows('cards', node, ['role', 'acts by']).map(([role, acts]) =>
        el('li', 'fig-card', [el('p', 'fig-card__role', role), el('p', 'fig-card__acts', acts)])
      );
      return [el('ul', 'fig-cards', cards)];
    },
  },
  flow: {
    // rehype-beautiful-mermaid runs first, so the block is already an <svg>.
    needs: 'svg',
    build: (svg) => [
      // A terminal bar like the callouts'. The play control ships hidden and the
      // script reveals it: a dead button would greet a reader without JS.
      el('div', 'fig__bar', [
        el('span', 'fig__title', [text('~ flow')], { ariaHidden: 'true' }),
        el('button', 'fig__play', [text('Replay')], { type: 'button', hidden: true }),
      ]),
      el('div', 'fig__body', [atDrawnSize(markEnds(svg))]),
    ],
  },
};

const FIGURE_KINDS = Object.keys(KINDS);

const NEEDS = { table: 'a table', svg: 'a Mermaid diagram' };

export default function rehypeFigures() {
  return (tree) => {
    visit(tree, 'raw', (node, i, parent) => {
      const kind = MARKER.exec(node.value.trim())?.[1];
      if (!kind || !parent) return;
      const spec = KINDS[kind];
      if (!spec)
        throw new Error(`unknown figure kind "${kind}" (known: ${FIGURE_KINDS.join(', ')})`);
      let j = i + 1;
      while (parent.children[j]?.type === 'text' && !parent.children[j].value.trim()) j++;
      const target = parent.children[j];
      if (!isElement(target, spec.needs)) {
        throw new Error(`figure "${kind}" needs ${NEEDS[spec.needs]} right below its marker`);
      }
      const figure = el('figure', null, spec.build(target), { className: ['fig', `fig--${kind}`] });
      parent.children.splice(i, j - i + 1, figure);
      return i;
    });
  };
}
