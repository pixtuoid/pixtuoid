# site — single-sourced content

Every rendered copy on the site and where it is generated FROM. Hand-editing
the rendered copy is the recurring failure — the next `just gen-*` silently
reverts it. Rows whose drift a gate already catches are one line here; prose
only carries what no gate can.

## Generated artifacts (edit the source, never the output)

| Rendered output                                                 | Source of truth                                                                                                       | Regen             | Drift gate                                                                                                |
| --------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------- | ----------------- | --------------------------------------------------------------------------------------------------------- |
| Root `README.md` features table + install block + tools glimpse | `src/features.json` / `src/install.json` / `src/sources.json`                                                         | `just gen-readme` | `just gen-readme-check` (CI `generated drift` job)                                                        |
| `public/demos/*` stills + clips                                 | `scripts/media.json` → `gen-media.py` (real TuiRenderer; wasm `hero_still` for `hero-wide.png` / `vibing-poster.png`) | `just site-demos` | none: gitignored, rendered per CI build (`.github/actions/gen-demos`)                                     |
| `src/assets/pix-icons/*.png`                                    | sprite pack palette (`pack.toml`) → `gen-pix-icons.py`                                                                | `just gen-icons`  | `just gen-icons-check` (decode-compared); `config/pix-icons.test.mjs` bridges `features.json` `pix` names |

Notes no gate carries:

- The §3 proof media (`demos/proof*`) renders over the session in
  `crates/pixtuoid/examples/snapshot/proof-session/` — retime it, not the component.

## Manifest seams (build-guarded)

- **`features.json` is the total feature collection, partitioned by
  `channel`**: a `channel`-bearing row joins `showcase.json` and drives the 5F
  studio dial (`consts.ts` `featureForChannel`); channel↔id is a BIJECTION
  enforced by the `astro.config.mjs` guard — build fails loud on the offending
  id. Rows without `channel` render as the quiet roster grid.
- The six-floor vocabulary (`data-floor`/`data-floor-label`) stamps from the
  ONE `FLOORS` manifest in `consts.ts`; statusline lift, `ElevatorShaft`, and
  the scrollspy all read the same `FLOOR_SPY_ROOT_MARGIN` band so readouts
  can't disagree. `#features` is an anchor-compat shim atop the merged 5F
  band. `data-keys-scope="channels"` on `#showcase` claims digits locally.
- `__GH_STARS__` is a build-time `vite.define`, null on offline builds — every
  consumer omits its ★ segment on null (`config/plaque-stars.mjs` is the
  plaque's display authority, unit-tested on the null arm).

## Sharp edges (each pinned; don't "simplify")

- **`OfficeBackdrop`'s reveal roll is FRAME-driven, never clock-driven.**
  Safari can stall the whole main thread ~1.4s in tab-snapshot IPC right as
  the roll starts; a wall-clock ramp froze mid-roll and SNAPPED. So `paint()`
  accumulates `reveal.elapsed` per PAINTED frame (step clamped to
  `REVEAL_MAX_STEP_MS`, DERIVED from `FRAME_MS` — a hardcoded twin silently
  disarms the clamp), holds the start for `REVEAL_READY_FRAMES` on-budget
  frames (bounded by `REVEAL_READY_MAX_WAIT`), and defers `is-live` /
  `pix:onair` / ♩ until then. The four reveal fields share ONE `reveal` object
  because reduced-motion must rewind them together (pinned by the un-reduce
  e2e test). The stall is intermittent and invisible to Playwright — pin
  changes here with repeated interleaved trials, never a single A/B.
- **The caption overlay (`#office-overlay`)** lays DOM spans over the
  pixelated canvas from `Office.overlay_json()` (same badge/neon_sign model as
  the TUI/floating painters; `badge` = per-CLI hue, dot = activity tone). The
  engine re-derives the prefix by splitting on the FIRST `·` — a cross-crate
  echo pinned by the web `labels == badges` test, not a shared const. Rules:
  captions fade in only after the roll settles; `JSON.parse` try/caught;
  `.textContent` never `innerHTML` (cwds are untrusted); reduced-motion is
  `display:none`; caption legibility is HALO-carried by design (raw hues are
  deliberately not WCAG-gated against office pixels). Pinned by
  `smoke.spec.ts` "crisp AA captions overlay the live office".
- **Contrast has FOUR populations**, each swept: bare-over-office
  ("bare hero text clears WCAG AA…" — `officeGrounds` scrolls the element in
  AND asserts the read hit painted canvas, else `getImageData` grades "dimmer
  over black"); opaque plates + translucent `--screen` chips ("plate and chip
  text clears WCAG AA in every theme" — runs dracula too); the docs callout
  window (its own sweep). `paintedContrast` composites ancestor backgrounds
  down and FOLDS ancestor `opacity` into the ink. Corollary: de-emphasis
  comes from SIZE, never sub-AA ink or an opacity multiplier. Office-ground
  hues need theme-aware tokens (`--office-ink*`, `--led-ink`) measured
  against the real composite. `--led-glow` stays theme-independent lime on
  purpose: a shadow the glyph paints is decoration, not the ground WCAG
  grades against (measured worst case 5.28:1) — don't theme it.
- **♩ audio**: muted-by-default, gesture-gated (`AudioContext` only on first
  click); foley prewarms OFF-thread in `public/audio-worker.js` at idle (the
  click-time chunked warmup its fallback), music streams from `public/music-worker.js`
  into `public/music-worklet.js`, silent where either can't run; buffers COPIED out
  of wasm memory (views dangle on `memory.grow`); sound rides the render's
  pause-shifted `nowMs`. **iOS silent-switch bypass is DELIBERATE (#664)**:
  `navigator.audioSession.type = 'playback'` before the context, else the Ring/Silent switch mutes a deliberate ♩ tap.
- **The VIBING channel is a SECOND live `Office` (#468)**: each office keeps
  its own weather, and both consumers MUST share ONE `init()` via
  `public/office-driver.js` (`sharedWasm` / `officeFrameView`) — independent `mod.default()` calls
  racing instantiate two wasm instances that stomp the module-global. The
  bounded init retry lives INSIDE that one promise; it nulls only a SETTLED
  rejection (#671). Its rAF loop runs only active + in-view + not paused +
  not reduced-motion, and joins the `pix:paused` set as a consumer.
- **Scoped `<style>` does not reach `set:html` content** — runtime-injected
  markup carries no Astro scope hash; target it with `:global(...)`. Caught
  only by rendering, never the static gates.
- **The nav/footer logo mark IS the favicon** (`public/favicon-32*.png`, via
  `.js-brand-mark`; `Base.astro`'s `syncBrand` swaps tab + marks together).
  Don't reintroduce a separate mark asset; size at 32 or an integer fraction.
