import assert from 'node:assert/strict';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import net from 'node:net';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import process from 'node:process';
import test from 'node:test';
import { setTimeout as sleep } from 'node:timers/promises';

import {
  aggregate,
  evaluateAssertions,
  median,
  previewServer,
  startServer,
  stop,
} from './lighthouse-runner.mjs';

const lhr = ({ perf = 0.9, contrast = 1, lcp = 2000, mark = 5000 } = {}) => ({
  categories: { performance: { score: perf } },
  audits: {
    'color-contrast': { score: contrast },
    'largest-contentful-paint': { score: 0.5, numericValue: lcp },
    'user-timings': {
      details: {
        items: [
          { name: 'pixtuoid-revealed', startTime: mark },
          { name: 'measured-span', startTime: 1, duration: 42 },
        ],
      },
    },
  },
});

test('median handles odd and even sample counts', () => {
  assert.equal(median([3, 1, 2]), 2);
  assert.equal(median([4, 1, 2, 3]), 2.5);
});

test('pessimistic is the worst run per kind: min score, max numeric', () => {
  assert.equal(aggregate([0.8, 0.95], 'pessimistic', 'score'), 0.8);
  assert.equal(aggregate([1000, 3000], 'pessimistic', 'numeric'), 3000);
  assert.equal(aggregate([0.8, 0.95], 'optimistic', 'score'), 0.95);
  assert.equal(aggregate([1000, 3000], 'optimistic', 'numeric'), 1000);
});

test('minScore and maxNumericValue fail in the right direction', () => {
  const assertions = {
    'categories:performance': ['error', { minScore: 0.7, aggregationMethod: 'median' }],
    'largest-contentful-paint': ['error', { maxNumericValue: 9000, aggregationMethod: 'median' }],
  };
  assert.deepEqual(evaluateAssertions(assertions, 'u', [lhr(), lhr(), lhr()]), []);
  const bad = evaluateAssertions(assertions, 'u', [
    lhr({ perf: 0.5, lcp: 20000 }),
    lhr({ perf: 0.5, lcp: 20000 }),
    lhr({ perf: 0.9, lcp: 2000 }),
  ]);
  assert.equal(bad.length, 2, JSON.stringify(bad));
  assert.match(bad[0].error, /violates minScore 0\.7/);
  assert.match(bad[1].error, /violates maxNumericValue 9000/);
});

test('default aggregation is median — the documented LHCI divergence', () => {
  // One bad run out of three: median passes, pessimistic would fail.
  const runs = [lhr({ perf: 0.9 }), lhr({ perf: 0.9 }), lhr({ perf: 0.1 })];
  const noMethod = { 'categories:performance': ['error', { minScore: 0.7 }] };
  assert.deepEqual(evaluateAssertions(noMethod, 'u', runs), []);
  const pessimistic = {
    'categories:performance': ['error', { minScore: 0.7, aggregationMethod: 'pessimistic' }],
  };
  assert.equal(evaluateAssertions(pessimistic, 'u', runs).length, 1);
  // And the default is median, not optimistic: one GOOD run out of three must
  // not rescue the assertion (median 0.5 fails where optimistic 0.9 passes) —
  // the exact regression direction the divergence exists to prevent.
  const mostlyBad = [lhr({ perf: 0.9 }), lhr({ perf: 0.5 }), lhr({ perf: 0.5 })];
  assert.equal(evaluateAssertions(noMethod, 'u', mostlyBad).length, 1);
});

test('user-timings resolve a mark by startTime and a measure by duration', () => {
  const assertions = {
    'user-timings:pixtuoid-revealed': [
      'error',
      { maxNumericValue: 6500, aggregationMethod: 'pessimistic' },
    ],
  };
  assert.deepEqual(evaluateAssertions(assertions, 'u', [lhr({ mark: 6000 })]), []);
  assert.equal(evaluateAssertions(assertions, 'u', [lhr({ mark: 7000 })]).length, 1);
  // The measure fixture has startTime 1 and duration 42: a 10 bound must fail
  // via the DURATION read — a swapped `startTime ?? duration` fallback would
  // read 1 and green it.
  const measure = { 'user-timings:measured-span': ['error', { maxNumericValue: 10 }] };
  assert.equal(evaluateAssertions(measure, 'u', [lhr()]).length, 1);
  const loose = { 'user-timings:measured-span': ['error', { maxNumericValue: 50 }] };
  assert.deepEqual(evaluateAssertions(loose, 'u', [lhr()]), []);
});

test('a missing audit, category, or timing FAILS — never a vacuous pass', () => {
  for (const key of ['categories:nope', 'not-an-audit', 'user-timings:renamed-mark']) {
    const failures = evaluateAssertions({ [key]: ['error', { minScore: 1 }] }, 'u', [lhr()]);
    assert.equal(failures.length, 1, key);
    assert.match(failures[0].error, /missing/, key);
  }
});

test('a mistyped aggregationMethod or a bound-less assertion FAILS, never passes', () => {
  const typo = {
    'categories:performance': ['error', { minScore: 0.5, aggregationMethod: 'typo' }],
  };
  assert.equal(evaluateAssertions(typo, 'u', [lhr()]).length, 1);
  const unbounded = { 'largest-contentful-paint': ['error', { aggregationMethod: 'median' }] };
  assert.equal(evaluateAssertions(unbounded, 'u', [lhr()]).length, 1);
});

function freePort() {
  return new Promise((resolve) => {
    const probe = net.createServer().listen(0, 'localhost', () => {
      const { port } = probe.address();
      probe.close(() => resolve(port));
    });
  });
}

async function answersWithin(url, ms) {
  for (const deadline = Date.now() + ms; Date.now() < deadline; await sleep(100)) {
    try {
      await fetch(url);
    } catch {
      return false;
    }
  }
  return true;
}

test('the preview stays a foreground child under an agent, so stop() frees its port', async () => {
  // CI is no agent: force astro's detection (am-i-vibing reads CLAUDECODE) so
  // this takes the path that daemonizes.
  process.env.CLAUDECODE = '1';
  const root = mkdtempSync(join(tmpdir(), 'lh-preview-'));
  mkdirSync(join(root, 'dist'));
  writeFileSync(join(root, 'dist', 'index.html'), 'ok');
  const { command, ready } = previewServer({
    collect: {
      startServerCommand: 'npm run preview',
      url: [`http://localhost:${await freePort()}/`],
    },
  });
  let server;
  let outlived;
  try {
    server = await startServer(`${command} --root ${root}`, ready, 20_000);
    assert.equal(await (await fetch(ready)).text(), 'ok');
    stop(server);
    outlived = await answersWithin(ready, 5_000);
  } finally {
    if (server) stop(server);
    // A daemon reads as a launcher that exited early, or one that outlives
    // stop(); its pid is in its lock file, so a red run leaves nothing listening.
    const lock = join(root, '.astro', 'preview.json');
    if (existsSync(lock)) process.kill(JSON.parse(readFileSync(lock, 'utf8')).pid);
  }
  assert.equal(outlived, false, 'the preview outlived stop(): astro daemonized it');
});

test('a squatted audit port fails the start instead of auditing the squatter', async () => {
  const squatter = net.createServer().listen(0, 'localhost');
  await new Promise((resolve) => squatter.once('listening', resolve));
  const root = mkdtempSync(join(tmpdir(), 'lh-preview-'));
  mkdirSync(join(root, 'dist'));
  const { command, ready } = previewServer({
    collect: {
      startServerCommand: 'npm run preview',
      url: [`http://localhost:${squatter.address().port}/`],
    },
  });
  const start = startServer(`${command} --root ${root}`, ready, 5_000);
  try {
    // vite moves the preview to the next free port and says so; only the ready
    // signal keeps the runner from taking that for the audited server.
    await assert.rejects(start, (e) => /not ready[\s\S]*is in use/.test(e.message));
  } finally {
    squatter.close();
    start.then(stop, () => {});
  }
});
