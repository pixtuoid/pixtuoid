// Assert every rendered doc page, a doc's or a post's, has a body and drew every
// diagram its source declares. Born for the silent empty-render class — a render step
// collapsed a doc's <Content /> to an EMPTY <article> WITHOUT failing `astro
// build` (#680) — and GENERIC on purpose: it globs every page carrying the wing
// shell's `<article class="prose">`, so no per-page string can drift. A render
// step that throws fails `astro build` by itself (the glob loader logs and
// rethrows: astro's content/loaders/glob.js:148-150, at package.json's version);
// this is for the one that
// returns nothing without throwing.
//
// Usage: node config/assert-docs-rendered.mjs [distDir=dist]
import { readFileSync, readdirSync } from 'node:fs';
import { dirname, join, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import process from 'node:process';
import { fromMarkdown } from 'mdast-util-from-markdown';
import { visit } from 'unist-util-visit';
import { markdownPages } from './blog.mjs';
import { docSlug } from './docs.mjs';

// A generous floor: flags a collapsed render without coupling to any page's size.
export const MIN_BODY_CHARS = 500;
// \bprose\b, not a sole-class match, so a future `class="prose max-w-none"` or an
// appended Astro scoped class doesn't drop the article → redden every deploy.
const DOC_ARTICLE = /<article class="[^"]*\bprose\b[^"]*"[^>]*>([\s\S]*?)<\/article>/;

/** The source of every Mermaid code block a Markdown document holds. */
export function mermaidFences(markdown) {
  const sources = [];
  visit(fromMarkdown(markdown), { type: 'code', lang: 'mermaid' }, (code) => {
    sources.push(code.value);
  });
  return sources;
}

/**
 * Given doc-page entries `[{ name, html }]` and `diagrams`, the count of
 * Mermaid blocks each page's source declares by page name, return the failures +
 * how many doc pages were seen.
 */
export function checkDocPages(entries, diagrams) {
  const failures = [];
  let docPages = 0;
  for (const { name, html } of entries) {
    const m = html.match(DOC_ARTICLE);
    if (!m) continue; // not a wing's article
    docPages += 1;
    const body = m[1]
      .replace(/<[^>]+>/g, ' ')
      .replace(/\s+/g, ' ')
      .trim();
    if (body.length < MIN_BODY_CHARS) {
      failures.push(
        `/${name}: doc body only ${body.length} chars (< ${MIN_BODY_CHARS}) — render collapsed`
      );
    }
    if (!(name in diagrams)) {
      failures.push(
        `/${name}: a rendered article the rosters (src/docs.json, the blog) do not list`
      );
      continue;
    }
    // A diagram is the svg a reader is told about (`role="img"`); a figure's drawing is `aria-hidden`.
    const drawn = (m[1].match(/<svg\s[^>]*\brole="img"/g) ?? []).length;
    const declared = diagrams[name];
    if (drawn !== declared) {
      failures.push(
        `/${name}: ${drawn} inline <svg> for the ${declared} mermaid blocks its source declares`
      );
    }
  }
  if (docPages === 0) {
    failures.push("no doc pages found (the 'article.prose' selector drifted?)");
  }
  return { failures, docPages };
}

function main(dist) {
  const entries = readdirSync(dist, { recursive: true })
    .filter((file) => file.endsWith(`${sep}index.html`))
    .map((file) => ({
      name: dirname(file).split(sep).join('/'),
      html: readFileSync(join(dist, file), 'utf8'),
    }));
  const site = fileURLToPath(new URL('../', import.meta.url));
  const declared = (file) => mermaidFences(readFileSync(file, 'utf8')).length;
  const diagrams = Object.fromEntries(
    markdownPages().map((page) => [docSlug(page), declared(join(site, '..', page.path))])
  );
  const { failures, docPages } = checkDocPages(entries, diagrams);
  if (failures.length > 0) {
    console.error(`✗ doc-render check FAILED (${dist}):\n  ${failures.join('\n  ')}`);
    process.exit(1);
  }
  console.log(
    `✓ doc-render check: ${docPages} doc pages have a body and every diagram their source declares`
  );
}

// Only when invoked directly, so the test can import checkDocPages without
// triggering the fs read + process.exit.
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main(process.argv[2] ?? 'dist');
}
