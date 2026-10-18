# site — agent guide

The **landing page**: a self-contained **Astro** static site (NOT Rust) →
GitHub Pages. A *consumer* of the Rust workspace's outputs — version, rendered
docs, demo media all flow in from outside `site/`. Parent guide: the workspace
[`../AGENTS.md`](../AGENTS.md). Human detail: [`README.md`](README.md).

> **You are in the Astro consumer, not the Rust producer.** Rust house rules
> (`cargo`/`clippy`, `just preflight`, the crate CI gates) do not apply to a
> site-only change. The gates here are `just site-check` (+ `just site-fmt`)
> and `just site-e2e`; the pre-push hook still runs `just preflight`.

## Cross-boundary build inputs (the coupling that bites)

`astro build` reads **these files from OUTSIDE `site/`** (workspace
`Cargo.toml` + the `docs/` pages below); a rename/move of any FAILS the
build:

- workspace `Cargo.toml` → displayed-version FALLBACK only; the primary source
  is the latest release tag (`config/released-version.mjs`, unit-tested — main
  runs ahead of the released version mid-cycle). Both workflows checkout with
  `fetch-depth: 0`. `pages.yml` deploys on push to `main`, not on tag push, so
  a fresh tag shows after the next `main` commit or a `workflow_dispatch`.
- `docs/{CONFIGURATION, ARCHITECTURE, CONTRIBUTING}.md`
  → rendered routes via glob loaders in `src/content.config.ts`. **Adding,
  renaming, or REMOVING a rendered doc is a multi-point edit**: glob
  pattern, `src/pages/*.astro`, the `DOCS` entry in `consts.ts` (`Nav.astro`
  and `Docs.astro`'s sidebar/pager derive from it; `assert-docs-rendered` is
  generic by design — it globs every rendered `article.prose` — and needs no
  edit), `lighthouserc.json`, and the smoke viewport table.

`public/wasm/` is gitignored `just gen-wasm` output; without it the office
silently stays on its poster. `public/demos/` is gitignored too:
`just gen-media --only site` renders it locally (before a build or dev server,
which `astro.config.mjs`'s `demoAssets()` fails on a missing demo, and before
`test:demos`), and CI renders it per build.

**The architecture diagram renders at build**, in process
(`config/rehype-beautiful-mermaid.mjs` — no browser); Playwright's Chromium is
installed only for `site.yml`'s e2e step. The empty-render class is
gated by `config/assert-docs-rendered.mjs` (`check:docs`), whose header owns
the mechanism; `pages.yml`'s pins carry their own.

**Docs shell**: the doc routes (the `DOCS` manifest in `consts.ts` is the
roster) mount the Statusline doc variant (no PR feed fetch); `Docs.astro`'s
sidebar reads the one `FLOORS` manifest.
Blockquotes promote to terminal callouts (`config/rehype-callouts.mjs`,
unit-tested, registered AFTER `rehypeRepoLinks`). Adding a `.prose` colour rule
needs a twin under `.callout__body` — the specificity arithmetic and its
theme-prefixed tie are on the rule itself in `Docs.astro`, and the smoke
suite's callout AA sweep grades it.

## Single-sourced content

Every generated artifact, manifest seam, and rendered-copy sharp edge:
[`SINGLE-SOURCED.md`](SINGLE-SOURCED.md) — read it before touching this area.

## CSP (hash-based, two coordinated halves in astro.config.mjs)

`cspInlineHashes()` in `astro.config.mjs` owns the WHY; `config/csp-hashes.mjs`
owns the parse. The rules a page author needs: an `is:inline` script needs NO
manual CSP step; a hand-written `public/*.js` loaded by URL rides
`script-src 'self'`; an inline `<style>` or `style` attribute needs none
either — each is hashed, an attribute's under `'unsafe-hashes'`, but one set
at runtime (`setAttribute('style', …)`, `style.cssText`, markup through
`innerHTML`) is blocked: set `el.style.<property>` instead. `astro dev` serves
NO CSP; regressions surface in `just site-e2e`'s console watchdog.

## Dev server (agent-driving)

`just site-dev-bg` daemonizes (`astro dev --background`, polls the dev-only
`/_astro/status` endpoint); `just site-dev-stop` frees the port. Two edges:
the status endpoint is dev-server only (astro's `core/create-vite.js`, at
`package.json`'s version, registers it under `command === "dev"`; `astro
preview` daemonizes too but has nothing to poll), and dev/preview share port
4321 — stop the daemon before `just site-e2e` (its webServer fails loud on a
squatted port).
`npm run lighthouse` needs 4321 AND `collect.previewPort` free: its preview
moves there so the gzip proxy can take the audited port.

## Gates

`just site-check` = `npm run verify` (format:check → lint → check → knip →
test:unit → test:demos → build → check:docs → audit). `just site-e2e` = Playwright vs the
PRODUCTION build — the runtime-contract tier (`__pixLights`/`pix:onair`/
`data-lit` seams, scrollspy keys, docs-nav variant, reduced-motion, console
watchdog) that tsc/knip/build are blind to. CI: `site.yml` / `pages.yml`.

- **Lighthouse** (`npm run lighthouse`, in-repo runner
  `config/lighthouse-runner.mjs`, lighthouse's programmatic API, median of
  `lighthouserc.json`'s `numberOfRuns` serial runs; runner-semantics pinned by
  its test — a renamed audit FAILS instead of passing vacuously).
  **A category score is a budget, not a
  contract**: `color-contrast` is a small weight in the a11y category, so a total
  contrast failure still clears the category budget — anything that must never regress gets
  its own per-audit assertion with `aggregationMethod: pessimistic` (median
  greens a binary audit that failed one run). Collect URLs pin `?theme=day`
  (otherwise the wall clock picks the palette — half of CI runs would audit
  each). **The theme matrix is `smoke.spec.ts`** (day/night/dracula), not a
  doubled collect list. The audit runs THROUGH `startPagesLikeProxy`, which
  gzips the wasm as GitHub Pages does and `astro preview` doesn't: at Lantern's
  `mobileSlow4G` throughput, `interactive`/LCP are BYTE budgets. They and
  `speed-index` were sized once (#1124): wasm padded to `just
  gen-wasm-check`'s cap, plus run spread.
- **Fonts**: Fontsource WOFF2 with `font-display: optional` + preloads — do
  not switch to the default `swap` (Ubuntu cold visits reflow past the CLS
  budget; `font-layout.spec.ts` reproduces it deliberately).
- **npm**: `audit` runs LAST in `verify` (a live advisory would short-circuit
  the checks below it — #847/#849); CI runs it when a PR changes
  `package.json` or `package-lock.json`, and daily in `audit.yml`.
  npm pinned (`packageManager` + `engine-strict`); install scripts
  fail-closed (`strict-allow-scripts`, exact-version `allowScripts`, explicit
  `fsevents` denial) — review with `npm install-scripts ls`.
- **Nothing gates a STALE `overrides` entry**: to retire one, fresh-resolve
  `--package-lock-only` in a scratch dir with and without it and diff the
  `packages` maps — `npm audit` cannot clear an entry (the retired
  `chrome-launcher` pin guarded a chain no advisory covered).
