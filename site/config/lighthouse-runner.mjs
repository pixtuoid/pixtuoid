#!/usr/bin/env node
// The official programmatic-Lighthouse pattern (lighthouse docs/readme.md)
// with the multi-run + median-aggregation CI guidance from
// docs/variability.md, reading the SAME lighthouserc.json the retired
// @lhci/cli wrapper did — LHCI's last publish (2025-06) pinned lighthouse 12,
// which dragged the @puppeteer/browsers and chrome-launcher overrides.
// Divergences from LHCI: every assertion fails CI whatever its level, and one
// without an explicit aggregationMethod aggregates by MEDIAN (LHCI defaulted
// to optimistic; median is stricter-or-equal).

import { spawn } from 'node:child_process';
import { mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import process from 'node:process';
import { pathToFileURL } from 'node:url';

export function median(values) {
  const sorted = [...values].sort((a, b) => a - b);
  const mid = Math.floor(sorted.length / 2);
  return sorted.length % 2 ? sorted[mid] : (sorted[mid - 1] + sorted[mid]) / 2;
}

// `kind` decides which direction is cautious: for scores, pessimistic = the
// WORST (min) run; for numeric budgets (lower is better), pessimistic = max.
export function aggregate(values, method, kind) {
  const worst = kind === 'score' ? Math.min : Math.max;
  const best = kind === 'score' ? Math.max : Math.min;
  switch (method) {
    case 'pessimistic':
      return worst(...values);
    case 'optimistic':
      return best(...values);
    case 'median':
      return median(values);
  }
}

function extract(lhr, key, kind) {
  if (key.startsWith('categories:')) {
    return lhr.categories?.[key.slice('categories:'.length)]?.score;
  }
  if (key.startsWith('user-timings:')) {
    const name = key.slice('user-timings:'.length);
    const item = lhr.audits?.['user-timings']?.details?.items?.find((i) => i.name === name);
    // A measure carries duration; a mark only startTime.
    return item?.duration ?? item?.startTime;
  }
  const audit = lhr.audits?.[key];
  return kind === 'score' ? audit?.score : audit?.numericValue;
}

/// One failure record per broken assertion; a value MISSING from any run is a
/// failure too — a renamed audit or dropped mark must never pass vacuously.
export function evaluateAssertions(assertions, url, lhrs) {
  const failures = [];
  for (const [key, [, opts]] of Object.entries(assertions)) {
    const kind = opts.minScore != null ? 'score' : 'numeric';
    const values = lhrs.map((lhr) => extract(lhr, key, kind));
    if (values.some((v) => typeof v !== 'number')) {
      failures.push({
        url,
        key,
        error: `value missing in ${values.filter((v) => typeof v !== 'number').length}/${lhrs.length} runs`,
      });
      continue;
    }
    const method = opts.aggregationMethod ?? 'median';
    const value = aggregate(values, method, kind);
    const pass = kind === 'score' ? value >= opts.minScore : value <= opts.maxNumericValue;
    if (!pass) {
      const limit =
        kind === 'score' ? `minScore ${opts.minScore}` : `maxNumericValue ${opts.maxNumericValue}`;
      failures.push({
        url,
        key,
        error: `${method} ${round(value)} violates ${limit} (runs: ${values.map(round).join(', ')})`,
      });
    }
  }
  return failures;
}

function round(n) {
  return Math.round(n * 1000) / 1000;
}

export function startServer(command, ready, timeoutMs) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, { shell: true, detached: process.platform !== 'win32' });
    let output = '';
    const timer = setTimeout(() => {
      stop(child);
      reject(new Error(`server not ready within ${timeoutMs}ms; output:\n${output}`));
    }, timeoutMs);
    const watch = (chunk) => {
      output += chunk;
      if (output.includes(ready)) {
        clearTimeout(timer);
        resolve(child);
      }
    };
    child.stdout.on('data', watch);
    child.stderr.on('data', watch);
    child.on('exit', (code) => {
      clearTimeout(timer);
      reject(new Error(`server exited early (code ${code}); output:\n${output}`));
    });
  });
}

export function previewServer(cfg) {
  const { origin, port } = new URL(cfg.collect.url[0]);
  return {
    // Under an agent, astro 7.4 starts `astro preview` as a detached daemon
    // (node_modules/astro/dist/cli/preview/index.js:37-45) that `stop` cannot
    // reach and the next run silently reuses; `--ignore-lock` is upstream's
    // one-off foreground server.
    command: `${cfg.collect.startServerCommand} -- --port ${port} --ignore-lock`,
    // The audited origin, not any localhost URL: on a squatted port vite moves
    // the preview to the next one, and the audit must time out, not measure the
    // squatter.
    ready: `${origin}/`,
  };
}

// astro preview is a process TREE under a shell — kill the group, not the
// shell, or CI keeps an orphan listener.
export function stop(child) {
  try {
    if (process.platform === 'win32') child.kill();
    else process.kill(-child.pid, 'SIGTERM');
  } catch {
    // already gone
  }
}

async function main() {
  const { default: lighthouse } = await import('lighthouse');
  const chromeLauncher = await import('chrome-launcher');
  const cfg = JSON.parse(readFileSync(new URL('../lighthouserc.json', import.meta.url), 'utf8')).ci;
  const outDir = new URL(`../${cfg.upload.outputDir}/`, import.meta.url);
  rmSync(outDir, { recursive: true, force: true });
  mkdirSync(outDir, { recursive: true });

  const preview = previewServer(cfg);
  const server = await startServer(
    preview.command,
    preview.ready,
    cfg.collect.startServerReadyTimeout
  );
  // Launch inside the try: a ChromeNotInstalledError must still tear the
  // detached preview down, or it squats its port for the next local run.
  let chrome;
  const failures = [];
  try {
    chrome = await chromeLauncher.launch({ chromeFlags: ['--headless'] });
    for (const url of cfg.collect.url) {
      const lhrs = [];
      const slug = new URL(url).pathname.replaceAll('/', '_') || '_';
      for (let run = 0; run < cfg.collect.numberOfRuns; run += 1) {
        // Serial on purpose: concurrent runs contend and skew metrics
        // (docs/variability.md).
        const result = await lighthouse(url, {
          port: chrome.port,
          output: 'json',
          logLevel: 'error',
        });
        lhrs.push(result.lhr);
        writeFileSync(new URL(`lhr-${slug}-${run}.json`, outDir), result.report);
      }
      failures.push(...evaluateAssertions(cfg.assert.assertions, url, lhrs));
      console.log(`${url}: ${cfg.collect.numberOfRuns} runs collected`);
    }
  } finally {
    chrome?.kill();
    stop(server);
  }

  if (failures.length > 0) {
    for (const f of failures) console.error(`FAIL ${f.url} ${f.key}: ${f.error}`);
    process.exit(1);
  }
  const n = Object.keys(cfg.assert.assertions).length;
  console.log(`lighthouse: ${n} assertions passed on ${cfg.collect.url.length} urls`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  await main();
}
