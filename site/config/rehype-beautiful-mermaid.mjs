// ```mermaid → inline <svg> at build time, with no browser: beautiful-mermaid
// lays the graph out in-process, so the site build needs no browser to render
// /architecture. The SVG's colors ride the page's tokens — --bg/--fg by
// inheritance, the library's --surface/--border/--muted hooks by name
// (global.css's `.prose svg` owns that pairing, and Figures.astro's
// `.prose .fig--flow svg` a flow figure's) — so one render follows every theme.
import { renderMermaidSVG } from 'beautiful-mermaid';
import { fromHtml } from 'hast-util-from-html';
import { SKIP, visit } from 'unist-util-visit';

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

function render(raw, index) {
  const { source, title, descr } = splitAccessibility(raw);
  const html = renderMermaidSVG(source, { transparent: true });
  const svg = fromHtml(html, { fragment: true }).children.find(
    (c) => c.type === 'element' && c.tagName === 'svg'
  );
  if (!svg) throw new Error('beautiful-mermaid returned no <svg>');
  return labelled(inheritPageTokens(svg), index, title, descr);
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
