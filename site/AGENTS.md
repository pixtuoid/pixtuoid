# site — agent guide

The **landing page**: a self-contained Astro static site, not Rust, deployed
to GitHub Pages. It consumes the Rust workspace's outputs: the version, the
rendered docs and the demo media all come from outside `site/`.
Cross-cutting rules: the root [`AGENTS.md`](../AGENTS.md). Read these two
before changing the site:

- [`README.md`](README.md): developing, the dev server, the demo art and the
  wasm, and the checks with their npm policy.
- [`SINGLE-SOURCED.md`](SINGLE-SOURCED.md): every generated artifact, manifest
  seam and rendered-copy sharp edge.

## Gates

- The gates are `just site-check` and `just site-e2e`, with `just site-fmt` to
  format (README's Checks says what each runs). The pre-push hook still runs
  `just preflight` on a site-only change.
- `just site-e2e` runs Playwright against the production build: the
  runtime-contract tier, which sees what tsc, knip and the build are blind
  to (the `__pixLights`, `pix:onair` and `data-lit` seams, the scrollspy keys,
  the docs-nav variant, reduced motion and the console watchdog).
- Lighthouse (`npm run lighthouse`, which serves `dist/` itself) audits the
  home page and every route `config/blog.mjs`'s `readingRoutes` lists: each
  rendered doc, the blog's lobby and every post.
  `config/lighthouse-budget.test.mjs` holds the assertion policy.
  - The theme matrix (day, night, dracula) is `smoke.spec.ts`'s; don't double
    Lighthouse's collect list per theme.
  - The office's worker fetches its wasm, which Lighthouse's simulation drops,
    so `just gen-wasm-check` alone gates the wasm (#1124).
- Fonts are WOFF2 with `font-display: optional`, loaded before use: a preload,
  or for the captions' face OfficeBackdrop's boot. Never `swap`: Ubuntu cold
  visits reflow past the CLS budget, which `font-layout.spec.ts` reproduces.
- Nothing gates a stale `overrides` entry, and `npm audit` cannot clear one
  (the retired `chrome-launcher` pin guarded a chain no advisory covered). To
  retire an entry, fresh-resolve `--package-lock-only` in a scratch dir with
  and without it and diff the `packages` maps.

## Build inputs outside `site/`

`astro build` reads these files from outside `site/`; renaming or moving one
fails the build:

- The workspace `Cargo.toml`: the origin (`homepage`), the default title
  (`description`) and the displayed version's fallback.
  - The displayed version is the latest release tag
    (`config/released-version.mjs`, unit-tested), since main runs ahead of it
    mid-cycle. A CI job that builds the site checks out with
    `fetch-depth: 0`; a shallow checkout silently falls back to `Cargo.toml`.
  - `pages.yml` deploys on a push to `main`, not on a tag push, so a fresh tag
    shows after the next `main` commit or a `workflow_dispatch`.
- The `docs/*.md` files `src/docs.json` names, each a rendered route. Adding,
  renaming or removing a rendered doc edits each of these:
  - its `docs.json` entry, from which the collection, the route, `Nav.astro`,
    `Wing.astro`'s sidebar and pager, the in-site doc links and Lighthouse's
    URLs derive (`assert-docs-rendered` globs every rendered `article.prose`
    and needs no edit);
  - `site.yml`'s `scope` paths;
  - the e2e rows that name a route: smoke's pan and `PLATE_SURFACES` tables,
    and `docs-lobby.spec.ts`.
- The `docs/` images the posts show.

## Rendered docs and the blog

- Every Mermaid diagram in a rendered doc renders at build, as a framed
  figure, in process with no browser (`config/rehype-beautiful-mermaid.mjs`).
  Playwright's Chromium is installed only for `site.yml`'s e2e step.
- `npm run build` passes `--force` because Astro's content store outlives an
  edit to a rehype plugin: its `content/content-layer.js`, at `package.json`'s
  version, clears the store only on a config or version change.
- `config/assert-docs-rendered.mjs` (`check:docs`) gates the empty-render
  class; its header owns the mechanism, and `pages.yml`'s pins carry their
  own.
- The doc routes (`src/docs.json` is the roster) and the blog share the Wing
  shell: they mount the Statusline without its index-only parts (no PR feed
  fetch), and `Wing.astro`'s sidebar reads the one `FLOORS` manifest.
- A post is one file in `src/content/blog/`. Its route, the lobby, the feed,
  Lighthouse's URLs and `check:docs`'s roster derive from that directory
  (`config/blog.mjs`). Edit these by hand:
  - the e2e rows that name its route (smoke's `PLATE_SURFACES` table);
  - `site.yml`'s `scope` paths, one for each `docs/` image it shows, which
    `config/rendered-docs-scope.test.mjs` holds to the posts.
- Blockquotes become terminal callouts (`config/rehype-callouts.mjs`,
  unit-tested). A new `.prose` colour rule needs a twin under
  `.callout__body`: the specificity arithmetic and its theme-prefixed tie are
  on the rule itself in `Wing.astro`, and the smoke suite's callout AA sweep
  grades it.

## CSP

`cspInlineHashes()` in `astro.config.mjs` owns the why and
`config/csp-hashes.mjs` the parse. For a page author:

- An `is:inline` script needs no manual CSP step.
- A hand-written `public/*.js` loaded by URL rides `script-src 'self'`.
- An inline `<style>` or `style` attribute needs no step either: each is
  hashed, an attribute's under `'unsafe-hashes'`.
- A style set at runtime (`setAttribute('style', …)`, `style.cssText`, markup
  through `innerHTML`) is blocked: set `el.style.<property>` instead.
- `astro dev` serves no CSP, so a regression shows only in `just site-e2e`'s
  console watchdog.
