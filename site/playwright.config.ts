import { defineConfig, devices } from '@playwright/test';

// Runs against the PRODUCTION build via `astro preview` — the dev server's Vite
// cache has twice masqueraded as a site bug in this repo. `dist/` must exist:
// `just site-e2e` builds first, and in CI the build step precedes the e2e step.
export default defineConfig({
  testDir: './tests/e2e',
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI ? 'github' : 'list',
  timeout: 45_000,
  use: {
    baseURL: 'http://localhost:4321/',
    trace: 'on-first-retry',
  },
  projects: [
    // Chromium only: it's the browser CI already installs, and the suite gates
    // cross-component CONTRACTS, not engine differences.
    { name: 'chromium', use: { ...devices['Desktop Chrome'] } },
  ],
  webServer: {
    // The astro bin DIRECTLY — an `npm run` wrapper leaves an orphaned astro
    // child holding the port when Playwright kills the tree. reuse stays false
    // for the same reason: a squatted port must fail loud, never quietly test
    // old bytes. Readiness stays Playwright's URL poll because Astro's
    // /_astro/status health endpoint is DEV-SERVER-ONLY; `astro preview` 404s it.
    // Under an agent astro daemonizes `preview`, which Playwright reads as an early
    // exit; `--ignore-lock` keeps it a foreground child (withastro/astro#17903,
    // node_modules/astro/dist/cli/preview/index.js:42-45).
    command: 'node node_modules/astro/bin/astro.mjs preview --port 4321 --ignore-lock',
    url: 'http://localhost:4321/',
    reuseExistingServer: false,
    timeout: 30_000,
  },
});
