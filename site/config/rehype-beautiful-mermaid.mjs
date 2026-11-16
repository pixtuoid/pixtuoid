// ```mermaid → a figure holding an inline <svg>, at build time, with no browser:
// beautiful-mermaid lays the graph out in-process, so the site build needs no
// browser to render a doc. The SVG's colors ride tokens — --bg/--fg by
// inheritance, the library's --surface/--border/--muted hooks by name
// (Docs.astro's `.fig--flow svg` owns that pairing) — so one render follows
// every theme.
import { renderMermaidSVG } from 'beautiful-mermaid';
import { fromHtml } from 'hast-util-from-html';
import { SKIP, visit } from 'unist-util-visit';
import { el } from './hast.mjs';

// Mermaid's accessibility directives are not part of beautiful-mermaid's grammar
// (it draws them as nodes), so they are lifted out here and put back as the
// <title>/<desc> the SVG is labelled by.
const ACC_DIRECTIVE = /^\s*acc(Title|Descr)\s*:\s*(.*?)\s*$/;

// The library's stylesheet opens with an @import of Google Fonts: the site's CSP
// (style-src 'self') blocks it — smoke.spec.ts's office-less first-visit console
// watchdog reds on the block — and the text falls to the same system-ui either way.
// The stack stays the library's own: node boxes are sized from Inter's glyph
// widths, so binding --font-body (a serif) can overflow them.
const FONT_IMPORT = /@import\s+url\([^)]*\)[^;]*;\s*/g;

// The root carries literal --bg/--fg (the library's defaults). Passing the page's
// var(--bg)/var(--fg) as bg/fg instead would emit `--bg:var(--bg)`, a
// self-referencing custom property, invalid at computed-value time, so every
// color-mix() falls to black. Dropping the declarations lets the library's
// var(--bg)/var(--fg) references inherit the page's values directly.
const ROOT_TOKENS = /--(bg|fg)\s*:[^;]*;?/g;

function splitAccessibility(source) {
  const acc = {};
  const lines = source.split('\n').filter((line) => {
    const m = ACC_DIRECTIVE.exec(line);
    if (!m) return true;
    acc[m[1]] = m[2];
    return false;
  });
  return { source: lines.join('\n'), title: acc.Title, descr: acc.Descr };
}

function mermaidSource(node) {
  if (node.type !== 'element' || node.tagName !== 'pre') return null;
  const code = node.children.find((c) => c.type === 'element' && c.tagName === 'code');
  if (!code || !(code.properties?.className ?? []).includes('language-mermaid')) return null;
  return code.children.map((c) => c.value ?? '').join('');
}

function inheritPageTokens(svg) {
  const style = String(svg.properties.style ?? '')
    .replace(ROOT_TOKENS, '')
    .trim();
  if (style) svg.properties.style = style;
  else delete svg.properties.style;
  for (const sheet of svg.children.filter((c) => c.tagName === 'style')) {
    for (const text of sheet.children) text.value = text.value.replace(FONT_IMPORT, '');
  }
  return svg;
}

function labelled(svg, index, title, descr) {
  const id = `diagram-${index}`;
  const label = (tagName, value) => ({
    type: 'element',
    tagName,
    properties: { id: `${id}-${tagName}` },
    children: [{ type: 'text', value }],
  });
  // WCAG 1.1.1: a diagram is non-text content, so it ships its text alternative
  if (!title || !descr) {
    throw new Error(`mermaid diagram ${index} needs accTitle and accDescr (WCAG 1.1.1)`);
  }
  const labels = [label('title', title), label('desc', descr)];
  svg.children.unshift(...labels);
  svg.properties.role = 'img';
  svg.properties.ariaLabelledBy = labels.map((l) => l.properties.id).join(' ');
  return svg;
}

const hasClass = (node, name) => node.properties?.className?.includes(name);

// Shrunk to a phone's width a ten-node diagram's labels are unreadable, so its
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
    if (from.has(id) !== to.has(id)) node.properties.className.push('fig__end');
  }
  return svg;
}

// A terminal window like a callout's: a bar, then a body that scrolls a diagram
// wider than the page (rehype-scroll-focus.mjs makes that body a tab stop).
const framed = (svg) =>
  el(
    'figure',
    ['fig', 'fig--flow'],
    [
      el('div', 'fig__bar', [el('span', 'fig__title', [{ type: 'text', value: '~ flow' }])], {
        ariaHidden: 'true',
      }),
      el('div', 'fig__body', [atDrawnSize(markEnds(svg))]),
    ]
  );

function render(raw, index) {
  const { source, title, descr } = splitAccessibility(raw);
  const html = renderMermaidSVG(source, { transparent: true });
  const svg = fromHtml(html, { fragment: true }).children.find(
    (c) => c.type === 'element' && c.tagName === 'svg'
  );
  if (!svg) throw new Error('beautiful-mermaid returned no <svg>');
  return framed(labelled(inheritPageTokens(svg), index, title, descr));
}

export default function rehypeBeautifulMermaid() {
  return (tree) => {
    let index = 0;
    visit(tree, (node, i, parent) => {
      const source = mermaidSource(node);
      if (source === null || !parent) return;
      parent.children[i] = render(source, index++);
      return SKIP;
    });
  };
}
