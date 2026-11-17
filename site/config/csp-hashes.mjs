// astro.config.mjs's astro:build:done hook walks dist/ and calls
// rewriteCspMeta() per page.
import { createHash } from 'node:crypto';

const HASH = /^'sha(256|384|512)-/;

// Quote-aware opening tag: it ends at the first `>` that is NOT inside a quoted
// attribute value, so `data-x="a>b"` can't truncate it and hash the wrong bytes
// (a CSP block in production only). The end tag must match everything a browser
// treats as a script close, including the parser-error forms `</script >` and
// `</script foo="bar">` — leaving content past a fake-strict `</script>`
// unhashed is the CodeQL js/bad-tag-filter primitive.
const SCRIPT_RE = /<script\b((?:[^>"']|"[^"]*"|'[^']*')*)>([\s\S]*?)<\/script[^>]*>/gi;

// A real `src` ATTRIBUTE (external script → rides 'self', no hash). Quoted
// values are stripped first so a `src=` inside another attribute's VALUE can't
// be mistaken for the attribute; the `(?:^|\s)` boundary keeps `data-src=` out.
function hasSrcAttr(attrs) {
  return /(?:^|\s)src\s*=/i.test(attrs.replace(/"[^"]*"|'[^']*'/g, ''));
}

/**
 * The set of `'sha256-…'` tokens for every inline <script> in `html`.
 * @param {string} html
 * @returns {Set<string>}
 */
export function inlineScriptHashes(html) {
  const hashes = new Set();
  for (const m of html.matchAll(SCRIPT_RE)) {
    if (hasSrcAttr(m[1] ?? '')) continue;
    hashes.add(`'sha256-${createHash('sha256').update(m[2], 'utf8').digest('base64')}'`);
  }
  return hashes;
}

// An inline <style> element, quote-aware the same way, and any end tag a
// browser treats as its close.
const STYLE_EL_RE = /<style\b((?:[^>"']|"[^"]*"|'[^']*')*)>([\s\S]*?)<\/style[^>]*>/gi;
// An opening tag's attributes, quote-aware, scanned once the script and style
// bodies are blanked so their text can't pose as markup.
const TAG_RE = /<[a-zA-Z][^\s/>]*((?:[^>"']|"[^"]*"|'[^']*')*)>/g;
// One attribute, walked in order: a name, then an optional value, quoted or
// not, so a `style=` inside another attribute's quoted value is never a name.
const ATTR_RE = /([^\s"'<>/=]+)(?:\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s"'=<>`]+)))?/g;

// The first attribute named `style`, as a browser takes a duplicate's first.
function styleAttr(attrs) {
  for (const a of attrs.matchAll(ATTR_RE)) {
    if (a[1].toLowerCase() === 'style') return a[2] ?? a[3] ?? a[4] ?? '';
  }
  return null;
}

const ENTITIES = { amp: '&', lt: '<', gt: '>', quot: '"', apos: "'" };

// A CSP hash covers an attribute's VALUE as parsed, so its character
// references are decoded first.
// A reference it can't decode fails the build: a hash of its raw text would
// silently block the style.
function decodeAttr(value) {
  return value.replace(/&(?:#x([0-9a-f]+)|#([0-9]+)|([a-z]+));/gi, (ref, hex, dec, name) => {
    if (hex || dec) {
      const cp = hex ? parseInt(hex, 16) : parseInt(dec, 10);
      // HTML's tokenizer remaps these (NUL and surrogates to U+FFFD, 0x80–0x9F
      // per windows-1252), so a literal decode would hash other text
      if (
        cp > 0x10ffff ||
        cp === 0 ||
        (cp >= 0xd800 && cp <= 0xdfff) ||
        (cp >= 0x80 && cp <= 0x9f)
      ) {
        throw new Error(
          `csp-hashes: ${ref} is one HTML's tokenizer may remap; write the character itself`
        );
      }
      return String.fromCodePoint(cp);
    }
    const ch = ENTITIES[name.toLowerCase()];
    if (ch === undefined) throw new Error(`csp-hashes: ${ref} is not in ENTITIES; add it`);
    return ch;
  });
}

const sha256 = (text) => `'sha256-${createHash('sha256').update(text, 'utf8').digest('base64')}'`;

/**
 * The `'sha256-…'` tokens for every inline <style> element and every `style`
 * attribute in `html`, and whether any attribute needs `'unsafe-hashes'`.
 * @param {string} html
 * @returns {{ hashes: Set<string>, attributes: boolean }}
 */
export function inlineStyleHashes(html) {
  const hashes = new Set();
  for (const m of html.matchAll(STYLE_EL_RE)) hashes.add(sha256(m[2]));
  const markup = html.replace(SCRIPT_RE, '<script>').replace(STYLE_EL_RE, '<style>');
  let attributes = false;
  for (const tag of markup.matchAll(TAG_RE)) {
    const style = styleAttr(tag[1] ?? '');
    if (style === null) continue;
    hashes.add(sha256(decodeAttr(style)));
    attributes = true;
  }
  return { hashes, attributes };
}

// The whole CSP element, not just its content attribute: it is RELOCATED as well
// as rewritten. Depends on Astro rendering the attributes in this fixed order.
const CSP_META_RE = /<meta http-equiv="content-security-policy" content="([^"]*)"\s*\/?>/i;

// Where the policy is re-anchored. A `<meta http-equiv>` CSP governs only the
// content that FOLLOWS it, and Astro emits it after whatever `<script>`/`<style>`
// the layout wrote above the head-injection point — which therefore ran
// unpoliced. Anchoring on the charset rather than `<head>` keeps `<meta charset>`
// inside the first 1024 bytes the encoding sniffer reads; the policy's hashes
// would otherwise push it out.
const CHARSET_RE = /<meta[^>]*\scharset\s*=[^>]*>/i;
const HEAD_OPEN_RE = /<head\b[^>]*>/i;

/**
 * Rewrite the CSP <meta> with the hashes of every inline script and style the
 * page carries (and `'unsafe-hashes'` when a style attribute needs it), in
 * place of any the build wrote, and hoist it above everything it governs.
 * @param {string} html
 * @returns {string | null} the rewritten html, or null if no CSP <meta> exists
 * @throws if a CSP <meta> exists but the document has no charset/<head> anchor
 */
export function rewriteCspMeta(html) {
  const found = html.match(CSP_META_RE);
  if (!found || found.index === undefined) return null;
  const scripts = inlineScriptHashes(html);
  const styles = inlineStyleHashes(html);
  const directives = found[1]
    .split(';')
    .map((d) => {
      const toks = d.trim().split(/\s+/).filter(Boolean);
      if (toks[0] !== 'script-src' && toks[0] !== 'style-src') return d.trim();
      const resources = toks.slice(1).filter((t) => !HASH.test(t) && t !== "'unsafe-hashes'");
      const add =
        toks[0] === 'script-src'
          ? [...scripts]
          : [...(styles.attributes ? ["'unsafe-hashes'"] : []), ...styles.hashes];
      return [toks[0], ...resources, ...add].join(' ');
    })
    .filter(Boolean)
    .join('; ');

  const stripped = html.slice(0, found.index) + html.slice(found.index + found[0].length);
  const anchor = stripped.match(CHARSET_RE) ?? stripped.match(HEAD_OPEN_RE);
  if (!anchor || anchor.index === undefined) {
    throw new Error('csp-hashes: no charset/<head> anchor to hoist the CSP <meta> to');
  }
  const at = anchor.index + anchor[0].length;
  const meta = `<meta http-equiv="content-security-policy" content="${directives}">`;
  return stripped.slice(0, at) + meta + stripped.slice(at);
}
