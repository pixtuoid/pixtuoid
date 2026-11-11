// The READMEs, ARCHITECTURE.md and the Raycast extension link the site's floors
// and doc pages, and no build reads across that boundary, so renaming a floor or
// a doc route would strand a link silently.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const read = (rel) => readFileSync(fileURLToPath(new URL(rel, import.meta.url)), 'utf8');
const floorIds = [...read('../src/consts.ts').matchAll(/\{ id: '([^']+)', fl: /g)].map((m) => m[1]);
const docRoutes = [...read('../src/consts.ts').matchAll(/\{ id: '[^']+', route: '([^']+)'/g)].map(
  (m) => m[1]
);

for (const rel of [
  '../../integrations/raycast/src/manage-sources.tsx',
  '../../integrations/raycast/README.md',
  '../../README.md',
]) {
  test(`${rel} links a floor the site has`, () => {
    const anchors = [...read(rel).matchAll(/pixtuoid\.dev\/#([\w-]+)/g)].map((m) => m[1]);
    assert.ok(anchors.length > 0, `${rel} links no pixtuoid.dev floor`);
    for (const a of anchors)
      assert.ok(floorIds.includes(a), `#${a} is not a FLOORS id (${floorIds})`);
  });
}

// A doc page's link names its route, slash and all: any other spelling redirects.
for (const rel of ['../../README.md', '../../docs/ARCHITECTURE.md']) {
  test(`${rel} links a doc page by the route the site builds`, () => {
    const pages = [...read(rel).matchAll(/pixtuoid\.dev\/([\w-]+\/?)/g)].map((m) => m[1]);
    assert.ok(pages.length > 0, `${rel} links no pixtuoid.dev doc page`);
    for (const p of pages)
      assert.ok(docRoutes.includes(p), `/${p} is not a DOCS route (${docRoutes})`);
  });
}
