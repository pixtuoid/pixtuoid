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

// A moving figure's terminal window: its title, and the switch that stops it (WCAG 2.2's SC 2.2.2
// asks for one on motion that starts by itself and lasts: www.w3.org/TR/WCAG22/#pause-stop-hide).
const playing = (title, body) => [
  h('div.fig__bar', [
    h('span.fig__title', title),
    h('label.fig__pause', [h('input', { type: 'checkbox' }), ' pause']),
  ]),
  h('div.fig__body', body),
];

// A lap shorter than this reads as a flicker, not as a bar filling.
const MIN_LAP_MS = 900;

/** The slow motion that brings the fastest lap up to a watchable one: 1, 2 or 5 times a power of ten. */
function slowMotion(fastestMs) {
  const least = MIN_LAP_MS / fastestMs;
  const decade = 10 ** Math.floor(Math.log10(least));
  return decade * [1, 2, 5, 10].find((step) => step * decade >= least);
}

function lapMs(cell) {
  const text = toText({ type: 'root', children: cell }).trim();
  const ms = /^([\d.]+)\s*ms$/.exec(text)?.[1];
  if (!ms) throw new Error(`figure "laps": "${text}" is not a time in ms`);
  return Number(ms);
}

// The frame a repaint figure draws, and what it holds: the furniture is there to be a picture
// worth repainting, and the boxes are where the second pass's changes are. Neither is to scale.
const FRAME = { width: 96, height: 54 };
const FURNITURE = [
  [8, 34, 14, 8],
  [28, 34, 14, 8],
  [48, 34, 14, 8],
  [68, 34, 14, 8],
  [8, 8, 30, 12],
  [58, 8, 30, 12],
];
const CHANGES = [
  [14, 12, 6, 4],
  [52, 37, 5, 3],
  [80, 15, 4, 5],
];
const box = (className, [x, y, width, height]) => s('rect', { className, x, y, width, height });

const pane = (pass, pixels, washes) =>
  h('li.fig-pane', [
    s('svg', { viewBox: `0 0 ${FRAME.width} ${FRAME.height}`, ariaHidden: 'true' }, [
      box('fig-pane__frame', [0, 0, FRAME.width, FRAME.height]),
      ...FURNITURE.map((at) => box('fig-pane__desk', at)),
      ...washes.map((at) => box('fig-pane__wash', at)),
    ]),
    h('p.fig-pane__pass', pass),
    h('p.fig-pane__pixels', pixels),
  ]);

// Each kind draws its figure's content from the table it marks.
const KINDS = {
  laps: (table) => {
    const runners = rows('laps', table, ['runner', 'lap']).map(([who, lap]) => ({
      who,
      lap,
      ms: lapMs(lap),
    }));
    const slowed = slowMotion(Math.min(...runners.map((runner) => runner.ms)));
    const longest = Math.max(...runners.map((runner) => runner.ms));
    return playing(`${slowed}× slow motion: a bar fills once a lap`, [
      h(
        'ul.fig__laps',
        runners.map(({ who, lap, ms }) =>
          h('li.fig-lap', { style: `--lap:${Number(((ms * slowed) / 1000).toFixed(2))}s` }, [
            h('p.fig-lap__who', who),
            s(
              'svg.fig-lap__track',
              { viewBox: `0 0 ${TRACK} 1`, preserveAspectRatio: 'none', ariaHidden: 'true' },
              s('rect', { width: Math.round((ms / longest) * TRACK * 10) / 10, height: 1 })
            ),
            h('p.fig-lap__time', lap),
          ])
        )
      ),
    ]);
  },
  repaint: (table) => {
    const passes = rows('repaint', table, ['pass', 'pixels']);
    if (passes.length !== 2)
      throw new Error(`figure "repaint" compares two passes, found ${passes.length}`);
    const [[whole, wholePixels], [changed, changedPixels]] = passes;
    return playing('one frame, repainted (not to scale)', [
      h('ul.fig__panes', [
        pane(whole, wholePixels, [[0, 0, FRAME.width, FRAME.height]]),
        pane(changed, changedPixels, CHANGES),
      ]),
    ]);
  },
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
