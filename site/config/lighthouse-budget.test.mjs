import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const config = JSON.parse(
  readFileSync(new URL('../lighthouserc.json', import.meta.url), 'utf8')
).ci;

test('performance budgets use stable samples', () => {
  // "The median Lighthouse score of 5 runs is twice as stable as 1 run" (docs/variability.md)
  assert.ok(config.collect.numberOfRuns >= 5);
  for (const audit of [
    'categories:performance',
    'first-contentful-paint',
    'largest-contentful-paint',
    'speed-index',
    'interactive',
    'total-blocking-time',
    'cumulative-layout-shift',
  ]) {
    assert.equal(
      config.assert.assertions[audit][1].aggregationMethod,
      'median',
      `${audit} must aggregate volatile CI samples by median`
    );
  }
});

test('the first-visit reveal has a hard upper bound', () => {
  assert.deepEqual(config.assert.assertions['user-timings:pixtuoid-revealed'], [
    'error',
    { maxNumericValue: 6500, aggregationMethod: 'pessimistic' },
  ]);
});
