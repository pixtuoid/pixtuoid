import assert from 'node:assert/strict';
import test from 'node:test';
import config from '../lighthouserc.mjs';

const { collect, assert: asserted } = config.ci;

test('performance budgets use stable samples', () => {
  // "The median Lighthouse score of 5 runs is twice as stable as 1 run" (docs/variability.md)
  assert.ok(collect.numberOfRuns >= 5);
});

// LHCI's defaults pass what should fail: a `warn` exits 0, and an assertion without an aggregationMethod takes the
// run "most likely to pass" (github.com/GoogleChrome/lighthouse-ci/blob/main/docs/configuration.md#assertions).
// One without a bound it knows is graded as `minScore: 0.9` (`realMinScore` in @lhci/utils' assertions.js).
test('every assertion fails CI, has a bound and names how its runs aggregate', () => {
  for (const [audit, [level, options]] of Object.entries(asserted.assertions)) {
    assert.equal(level, 'error', audit);
    assert.ok('minScore' in options || 'maxNumericValue' in options, audit);
    assert.ok(['median', 'pessimistic'].includes(options.aggregationMethod), audit);
  }
});
