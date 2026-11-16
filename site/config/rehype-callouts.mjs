// Markdown callouts → terminal-window chrome: every top-level blockquote in a
// rendered doc becomes a small terminal window — "~ note" by default, a red-dot
// "~ warning" when the quote OPENS with an imperative-warning strong.
// The red dot reuses global.css's .terminal__dot--r — no second red literal.
import { toText } from 'hast-util-to-text';
import { h } from 'hastscript';
import { EXIT, SKIP, visit } from 'unist-util-visit';

const WARN_RE = /^(don'?t|never|warning|danger|caution)\b/i;

// The FIRST <strong> in document order — the docs' "**Don't …**" idiom
// puts the imperative there; a plain editorial quote has none → note.
function firstStrongText(node) {
  let text = '';
  visit(node, { type: 'element', tagName: 'strong' }, (strong) => {
    text = toText(strong);
    return text ? EXIT : SKIP;
  });
  return text;
}

function calloutFor(blockquote) {
  // Astro's default remark-smartypants has already turned a straight "'" into
  // U+2019 by the time this hast transform runs — normalize back.
  const opener = firstStrongText(blockquote).trim().replace(/’/g, "'");
  const warn = WARN_RE.test(opener);
  const kind = warn ? 'warn' : 'note';
  return h('div', { className: ['callout', `callout--${kind}`] }, [
    h('div.callout__bar', { ariaHidden: 'true' }, [
      ...(warn ? [h('span.terminal__dot.terminal__dot--r')] : []),
      h('span.callout__title', warn ? '~ warning' : '~ note'),
    ]),
    {
      ...blockquote,
      properties: { ...(blockquote.properties || {}), className: ['callout__body'] },
    },
  ]);
}

export default function rehypeCallouts() {
  return function transform(tree) {
    visit(tree, 'element', (node, i, parent) => {
      if (node.tagName !== 'blockquote' || !parent) return;
      // wrap and DON'T descend — a nested quote stays a plain quote
      parent.children[i] = calloutFor(node);
      return SKIP;
    });
  };
}
