# pixtuoid — website

The landing page for [pixtuoid](https://github.com/pixtuoid/pixtuoid), an
[Astro](https://astro.build) site in `site/`. It deploys to GitHub Pages at
**https://pixtuoid.dev/**; the old project page,
https://pixtuoid.github.io/pixtuoid/, redirects there.

Two more guides sit beside this one: [`AGENTS.md`](AGENTS.md) for the files
the build reads from outside `site/` and the CSP, and
[`SINGLE-SOURCED.md`](SINGLE-SOURCED.md) for the generated content and where
each piece comes from.

## Develop

```sh
npm install        # or, from the repo root: just site-setup
npm run dev        # http://localhost:4321/, or: just site-dev
just site-dev-bg   # the dev server in the background, for an agent; just site-dev-stop ends it
```

`just site-dev-bg` waits until the dev server answers on its dev-only
`/_astro/status` endpoint. Stop it before `just site-e2e`: the dev server and
the e2e's preview server share port 4321.

The live office runs from `public/wasm/`, which `just gen-wasm` builds (it
needs Rust) and git ignores. Without it, `dev` and `verify` still work, and
the office shows its poster.

## Checks

```sh
npm run verify     # format:check → lint → check → knip → test:unit → test:demos → build → check:docs → audit
npm run lighthouse # Lighthouse CI's budgets
```

From the repo root, `just site-check` runs `verify`, `just site-fmt` formats,
and `just site-e2e` runs the Playwright smoke suite. Use `just site-e2e`, not
`npm run e2e`: it builds the wasm and renders the demos first.

- **`audit` runs last** on purpose: it reads advisories live, so a stranger's
  publish would otherwise stop every check after it.
- **npm 12 is required** (`packageManager`, with `engine-strict`).
  Dependencies' install scripts don't run unless `allowScripts` lists them
  (`.npmrc`); `npm install-scripts ls` shows what a change would run.

In CI, `.github/workflows/site.yml` runs `verify`'s checks, the e2e and
Lighthouse. It runs `audit` only when a PR changes `package.json` or
`package-lock.json`; `audit.yml` runs it daily. `.github/workflows/pages.yml`
deploys.

## Design

- **Type.** "Cozy Terminal": Jersey 10 for pixel display, Monaspace Neon for
  UI and code, Lora for body text, and Fusion Pixel 8px for the office's
  captions. ASCII dividers, a blinking cursor and CRT scanlines.
- **Palette.** A warm "Coworking" palette, cream with Claude coral. Day is
  cream and night is after hours. Until the visitor picks one, the site
  follows their local time, night from 19:00 to 07:00, and a system that
  prefers dark keeps night at noon too. `dracula` is a hidden easter egg:
  type it, or open `?theme=dracula`.
- **The building.** The home page is one continuous shot of the real office.
  `OfficeBackdrop.astro` runs the `pixtuoid-web` wasm engine on a fixed
  full-viewport canvas, showing its poster first. Under reduced motion,
  without script, or if the engine fails, the poster stays.
  - Scrolling is the light switch: the `#dimmer` sheet darkens toward a
    statement and lifts in the gaps that show the office.
  - Each section is a floor, from the top floor down to 1F. The fixed chrome
    is `Statusline.astro` (the floor readout, scrollspy, the PR feed and the
    install chip) and two switches, `#office-pause` and ♩ `#office-audio`.
  - The floor digits ride to their floors from anywhere on the page, except
    where something claims them: a typing surface, a modifier, the boot
    splash, or a focused `[data-keys-scope]` region. The appearance key
    retints only while focus is on the page, a floor or the statusline, so
    dictation into a control can't fire it (WCAG 2.1.4). The statusline's
    popover holds the saved off-switch for the keys.
  - The 404 page is the office at 3 a.m.
- **What a new section wires.**
  - `data-lit`, for the dimmer, and a row in `src/floors.json`, from which
    the statusline, the wings' sidebar and the section ids derive.
  - The backdrop publishes `window.__pixLights`, `pix:onair` with
    `.is-live`, and `pix:paused`. Every auto-motion longer than five seconds
    listens to `pix:paused`, so one control governs the page's motion.
  - `Install.astro` fires `pix:install-copy`; the backdrop walks a coworker
    in (capped), then `pix:hired` flashes the statusline's receipt.
  - `Base.astro`'s head defines `window.__pixNight()`, `__pixTheme` and
    `__pixKeys`. Never derive the night window, the theme's background map
    or the typing guard again inline.
  - At boot, `pix:revealed` (the splash lifting) releases the office's floor
    roll, and `__pixEngineReady` lets the splash lift straight into it.
- **Effects.** The CRT power-on, the hero's pixel dust and the dimmer, all
  safe under `prefers-reduced-motion`. Pause freezes the office's frame and
  stops all ambient motion through `pix:paused`. It still shows on the
  no-wasm poster, where the ticker, the dust and the clips still run.
- **Wings.** `layouts/Wing.astro` gives the doc routes and the blog one
  shell: an elevator-panel sidebar, a mini table of contents and a pager,
  each from its own roster. A blockquote becomes a terminal-window callout.

## Demo art

`public/demos/*` is generated and git-ignored, never placed by hand. From the
repo root, `just site-demos` renders it when it is missing or its manifests
change, and `just gen-media --only site` renders it again after a change to
the look. Both run `scripts/gen-media.py` over `scripts/media.json`, through
the real TuiRenderer; a clip is encoded to `.mp4` and `.webm` with a poster.

Pixel art lives in `public/`, not `src/assets/`, whose optimizer would resize
and blur it.

## The showcase (Studio Wall)

The `Showcase` and `ChannelStage` components draw what **`src/showcase.json`**
lists, beside `src/themes.json`, `src/weather.json`, `src/features.json` and
`src/install.json`. A channel is one of three kinds:

- **`clip`**: an mp4, a webm and a poster from `just gen-media --only site`;
- **`variant-set`**: a grid of screenshots, from `variantsRef` or `variants`;
- **`soon`**: a placeholder.

`astro.config.mjs` checks at build time that one channel is the default live
one, that ids are unique, that each asset exists, and that features and
showcase channels pair one to one.

- **A new demo channel** is one `showcase.json` entry and its assets from
  `just gen-media --only site`. A `clip` channel also adds its
  `"kind": "clip"` job, named by the channel's `asset`, to
  `scripts/media.json`. No component changes.
- **A new theme** is one row in `src/themes.json` and
  `just gen-media --only site`. The chips, the counts, the retint and the
  renders all pick it up.

## Domain and deploy

The site is served at **pixtuoid.dev** (`base: '/'`; `site` is the
workspace's `homepage` in `Cargo.toml`).

- The domain is set in the repository's **Settings → Pages**. A deploy from
  Actions needs no `CNAME` file.
- DNS: the apex's A and AAAA records point at GitHub Pages, and `www` is a
  CNAME to `pixtuoid.github.io`.
- `robots.txt` and the sitemap derive from the config.
- The first deploy needs **Source: GitHub Actions** in Settings → Pages;
  after that, every push to `main` redeploys.
