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
import http from 'node:http';
import process from 'node:process';
import { pipeline } from 'node:stream';
import { pathToFileURL } from 'node:url';
import { createGzip } from 'node:zlib';

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

function startServer(command, readyPattern, timeoutMs) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, { shell: true, detached: process.platform !== 'win32' });
    const pattern = new RegExp(readyPattern);
    let output = '';
    const timer = setTimeout(() => {
      stop(child);
      reject(new Error(`server not ready within ${timeoutMs}ms; output:\n${output}`));
    }, timeoutMs);
    const watch = (chunk) => {
      output += chunk;
      if (pattern.test(output)) {
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

// `astro preview` (vite) compresses text MIME types only, so it serves the wasm —
// the home page's largest asset — RAW, while GitHub Pages serves it gzip (and
// answers a br-only client with the raw bytes; fetched 2026-09-20). Under
// simulated throttling `interactive`/LCP are byte budgets, so without this the
// audit prices the wasm at its raw size rather than what a visitor downloads.
// Everything else passes through untouched: the proxy closes that ONE gap, it is
// not a server.
export function startPagesLikeProxy({ upstreamPort, port }) {
  const server = http.createServer((req, res) => {
    const upstream = http.request(
      {
        host: 'localhost',
        port: upstreamPort,
        method: req.method,
        path: req.url,
        headers: req.headers,
      },
      (up) => {
        const headers = { ...up.headers };
        const gzip =
          /^application\/wasm\b/.test(headers['content-type'] ?? '') &&
          !headers['content-encoding'] &&
          /\bgzip\b/.test(req.headers['accept-encoding'] ?? '');
        // `pipeline`, not `.pipe()`: a source that dies mid-body must END the
        // response, and an unhandled stream error would skip main()'s `finally`.
        if (!gzip) {
          res.writeHead(up.statusCode, headers);
          pipeline(up, res, () => {});
          return;
        }
        delete headers['content-length'];
        headers['content-encoding'] = 'gzip';
        headers.vary = 'Accept-Encoding';
        res.writeHead(up.statusCode, headers);
        pipeline(up, createGzip({ level: 9 }), res, () => {});
      }
    );
    // Chrome aborts requests mid-flight; an answer arriving after would reach
    // `pipeline` with `res` closed, which THROWS rather than calling back.
    res.on('close', () => {
      if (!res.writableFinished) upstream.destroy();
    });
    upstream.on('error', () => {
      if (res.headersSent) {
        res.destroy();
        return;
      }
      res.writeHead(502);
      res.end();
    });
    req.pipe(upstream);
  });
  return new Promise((resolve, reject) => {
    server.once('error', reject);
    // `localhost`, like the preview it fronts: Chrome may try ::1 first.
    server.listen(port, 'localhost', () => resolve(server));
  });
}

// The audited URLs' ONE port (the proxy's) and the preview's own, which the
// runner passes to the start command — so the pair lives in one place.
export function resolvePorts(cfg) {
  const { previewPort } = cfg.collect;
  const auditPort = Number(new URL(cfg.collect.url[0]).port);
  return { auditPort, previewPort };
}

// astro preview is a process TREE under a shell — kill the group, not the
// shell, or CI keeps an orphan listener.
function stop(child) {
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

  const { auditPort, previewPort } = resolvePorts(cfg);
  const server = await startServer(
    `${cfg.collect.startServerCommand} -- --port ${previewPort}`,
    cfg.collect.startServerReadyPattern,
    cfg.collect.startServerReadyTimeout
  );
  // Launch inside the try: a ChromeNotInstalledError must still tear the
  // detached preview down, or it squats its port for the next local run.
  let chrome;
  let proxy;
  const failures = [];
  try {
    proxy = await startPagesLikeProxy({ upstreamPort: previewPort, port: auditPort });
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
    proxy?.close();
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
