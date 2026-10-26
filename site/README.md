# pixtuoid — website

The marketing landing page for [pixtuoid](https://github.com/pixtuoid/pixtuoid),
built with [Astro](https://astro.build). Deploys to GitHub Pages at
**https://pixtuoid.dev/** (the old project page
https://pixtuoid.github.io/pixtuoid/ redirects there).

A Node project living in `site/`. The live office's `public/wasm/` is
gitignored output of `just gen-wasm` (needs Rust); `dev` and `verify` work
without it on the poster fallback. CI (`.github/workflows/site.yml`) runs
`npm run verify`'s checks, plus e2e and Lighthouse, with `audit` only when a
PR changes `package.json` or `package-lock.json` (and daily in `audit.yml`);
deploys run via `.github/workflows/pages.yml`. Agent notes,
build-input coupling, and CSP details: [`AGENTS.md`](AGENTS.md). Generated
content and its sources: [`SINGLE-SOURCED.md`](SINGLE-SOURCED.md).

## Develop

```sh
npm install        # or: just site-setup   (from the repo root)
npm run dev        # http://localhost:4321/   ·  just site-dev
just site-dev-bg   # background daemon (agent-friendly) · just site-dev-stop
```

The background daemon polls the dev-only `/_astro/status` endpoint until
ready; stop it before `just site-e2e` (dev and preview share port 4321).

## Quality gates

```sh
npm run verify     # format:check → lint → check → knip → test:unit → test:demos → build → check:docs → audit
npm run lighthouse # three-run a11y / SEO / performance budgets (in-repo runner)
```

From the repo root: `just site-check`, `just site-fmt`, and `just site-e2e` for
the Playwright smoke suite (not bare `npm run e2e`: it builds the wasm first).
`audit` runs LAST in `verify` on purpose (it resolves advisories live —
someone else's publish would short-circuit every check below it). npm 12 is required (`packageManager` +
`engine-strict`); dependency install scripts are fail-closed via `.npmrc` +
`allowScripts` — review changes with `npm install-scripts ls`.

## Design

- **Layout/type** — "Cozy Terminal": Jersey 10 (pixel display) · Monaspace
  Neon (UI/code) · Lora (body) · Fusion Pixel 8px (the office's captions);
  ASCII dividers, blinking cursor, CRT scanlines.
- **Palette** — warm "Coworking" (cream + Claude coral). Day = cream, night =
  after-hours; until the visitor picks, the site follows their wall clock
  (19:00–07:00 → night; `prefers-color-scheme` keeps night at noon for
  dark-preference systems). `dracula` is a hidden easter egg (type it, or
  `?theme=dracula`).
- **The building (OPEN FLOOR)** — one continuous camera hold on the REAL
  office: `OfficeBackdrop.astro` runs the `pixtuoid-web` wasm engine as a
  fixed full-viewport canvas (poster-first; reduced-motion / no-JS / failure
  stays on the still). Scrolling is the light switch — the `#dimmer` sheet
  darkens toward statements and releases in office gaps. Each section is a
  floor (6F → 1F); fixed chrome is `Statusline.astro` (floor readout +
  scrollspy + PR feed + install chip), `ElevatorShaft.astro` (click-to-ride
  rail), and the `#office-pause` switch. Digits `1–6` ride between floors
  document-global (typing surfaces, modifiers, the boot splash, and focused
  `[data-keys-scope]` regions claim them locally); `t` retints, focus-gated
  (WCAG 2.1.4); the statusline popover carries the persisted keys
  off-switch. 404 is the office at 3 a.m.
- **Cross-component seams** (what a new section wires): `data-lit` (dimmer)
  and a `FLOORS` row in `consts.ts` (statusline, shaft, section ids all
  derive from it). The backdrop publishes `window.__pixLights`, `pix:onair` /
  `.is-live`, and `pix:paused` (every >5s auto-motion listens — one control
  governs page motion). `Install.astro` fires `pix:install-copy` → the
  backdrop walks a coworker in (capped) → `pix:hired` flashes the statusline
  receipt. `Base.astro`'s head defines `window.__pixNight()` / `__pixTheme` /
  `__pixKeys` — never re-derive the night window, theme BG map, or typing
  guard inline. Boot handshake: `pix:revealed` (splash lifts) releases the
  office's floor-roll; `__pixEngineReady` lets the splash lift straight into
  it.
- **FX** — CRT power-on, hero pixel-dust, the dimmer; all
  `prefers-reduced-motion`-safe. Pause freezes the office frame AND stops all
  ambient motion via `pix:paused`; it stays visible on a no-wasm poster
  (ticker/dust/clips still run).
- **Docs shell** — `layouts/Docs.astro` gives the doc routes a shared
  elevator-panel sidebar + mini-TOC + pager off the one `DOCS` manifest;
  blockquotes promote to terminal-window callouts.

## Demo art

`public/demos/*` is **generated** and gitignored, never hand-placed:
`just site-demos` renders it when absent or its manifests change (`just gen-media --only site` re-renders a look change) from
the repo root (`scripts/gen-media.py` + `scripts/media.json`, rendering
through the real TuiRenderer; clips re-encode to `.mp4` + `.webm` + poster).
Pixel art lives in `public/` on purpose — Astro's `src/assets/` optimizer
would resize/blur it.

## Showcase (Studio Wall)

Manifest-driven (`Showcase` → `ChannelStage`), defined in
**`src/showcase.json`** alongside `src/themes.json`, `src/weather.json`,
`src/features.json`, `src/install.json`. Channel kinds: **`clip`** (mp4 +
webm + poster from `just gen-media --only site`), **`variant-set`** (screenshot grid via
`variantsRef`/`variants`), **`soon`** (placeholder). `astro.config.mjs`
enforces the invariants at build time (one default live channel, unique ids,
assets present, features↔showcase bijection).

**Adding a demo channel**: one `showcase.json` entry + `just gen-media --only site`
assets (a `clip` channel also adds its render + `encode_clip` block in
`scripts/gen-media.py`). No component edits.

**Adding a theme**: one row in `src/themes.json` + `just gen-media --only site`. Chips,
counts, retint, and renders all pick it up automatically.

## Custom domain & deploy

The site lives on **pixtuoid.dev** (`base: '/'`, `site` in
`astro.config.mjs`); the domain is configured in **Settings → Pages** (Actions
deploys need no `CNAME` file), apex A/AAAA at GitHub Pages, `www` CNAME'd to
`pixtuoid.github.io`. `robots.txt` + sitemap derive from the config. First
deploy: set **Source: GitHub Actions**; after that every push to `main`
touching `site/**` redeploys.
