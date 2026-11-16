// A rendered doc is a site build input, yet whether a PR runs that build is
// decided by site.yml's `scope` path list, which no build reads.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const read = (rel) => readFileSync(fileURLToPath(new URL(rel, import.meta.url)), 'utf8');

test("site.yml's scope paths cover every doc content.config.ts renders", () => {
  const docs = [
    ...read('../src/content.config.ts').matchAll(/pattern: '([^']+)', base: '\.\.\/docs'/g),
  ].map((m) => `docs/${m[1]}`);
  assert.ok(docs.length > 0, 'content.config.ts renders no docs/ page');
  const scope = /id: scope\n\s+with:\n\s+paths: (.+)/.exec(
    read('../../.github/workflows/site.yml')
  );
  assert.ok(scope, "site.yml's `scope` step lists no paths");
  const paths = scope[1].split(' ');
  for (const doc of docs) assert.ok(paths.includes(doc), `${doc} is not in site.yml's scope paths`);
});
