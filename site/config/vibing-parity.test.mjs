// The VIBING poster must be a truthful still of the live canvas it covers, and
// the layout units ARE the camera (fewer units = closer zoom). The values
// necessarily live in three files, so pin the copies together.
import { strict as assert } from 'node:assert';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import test from 'node:test';

const here = dirname(fileURLToPath(import.meta.url));
const showcase = JSON.parse(readFileSync(join(here, '..', 'src', 'showcase.json'), 'utf8'));
const media = JSON.parse(readFileSync(join(here, '..', '..', 'scripts', 'media.json'), 'utf8'));
const astro = readFileSync(join(here, '..', 'src', 'components', 'Showcase.astro'), 'utf8');
const stage = readFileSync(join(here, '..', 'src', 'components', 'ChannelStage.astro'), 'utf8');

const vibing = showcase.find((c) => c.id === 'vibing');
const poster = media.find((j) => j.id === 'vibing-poster');

test('vibing poster job mirrors the live canvas (dims + seed)', () => {
  assert.ok(vibing, 'showcase.json has a vibing channel');
  assert.ok(poster, 'media.json has a vibing-poster job');
  assert.equal(poster.w, vibing.w, 'poster width == canvas width, in layout units');
  assert.equal(poster.h, vibing.h, 'poster height == canvas height, in layout units');
  const seedMatch = astro.match(/const VIBING_SEED = (\d+)/);
  assert.ok(seedMatch, 'Showcase.astro declares VIBING_SEED as a numeric literal');
  assert.equal(
    Number(seedMatch[1]),
    poster.seed,
    'poster layout seed == the live Office constructor seed'
  );
  // else the poster→canvas crossfade reframes to a different sky/wetness
  const hourMatch = stage.match(/value="(\d+)"/);
  assert.ok(hourMatch, "ChannelStage.astro declares the time slider's default value");
  assert.equal(Number(hourMatch[1]), poster.hour, 'poster hour == slider default hour');
  const weatherMatch = stage.match(/weather: '([a-z]+)'/);
  assert.ok(weatherMatch, 'ChannelStage.astro declares the default weather chip id');
  assert.equal(weatherMatch[1], poster.weather, 'poster weather == default-active chip');
});
