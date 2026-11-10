// The README and the Raycast extension link the site's floors by id, and no
// build reads across that boundary, so renaming a floor would strand a link silently.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const read = (rel) => readFileSync(fileURLToPath(new URL(rel, import.meta.url)), 'utf8');
const floorIds = [...read('../src/consts.ts').matchAll(/\{ id: '([^']+)', fl: /g)].map((m) => m[1]);

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
