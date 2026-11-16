// Assert every rendered doc page has a body and that /architecture kept its
// inline diagram <svg>. Born for the silent empty-render class — a render step
// collapsed a doc's <Content /> to an EMPTY <article> WITHOUT failing `astro
// build` (#680) — and GENERIC on purpose: it globs every page carrying the Docs
// layout's `<article class="prose">`, so no per-page string can drift. A render
// step that throws fails `astro build` by itself (the glob loader logs and
// rethrows, astro/dist/content/loaders/glob.js); this is for the one that
// returns nothing without throwing.
//
// Usage: node config/assert-docs-rendered.mjs [distDir=dist]
import { readFileSync, readdirSync, existsSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import process from 'node:process';

// A generous floor: flags a collapsed render without coupling to any page's size.
export const MIN_BODY_CHARS = 500;
// \bprose\b, not a sole-class match, so a future `class="prose max-w-none"` or an
// appended Astro scoped class doesn't drop the article → redden every deploy.
const DOC_ARTICLE = /<article class="[^"]*\bprose\b[^"]*"[^>]*>([\s\S]*?)<\/article>/;

/**
 * Given doc-page entries `[{ name, html }]`, return the failures + how many
 * Docs-layout pages were seen.
 */
export function checkDocPages(entries) {
  const failures = [];
  let docPages = 0;
  for (const { name, html } of entries) {
    const m = html.match(DOC_ARTICLE);
    if (!m) continue; // not a Docs-layout page
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
    if (name === 'architecture' && !/<svg[\s>]/.test(m[1])) {
      failures.push('/architecture: no inline <svg> — the mermaid diagram did not render');
    }
  }
  if (docPages === 0) {
    failures.push("no doc pages found (the 'article.prose' selector drifted?)");
  }
  return { failures, docPages };
}

function main(dist) {
  const entries = readdirSync(dist, { withFileTypes: true })
    .filter((e) => e.isDirectory())
    .map((e) => ({ name: e.name, file: join(dist, e.name, 'index.html') }))
    .filter((e) => existsSync(e.file))
    .map((e) => ({ name: e.name, html: readFileSync(e.file, 'utf8') }));
  const { failures, docPages } = checkDocPages(entries);
  if (failures.length > 0) {
    console.error(`✗ doc-render check FAILED (${dist}):\n  ${failures.join('\n  ')}`);
    process.exit(1);
  }
  console.log(`✓ doc-render check: ${docPages} doc pages have a body; /architecture <svg> present`);
}

// Only when invoked directly, so the test can import checkDocPages without
// triggering the fs read + process.exit.
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main(process.argv[2] ?? 'dist');
}
