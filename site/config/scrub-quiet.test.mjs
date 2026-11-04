// The Showcase scrubber lands each jump on office-driver.js's quietInstant: the
// engine's strike-free tail of a lightning bucket. Its two constants are copies
// of pixtuoid-scene's, pinned here so an engine change can't let a scrub flash.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { quietInstant } from '../public/office-driver.js';

const read = (rel) => readFileSync(fileURLToPath(new URL(rel, import.meta.url)), 'utf8');
const SKY = read('../../crates/pixtuoid-scene/src/sky/mod.rs');
const DRIVER = read('../public/office-driver.js');
const engine = (name) => Number(SKY.match(new RegExp(`const ${name}: u64 = (\\d+);`))?.[1]);

for (const name of ['LIGHTNING_PERIOD_MS', 'STRIKE_GAP_MS']) {
  test(`office-driver.js's ${name} is the engine's`, () => {
    const js = DRIVER.match(new RegExp(`const ${name} = (\\d+);`));
    assert.ok(js, `office-driver.js declares ${name}`);
    assert.equal(Number(js[1]), engine(name));
  });
}

test('quietInstant lands in its own bucket\u2019s strike-free tail', () => {
  const [period, gap] = [engine('LIGHTNING_PERIOD_MS'), engine('STRIKE_GAP_MS')];
  for (const t of [0, 1, period - 1, period, 1_767_000_123_456, 1_767_000_014_999]) {
    const bucket = Math.floor(t / period) * period;
    const q = quietInstant(t);
    assert.ok(q >= bucket + period - gap && q < bucket + period, `${t} -> ${q}`);
  }
});

test('the Showcase scrubber lands its jumps through quietInstant', () => {
  const showcase = read('../src/components/Showcase.astro');
  assert.match(showcase, /vBase = vDriver \? vDriver\.quietInstant\(at\) : at;/);
});
