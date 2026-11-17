// The READMEs, ARCHITECTURE.md and the Raycast extension link the site's floors
// and pages, and no build reads across that boundary, so renaming a floor or
// a route would strand a link silently.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { FEED_ROUTE, readingRoutes } from './blog.mjs';
import floors from '../src/floors.json' with { type: 'json' };

const read = (rel) => readFileSync(fileURLToPath(new URL(rel, import.meta.url)), 'utf8');
const floorIds = floors.map((f) => f.id);
const routes = [...readingRoutes(), FEED_ROUTE];

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

// A page's link names its route, slash and all: any other spelling redirects.
for (const rel of ['../../README.md', '../../docs/ARCHITECTURE.md']) {
  test(`${rel} links a page by the route the site builds`, () => {
    const pages = [...read(rel).matchAll(/pixtuoid\.dev\/([\w.-]+(?:\/[\w.-]+)*\/?)/g)].map(
      (m) => m[1]
    );
    assert.ok(pages.length > 0, `${rel} links no pixtuoid.dev page`);
    for (const p of pages)
      assert.ok(routes.includes(p), `/${p} is no route of the site (${routes})`);
  });
}
