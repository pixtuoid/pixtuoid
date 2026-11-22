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

// What moves in a figure carries MOVING, and its figure REVEAL, the page's reveal-on-scroll
// class, which Base.astro's observer completes: Wing.astro plays the motion from there.
const MOVING = 'fig__moving';
const REVEAL = 'reveal';

function moves(body) {
  let found = false;
  visit({ type: 'root', children: body }, 'element', (node) => {
    found ||= [node.properties.className].flat().includes(MOVING);
  });
  return found;
}

// A drawn figure's terminal window.
export const windowed = (title, body, { bar = {}, scroller = {} } = {}) => [
  h('div.fig__bar', bar, [h('span.fig__title', title)]),
  h('div.fig__body', scroller, body),
];

function milliseconds(kind, cell) {
  const text = toText({ type: 'root', children: cell }).trim();
  const ms = /^(\d+(?:\.\d+)?)\s*ms$/.exec(text)?.[1];
  if (!ms) throw new Error(`figure "${kind}": "${text}" is not a time in ms`);
  return Number(ms);
}

// A budget figure's timeline: how many budgets it runs for, and one budget's width in the units
// of its `viewBox`.
const BEATS = 4;
const BEAT = 100;
// The lane the frames run in, the margin a beat's tick reaches past it, and the gap that keeps two
// frames run back to back apart.
const LANE = 10;
const TICK = 3;
const GAP = 1;

/**
 * The frames a loop shows in the timeline: one is due on every beat, and starts there or when the
 * one before it ends, whichever is later.
 */
function runs(takes, budget) {
  const shown = [];
  for (let due = 0, start = 0; start < BEATS * budget; due += budget) {
    start = Math.max(start, due);
    if (start >= BEATS * budget) break;
    shown.push({ start, late: start > due });
    start += takes;
  }
  return shown;
}

// What a timeline's lengths are drawn to: tenths of a unit keep a frame's edge on its beat.
const PER_UNIT = 10;
const drawn = (units) => Math.round(units * PER_UNIT) / PER_UNIT;

const timeline = (takes, budget) =>
  s(
    'svg.fig-budget__line',
    { viewBox: `0 ${-TICK} ${BEATS * BEAT} ${LANE + 2 * TICK}`, ariaHidden: 'true' },
    [
      ...runs(takes, budget).map(({ start, late }) =>
        s('rect', {
          className: ['fig-budget__run', ...(late ? ['fig-budget__run--late'] : [])],
          x: drawn((start / budget) * BEAT),
          width: Math.max(drawn((takes / budget) * BEAT) - GAP, 0),
          height: LANE,
        })
      ),
      ...Array.from({ length: BEATS + 1 }, (_, beat) =>
        s('path.fig-budget__beat', { d: `M${beat * BEAT} ${-TICK}V${LANE + TICK}` })
      ),
      s('rect', {
        className: ['fig-budget__now', MOVING],
        y: -TICK,
        width: 2,
        height: LANE + 2 * TICK,
      }),
    ]
  );

// The frame a two-pane figure draws, and what it holds: the furniture is there to be a picture,
// `CHANGES` are where a repaint's changes are, and the tiles are a frame cut fine and cut coarse.
// None is to scale.
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
// What changed in a frame, in the groups a merge would box together: the left pane draws every
// change, the right one the box around each group.
const GROUPS = [
  [
    [4, 3, 5, 3],
    [12, 5, 4, 3],
    [8, 9, 5, 2],
  ],
  [
    [30, 2, 6, 3],
    [39, 4, 5, 3],
    [34, 8, 4, 3],
  ],
  [
    [58, 3, 4, 4],
    [64, 6, 5, 3],
  ],
  [
    [82, 4, 5, 3],
    [86, 9, 4, 4],
    [80, 10, 3, 3],
  ],
  [
    [10, 24, 5, 3],
    [17, 27, 4, 3],
  ],
  [
    [44, 22, 5, 4],
    [51, 25, 4, 3],
    [47, 29, 5, 2],
  ],
  [
    [6, 42, 5, 4],
    [13, 45, 5, 3],
    [20, 43, 4, 4],
  ],
  [
    [70, 40, 5, 3],
    [77, 43, 5, 4],
    [84, 41, 4, 3],
  ],
];
const around = (group) => {
  const x = Math.min(...group.map(([left]) => left));
  const y = Math.min(...group.map(([, top]) => top));
  return [
    x,
    y,
    Math.max(...group.map(([left, , width]) => left + width)) - x,
    Math.max(...group.map(([, top, , height]) => top + height)) - y,
  ];
};
const TILES = { fine: [16, 9], coarse: [4, 3] };
const rect =
  (...className) =>
  ([x, y, width, height]) =>
    s('rect', { className, x, y, width, height });

/** The lines that cut the frame into `across` by `down` tiles. */
function cuts([across, down]) {
  const lines = [];
  for (let i = 1; i < across; i++) lines.push(`M${(FRAME.width / across) * i} 0V${FRAME.height}`);
  for (let j = 1; j < down; j++) lines.push(`M0 ${(FRAME.height / down) * j}H${FRAME.width}`);
  return s('path.fig-pane__cuts', { d: lines.join('') });
}

const pane = (what, count, over) =>
  h('li.fig-pane', [
    s('svg', { viewBox: `0 0 ${FRAME.width} ${FRAME.height}`, ariaHidden: 'true' }, [
      rect('fig-pane__frame')([0, 0, FRAME.width, FRAME.height]),
      ...FURNITURE.map(rect('fig-pane__desk')),
      ...over,
    ]),
    h('p.fig-pane__what', what),
    h('p.fig-pane__count', count),
  ]);

/** A two-pane figure's two rows, or the build fails naming how many it found. */
function pair(kind, table, columns) {
  const found = rows(kind, table, columns);
  if (found.length !== 2) throw new Error(`figure "${kind}" compares two, found ${found.length}`);
  return found;
}

// A journey's stops, by the role the How it works page's script fills each with (public/journey.js). Each row
// names its stop "The <role>", which is the join: a row out of this order fails the build.
export const JOURNEY_STOPS = ['agent', 'shim', 'decoder', 'office', 'frame'];

// `cards`, `budget` and `journey` draw their content from the table they mark; the two-pane kinds
// draw a fixed picture and take only their captions from it.
const KINDS = {
  journey: (table) => {
    const found = rows('journey', table, ['stop', 'in the code', 'what happens', 'why']);
    if (found.length !== JOURNEY_STOPS.length) {
      throw new Error(`figure "journey" has ${JOURNEY_STOPS.length} stops, found ${found.length}`);
    }
    found.forEach(([name], i) => {
      const said = toText({ type: 'root', children: name }).trim();
      if (said.toLowerCase() !== `the ${JOURNEY_STOPS[i]}`) {
        throw new Error(
          `figure "journey": stop ${i + 1} is "${said}", where "The ${JOURNEY_STOPS[i]}" goes`
        );
      }
    });
    return h(
      'ol.fig-journey__stops',
      found.map(([name, code, what, why], i) =>
        h('li.fig-journey__stop', { dataStop: JOURNEY_STOPS[i] }, [
          h('p.fig-journey__head', [h('span.fig-journey__name', name), ' ', ...code]),
          h('p.fig-journey__what', what),
          h('details.fig-journey__why', [h('summary', 'Why'), h('p', why)]),
        ])
      )
    );
  },
  budget: (table) =>
    windowed('frames against their budget: a tick is when one is due', [
      h(
        'ul.fig__budgets',
        rows('budget', table, ['frame', 'takes', 'budget']).map(([frame, takes, budget]) =>
          h('li.fig-budget', [
            h('p.fig-budget__frame', frame),
            h('p.fig-budget__takes', takes),
            timeline(milliseconds('budget', takes), milliseconds('budget', budget)),
          ])
        )
      ),
    ]),
  repaint: (table) => {
    const [[whole, wholePixels], [changed, changedPixels]] = pair('repaint', table, [
      'pass',
      'pixels',
    ]);
    const washes = (areas) => areas.map(rect('fig-pane__wash', MOVING));
    return windowed('one frame, repainted (not to scale)', [
      h('ul.fig__panes', [
        pane(whole, wholePixels, washes([[0, 0, FRAME.width, FRAME.height]])),
        pane(changed, changedPixels, washes(CHANGES)),
      ]),
    ]);
  },
  boxes: (table) => {
    const [[changes, changeCount], [merged, mergedCount]] = pair('boxes', table, ['step', 'count']);
    const marks = GROUPS.flat().map(rect('fig-pane__change'));
    return windowed('what changed, and the boxes that get repainted (not to scale)', [
      h('ul.fig__panes', [
        pane(changes, changeCount, marks),
        pane(merged, mergedCount, [...marks, ...GROUPS.map(around).map(rect('fig-pane__box'))]),
      ]),
    ]);
  },
  tiles: (table) => {
    const [[small, smallCount], [big, bigCount]] = pair('tiles', table, ['tiles', 'images']);
    return windowed('one frame, cut into tiles (not to scale)', [
      h('ul.fig__panes', [
        pane(small, smallCount, [cuts(TILES.fine)]),
        pane(big, bigCount, [cuts(TILES.coarse)]),
      ]),
    ]);
  },
  cards: (table) =>
    h(
      'ul.fig__cards',
      rows('cards', table, ['role', 'acts by']).map(([role, acts]) =>
        h('li.fig-card', [h('p.fig-card__role', role), h('p.fig-card__acts', acts)])
      )
    ),
};

const FIGURE_KINDS = Object.keys(KINDS);

// The shell every figure shares: its class names its kind.
export const figure = (kind, children) =>
  h('figure', { className: [`fig--${kind}`, ...(moves(children) ? [REVEAL] : [])] }, children);

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
