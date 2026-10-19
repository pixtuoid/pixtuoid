import { expect, test, type Page } from '@playwright/test';
import sourcesData from '../../src/sources.json' with { type: 'json' };
import { E2E_STAR_COUNT } from '../../config/gh-stars.mjs';

type SourceRow = { badge: string; badge_color: string; name: string; status: string };
const supportedSources = (sourcesData as SourceRow[]).filter((s) => s.status === 'supported');

function relLuminance([r, g, b]: [number, number, number]): number {
  const lin = (c: number) => {
    const s = c / 255;
    return s <= 0.04045 ? s / 12.92 : Math.pow((s + 0.055) / 1.055, 2.4);
  };
  return 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b);
}
function contrastRatio(a: [number, number, number], b: [number, number, number]): number {
  const [la, lb] = [relLuminance(a), relLuminance(b)];
  const [hi, lo] = la > lb ? [la, lb] : [lb, la];
  return (hi + 0.05) / (lo + 0.05);
}
function compositeOver(
  [r, g, b, a]: [number, number, number, number],
  under: [number, number, number]
): [number, number, number] {
  const [ur, ug, ub] = under;
  return [r * a + ur * (1 - a), g * a + ug * (1 - a), b * a + ub * (1 - a)];
}
function parseRgb(css: string): [number, number, number, number] {
  const rgb = css.match(/rgba?\(([^)]+)\)/);
  if (rgb) {
    const [r, g, b, a] = rgb[1].split(',').map((s) => parseFloat(s));
    return [r, g, b, a ?? 1];
  }
  // Chromium resolves a color-mix() result to the `color(srgb r g b [/ a])`
  // form (0-1 components), not rgb() — the hero badge-code hue takes this path.
  const srgb = css.match(/color\(srgb\s+([\d.]+)\s+([\d.]+)\s+([\d.]+)(?:\s*\/\s*([\d.]+))?\)/);
  if (srgb) {
    const [r, g, b, a] = srgb.slice(1, 5).map((s) => (s === undefined ? undefined : parseFloat(s)));
    return [r! * 255, g! * 255, b! * 255, a ?? 1];
  }
  throw new Error(`unparseable color: ${css}`);
}

// The dimmer controller is IntersectionObserver-driven: its target opacity
// lands a frame or two after the scroll.
const SCROLL_SETTLE_MS = 300;

/** The observer adds `.in` once, then unobserves — `.in` IS the settled state. */
async function settleReveals(page: Page): Promise<void> {
  await page.evaluate(() =>
    document.querySelectorAll('.reveal').forEach((el) => el.classList.add('in'))
  );
  await page.waitForFunction(() =>
    [...document.querySelectorAll('.reveal')].every(
      (el) => parseFloat(getComputedStyle(el).opacity) === 1
    )
  );
}

/**
 * Brightest and darkest office pixel under an element's box, each composited with the live dimmer; `null` with no live office.
 * Both extremes, never an average: day's dimmer lightens the composite toward `--paper`, night's darkens it toward `--bg`.
 * The scroll is load-bearing — the canvas is a viewport-fixed backdrop, so an off-screen box indexes past its buffer and `getImageData` hands back ZEROED pixels.
 */
async function officeGrounds(
  page: Page,
  selector: string,
  nth = 0
): Promise<[number, number, number][] | null> {
  if ((await page.locator('#office-live').count()) === 0) return null;
  await page.locator(selector).nth(nth).scrollIntoViewIfNeeded();
  await page.waitForTimeout(SCROLL_SETTLE_MS);
  const sampled = await page.evaluate(
    ([sel, i]) => {
      const canvas = document.getElementById('office-live') as HTMLCanvasElement | null;
      if (!canvas || parseFloat(getComputedStyle(canvas).opacity) === 0) return null;
      const el = document.querySelectorAll(sel)[i as number];
      const r = el.getBoundingClientRect();
      const cr = canvas.getBoundingClientRect();
      const sx = canvas.width / cr.width;
      const sy = canvas.height / cr.height;
      const ctx = canvas.getContext('2d', { willReadFrequently: true })!;
      const x0 = Math.min(canvas.width - 1, Math.max(0, Math.floor((r.left - cr.left) * sx)));
      const y0 = Math.min(canvas.height - 1, Math.max(0, Math.floor((r.top - cr.top) * sy)));
      const w = Math.min(Math.max(1, Math.ceil(r.width * sx)), canvas.width - x0);
      const h = Math.min(Math.max(1, Math.ceil(r.height * sy)), canvas.height - y0);
      const data = ctx.getImageData(x0, y0, w, h).data;
      const relLum = ([rr, gg, bb]: number[]) => {
        const lin = (c: number) => {
          const s = c / 255;
          return s <= 0.04045 ? s / 12.92 : Math.pow((s + 0.055) / 1.055, 2.4);
        };
        return 0.2126 * lin(rr) + 0.7152 * lin(gg) + 0.0722 * lin(bb);
      };
      let maxLum = -1,
        maxPx = [0, 0, 0],
        minLum = 2,
        minPx = [0, 0, 0],
        painted = 0;
      for (let k = 0; k < data.length; k += 4) {
        if (data[k + 3] === 0) continue;
        painted++;
        const px = [data[k], data[k + 1], data[k + 2]];
        const lum = relLum(px);
        if (lum > maxLum) {
          maxLum = lum;
          maxPx = px;
        }
        if (lum < minLum) {
          minLum = lum;
          minPx = px;
        }
      }
      const dimmer = document.getElementById('dimmer') as HTMLElement;
      return {
        maxPx,
        minPx,
        painted,
        total: data.length / 4,
        dimmerBg: getComputedStyle(dimmer).backgroundColor,
        dimmerOpacity: parseFloat(dimmer.style.opacity || '0'),
      };
    },
    [selector, nth] as const
  );
  if (!sampled) return null;
  expect(
    sampled.painted,
    `${selector}[${nth}]: sampled ${sampled.total} office pixels, none painted — the box indexed past the canvas buffer, so the grade below would be "dimmer over black", not the office`
  ).toBeGreaterThan(0);
  const dim = [
    ...(parseRgb(sampled.dimmerBg).slice(0, 3) as [number, number, number]),
    sampled.dimmerOpacity,
  ] as [number, number, number, number];
  return [
    compositeOver(dim, sampled.maxPx as [number, number, number]),
    compositeOver(dim, sampled.minPx as [number, number, number]),
  ];
}

/**
 * The worst contrast ratio the element's text ACTUALLY renders at: every ancestor background composited down and every ancestor
 * `opacity` folded into the ink, over the office composite where a live office is the ground and the page's own plate where it is not.
 * Folding `opacity` is the difference between graded and rendered — a group at `opacity: .7` shows 30% of its ground through the glyph.
 */
async function paintedContrast(page: Page, selector: string, nth = 0): Promise<number> {
  const { ink, chain } = await page.evaluate(
    ([sel, i]) => {
      const el = document.querySelectorAll(sel)[i as number];
      const chain: { bg: string; opacity: number; isPageRoot: boolean }[] = [];
      for (let n: Element | null = el; n; n = n.parentElement) {
        const cs = getComputedStyle(n);
        chain.push({
          bg: cs.backgroundColor,
          opacity: parseFloat(cs.opacity),
          isPageRoot: n === document.body || n === document.documentElement,
        });
      }
      return { ink: getComputedStyle(el).color, chain };
    },
    [selector, nth] as const
  );
  const inkRgb = parseRgb(ink).slice(0, 3) as [number, number, number];
  // `opacity` fades a node's OWN background and everything inside it, never an
  // ancestor's — so a layer's group factor is the product from the ROOT down.
  const inkAlpha = chain.reduce((p, c) => p * c.opacity, 1);
  const groupAt = chain.map((_, k) => chain.slice(k).reduce((p, c) => p * c.opacity, 1));
  // An opaque plate makes whatever is under the page immaterial, so only a
  // chain translucent all the way out pays for the office sample's scroll.
  const plated = chain.some((c, k) => !c.isPageRoot && parseRgb(c.bg)[3] * groupAt[k] >= 1);
  const grounds = plated ? null : await officeGrounds(page, selector, nth);
  const seeds: [number, number, number][] = grounds ?? [[255, 255, 255]];
  let worst = Infinity;
  for (const seed of seeds) {
    let ground = seed;
    for (let k = chain.length - 1; k >= 0; k--) {
      // the office backdrop is a fixed sibling painting OVER the page background
      if (grounds && chain[k].isPageRoot) continue;
      const [r, g, b, a] = parseRgb(chain[k].bg);
      if (a * groupAt[k] > 0) ground = compositeOver([r, g, b, a * groupAt[k]], ground);
    }
    worst = Math.min(worst, contrastRatio(compositeOver([...inkRgb, inkAlpha], ground), ground));
  }
  return worst;
}
function watchErrors(page: Page): () => string[] {
  const errors: string[] = [];
  page.on('pageerror', (e) => errors.push(`pageerror: ${e.message}`));
  page.on('console', (msg) => {
    if (msg.type() === 'error') errors.push(`console.error: ${msg.text()}`);
  });
  return () => errors;
}

/**
 * The scroll is INSIDE the retry: Chromium's async scroll restoration after
 * reload() and late layout settling both keep moving the page under a slow
 * load, parking the viewport where the head never intersects the threshold.
 */
async function expectSectionReveal(page: Page, sectionId: string): Promise<void> {
  await expect(async () => {
    await page.evaluate(
      (id) => document.getElementById(id)!.scrollIntoView({ block: 'center', behavior: 'instant' }),
      sectionId
    );
    await expect(page.locator(`#${sectionId} .section-head.reveal`)).toHaveClass(/\bin\b/, {
      timeout: 500,
    });
  }).toPass({ timeout: 10_000 });
}

async function gotoLive(page: Page): Promise<void> {
  await page.addInitScript(() => sessionStorage.setItem('pix-booted', '1'));
  await page.goto('./');
  // A timeout here is the ABI-mismatch / loader-regression signal.
  await expect(page.locator('.backdrop.is-live')).toBeAttached({ timeout: 15_000 });
}

test('the office goes live and the statusline truth-light agrees', async ({ page }) => {
  const errors = watchErrors(page);
  await gotoLive(page);
  await expect(page.locator('[data-sl-onair]')).toHaveText('● LIVE', { timeout: 10_000 });
  // Buffer height is fixed at 130: width = min(640, max(64, round(w/h · 130))).
  const bufW = () =>
    page.evaluate(() => (document.getElementById('office-live') as HTMLCanvasElement).width);
  expect(await bufW()).toBe(231);
  await page.setViewportSize({ width: 500, height: 900 });
  await expect.poll(bufW).toBe(72);
  expect(errors()).toEqual([]);
});

test('the cross-component window contracts exist', async ({ page }) => {
  await gotoLive(page);
  await expect
    .poll(async () =>
      page.evaluate(() => ({
        night: typeof window.__pixNight === 'function' && typeof window.__pixNight() === 'boolean',
        hire: typeof window.__pixHire === 'function',
        lights: typeof window.__pixLights,
        revealed: window.__pixRevealed === true,
        engineReady: window.__pixEngineReady === true,
      }))
    )
    .toEqual({ night: true, hire: true, lights: 'number', revealed: true, engineReady: true });
});

test('digit keys ride between floors (scrollspy round-trip)', async ({ page }) => {
  await gotoLive(page);
  await page.keyboard.press('3');
  await expect(page.locator('[data-lift-digit]')).toHaveText('3F', { timeout: 10_000 });
  await page.keyboard.press('1');
  await expect(page.locator('[data-lift-digit]')).toHaveText('1F', { timeout: 10_000 });
});

test('scrolled to the true page bottom, the statusline clamps to the last floor', async ({
  page,
}) => {
  await gotoLive(page);
  await expect(async () => {
    await page.evaluate(() => window.scrollTo(0, document.documentElement.scrollHeight));
    await expect(page.locator('[data-lift-digit]')).toHaveText('1F', { timeout: 500 });
  }).toPass({ timeout: 10_000 });
});

test('the dimmer darkens statements and releases in office gaps', async ({ page }) => {
  await gotoLive(page);
  const dim = () =>
    page.evaluate(() => parseFloat(document.getElementById('dimmer')!.style.opacity || '0'));
  await page.evaluate(() =>
    document.getElementById('install')!.scrollIntoView({ block: 'center', behavior: 'instant' })
  );
  await expect.poll(dim).toBeGreaterThan(0.5);
  await page.evaluate(() =>
    document.querySelector('.office-gap')!.scrollIntoView({ block: 'center', behavior: 'instant' })
  );
  await expect.poll(dim).toBeLessThan(0.15);
  // The hero parks at 0.001 while a statement owns the viewport center.
  const heroOp = () =>
    page.evaluate(() =>
      parseFloat((document.querySelector('.hero__copy') as HTMLElement).style.opacity || '1')
    );
  await page.evaluate(() =>
    document.getElementById('install')!.scrollIntoView({ block: 'center', behavior: 'instant' })
  );
  await expect.poll(heroOp).toBeLessThan(0.01);
  await page.evaluate(() => window.scrollTo({ top: 0, behavior: 'instant' }));
  await expect.poll(heroOp).toBeGreaterThan(0.5);
});

test('the dimmer tracks live geometry across a Showcase channel swap', async ({ page }) => {
  await gotoLive(page);
  // Straddle the gap-2 observation hold — the reported bug's location.
  await page.evaluate(() => window.scrollTo({ top: 3763, behavior: 'instant' }));

  // Ground truth: the dimmer's own formula recomputed from LIVE rects and the
  // LIVE (possibly scroll-anchor-adjusted) scrollY, not from anything cached.
  const liveTruth = () =>
    page.evaluate(() => {
      const y = window.scrollY;
      const innerH = window.innerHeight;
      const center = y + innerH / 2;
      const reach = innerH * 0.55;
      let best = 0;
      let bestCap = 0.86;
      document.querySelectorAll<HTMLElement>('[data-lit]').forEach((el) => {
        const r = el.getBoundingClientRect();
        const top = r.top + y;
        const bottom = r.bottom + y;
        const d = center < top ? top - center : center > bottom ? center - bottom : 0;
        const p = d >= reach ? 0 : 1 - d / reach;
        if (p > best) {
          best = p;
          bestCap = el.dataset.litMax ? parseFloat(el.dataset.litMax) : 0.86;
        }
      });
      const ease = (t: number) => t * t * (3 - 2 * t);
      return bestCap * ease(best);
    });
  const pageOp = () =>
    page.evaluate(() => parseFloat(document.getElementById('dimmer')!.style.opacity || '0'));

  const channels = ['agents', 'openclaw', 'dashboard', 'meetings', 'pets', 'spaces', 'vibing'];
  for (const ch of channels) {
    await page.evaluate(
      (id) => (document.querySelector(`.dial__ch[data-ch="${id}"]`) as HTMLElement | null)?.click(),
      ch
    );
    await expect
      .poll(async () => Math.abs((await pageOp()) - (await liveTruth())), {
        message: `dimmer opacity vs live ground truth after switching to "${ch}"`,
      })
      .toBeLessThan(0.01);
  }
});

test('the hero pause switch freezes the office and resumes it seamlessly', async ({ page }) => {
  const errors = watchErrors(page);
  await gotoLive(page);
  const btn = page.locator('#office-pause');
  await expect(btn).toBeVisible();
  await expect(btn).toHaveAttribute('aria-pressed', 'false');
  const shot = () =>
    page.evaluate(() => (document.getElementById('office-live') as HTMLCanvasElement).toDataURL());
  const bufW = () =>
    page.evaluate(() => (document.getElementById('office-live') as HTMLCanvasElement).width);
  await btn.click();
  await expect(btn).toHaveAttribute('aria-pressed', 'true');
  const frozen = await shot();
  await page.waitForTimeout(400); // >10 would-be frames at the 33ms cap
  expect(await shot()).toBe(frozen);
  await expect(page.locator('[data-sl-onair]')).toHaveText('❚❚ PAUSED');
  // sizeBuffer() wipes the bitmap and no rAF will repaint it, so the resize
  // handler must re-render the ONE frozen frame — else a blank var(--bg) void.
  await page.setViewportSize({ width: 500, height: 900 });
  await expect.poll(bufW).toBe(72);
  expect(await btn.getAttribute('aria-pressed')).toBe('true');
  const painted = await page.evaluate(() => {
    const c = document.getElementById('office-live') as HTMLCanvasElement;
    const d = c.getContext('2d')!.getImageData(0, 0, c.width, c.height).data;
    return d.some((v) => v !== 0);
  });
  expect(painted).toBe(true);
  const frozen2 = await shot();
  await page.waitForTimeout(400);
  expect(await shot()).toBe(frozen2);
  await btn.focus();
  await page.keyboard.press('Enter');
  await expect(btn).toHaveAttribute('aria-pressed', 'false');
  await expect.poll(shot, { timeout: 10_000 }).not.toBe(frozen2);
  await expect(page.locator('[data-sl-onair]')).toHaveText('● LIVE');
  expect(errors()).toEqual([]);
});

test('the hero ♩ sound toggle: muted by default, gesture-gated, no AudioContext until clicked', async ({
  page,
}) => {
  await page.addInitScript(() => {
    (window as unknown as { __acCount: number }).__acCount = 0;
    const Real = window.AudioContext;
    if (!Real) return;
    const Wrapped = function (this: unknown, ...args: unknown[]) {
      (window as unknown as { __acCount: number }).__acCount++;
      return new (Real as new (..._a: unknown[]) => AudioContext)(...args);
    } as unknown as typeof AudioContext;
    Wrapped.prototype = Real.prototype;
    window.AudioContext = Wrapped;
  });
  const errors = watchErrors(page);
  await gotoLive(page);
  const btn = page.locator('#office-audio');
  const acCount = () => page.evaluate(() => (window as unknown as { __acCount: number }).__acCount);
  await expect(btn).toBeVisible({ timeout: 15_000 });
  await expect(btn).toHaveAttribute('aria-pressed', 'false');
  expect(await acCount()).toBe(0);
  // In headless (no audio backend) createBuffer throws and the ♩ degrades to
  // hidden; either way, no throw and never a second AudioContext.
  await btn.click();
  await expect.poll(acCount).toBe(1);
  await page.waitForTimeout(3000); // beds synth + live ticks
  expect(await acCount()).toBe(1);
  expect(errors()).toEqual([]);
});

test('the ♩ bus holds an over-full sum under the native ceiling and passes a quiet one at unity', async ({
  page,
}) => {
  const errors = watchErrors(page);
  await gotoLive(page);
  const { ceiling, loudPeak, quietPeak } = await page.evaluate(async () => {
    const { officeDriverUrl, wasmJsUrl } = (
      document.querySelector('script[data-office-driver-url]') as HTMLScriptElement
    ).dataset;
    const drv = await import(officeDriverUrl!);
    const office = new (await drv.sharedWasm(wasmJsUrl!)).Office(0);
    const rate = 48_000;
    // Each tone starts at `startS`; the peak is read from `fromS` on, since the spec starts the compressor's detector
    // average at 0.
    async function peak(amplitudes: number[], startS: number, fromS: number): Promise<number> {
      const ctx = new OfflineAudioContext(1, rate * 2, rate);
      const bus = drv.audioBus(ctx, office);
      amplitudes.forEach((gain, i) => {
        const tone = new OscillatorNode(ctx, { frequency: 110 * (i + 1) });
        tone.connect(new GainNode(ctx, { gain })).connect(bus);
        tone.start(startS);
      });
      const out = (await ctx.startRendering()).getChannelData(0).subarray(fromS * rate);
      return out.reduce((a, v) => Math.max(a, Math.abs(v)), 0);
    }
    return {
      ceiling: Math.fround(10 ** (office.audio_ceiling_dbfs() / 20)),
      loudPeak: await peak([0.9, 0.9, 0.9], 0.5, 0),
      quietPeak: await peak([0.1], 0, 1),
    };
  });
  expect(loudPeak, 'a burst of three summed lanes passed the ceiling').toBeLessThanOrEqual(ceiling);
  expect(loudPeak, 'the summed lanes never reached the output').toBeGreaterThan(ceiling / 2);
  expect(quietPeak, 'a −20 dBFS tone came out scaled: the makeup gain is uncancelled').toBeCloseTo(
    0.1,
    3
  );
  expect(errors()).toEqual([]);
});

test('♩ music: the music worker streams lofi into the worklet, which plays it', async ({
  page,
}) => {
  const errors = watchErrors(page);
  await gotoLive(page);
  const { peak, stereoIsMono } = await page.evaluate(async () => {
    const { officeDriverUrl, wasmJsUrl, musicWorkerUrl, musicWorkletUrl } = (
      document.querySelector('script[data-office-driver-url]') as HTMLScriptElement
    ).dataset;
    const drv = await import(officeDriverUrl!);
    const office = new (await drv.sharedWasm(wasmJsUrl!)).Office(0);
    // The controls as a tick hands them over.
    office.step(Date.now(), 64, 64);
    office.audio_begin();
    while (office.audio_warmup_step() > 0) {
      /* the bank, then the rain */
    }
    const { controls } = JSON.parse(office.audio_tick(0)).music;
    const rate = office.audio_sample_rate();
    const ctx = new OfflineAudioContext(2, rate, rate);
    const urls = { worklet: musicWorkletUrl, worker: musicWorkerUrl, wasm: wasmJsUrl };
    const ahead = office.audio_music_ahead_s();
    const music = await drv.startMusic(ctx, urls, ahead, Date.now(), controls);
    music.node.connect(ctx.destination);
    // The first chunk crossed worklet → worker ("need") → worklet before a quantum renders.
    await music.primed;
    const out = await ctx.startRendering();
    music.stop();
    const [left, right] = [out.getChannelData(0), out.getChannelData(1)];
    return {
      peak: left.reduce((a, v) => Math.max(a, Math.abs(v)), 0),
      stereoIsMono: left.every((v, i) => v === right[i]),
    };
  });
  expect(peak, 'the worklet played no music').toBeGreaterThan(0.001);
  expect(stereoIsMono, 'lofi renders L == R, and the worklet keeps both').toBe(true);
  expect(errors()).toEqual([]);
});

test('the ♩ click starts the music from the office it hears', async ({ page }) => {
  const errors = watchErrors(page);
  await gotoLive(page);
  await page.locator('#office-audio').click();
  await page.waitForFunction(
    () => (window as unknown as { __pixMusic?: string }).__pixMusic === 'playing',
    undefined,
    { timeout: 60_000 }
  );
  expect(errors()).toEqual([]);
});

test('every ♩ lane and one-shot reaches the speakers only through the limiter bus', async ({
  page,
}) => {
  type Wiring = { intoBus: { from: string; at: number }[]; intoDestination: string[] };
  await page.addInitScript(() => {
    const w = window as unknown as { __wiring: Wiring };
    w.__wiring = { intoBus: [], intoDestination: [] };
    const real = AudioNode.prototype.connect;
    AudioNode.prototype.connect = function (this: AudioNode, ...args: unknown[]) {
      const [to] = args;
      const from = this.constructor.name;
      if (to instanceof DynamicsCompressorNode)
        w.__wiring.intoBus.push({ from, at: performance.now() });
      if (to instanceof AudioDestinationNode) w.__wiring.intoDestination.push(from);
      return Reflect.apply(real, this, args);
    } as typeof real;
  });
  const errors = watchErrors(page);
  await gotoLive(page);
  await page.locator('#office-audio').click();
  await page.waitForFunction(
    () => (window as unknown as { __pixAudioReadyAt?: number }).__pixAudioReadyAt !== undefined,
    undefined,
    { timeout: 60_000 }
  );
  const wiring = () => page.evaluate(() => (window as unknown as { __wiring: Wiring }).__wiring);
  // The lanes connect while the buffers upload, before ready; a one-shot only after.
  const readyAt = await page.evaluate(
    () => (window as unknown as { __pixAudioReadyAt: number }).__pixAudioReadyAt
  );
  await expect
    .poll(async () => (await wiring()).intoBus.filter((c) => c.at > readyAt).length, {
      message: 'no one-shot joined the bus',
      timeout: 15_000,
    })
    .toBeGreaterThan(0);
  const { intoBus, intoDestination } = await wiring();
  expect(new Set(intoBus.map((c) => c.from))).toEqual(new Set(['GainNode']));
  expect(intoDestination, 'only the bus clamp may reach the speakers').toEqual(['WaveShaperNode']);
  expect(errors()).toEqual([]);
});

test('background playback: a hidden tab keeps ticking, reduced-motion stops it cold', async ({
  page,
}) => {
  const errors = watchErrors(page);
  await page.addInitScript(() => {
    const w = window as unknown as { __onairLive: number };
    w.__onairLive = 0;
    document.addEventListener('pix:onair', (e) => {
      if ((e as CustomEvent).detail?.live) w.__onairLive++;
    });
  });
  await gotoLive(page);
  const btn = page.locator('#office-audio');
  await expect(btn).toBeVisible({ timeout: 15_000 });
  await btn.click();
  await page.waitForTimeout(3000); // warmup + a few ticks
  const liveFires = () =>
    page.evaluate(() => (window as unknown as { __onairLive: number }).__onairLive);
  const baseline = await liveFires();
  // simulate a hidden tab (document.hidden is read-only — shadow it)
  await page.evaluate(() => {
    Object.defineProperty(document, 'hidden', { get: () => true, configurable: true });
    Object.defineProperty(document, 'visibilityState', {
      get: () => 'hidden',
      configurable: true,
    });
    document.dispatchEvent(new Event('visibilitychange'));
  });
  await page.waitForTimeout(2500); // ≥2 background ticks
  await page.emulateMedia({ reducedMotion: 'reduce' });
  await page.waitForTimeout(2500);
  expect(await liveFires()).toBe(baseline);
  expect(errors()).toEqual([]);
});

test('audio prewarm: the idle worker hands the take over and the ♩ click plays it', async ({
  page,
}) => {
  const errors = watchErrors(page);
  await gotoLive(page);
  // worker spawn (idle) + off-thread synthesis + adoption; generous for CI
  await page.waitForFunction(
    () => (window as unknown as { __pixAudioPrewarm?: string }).__pixAudioPrewarm === 'adopted',
    undefined,
    { timeout: 60_000 }
  );
  await page.locator('#office-audio').click();
  await page.waitForFunction(
    () => (window as unknown as { __pixAudioReadyAt?: number }).__pixAudioReadyAt !== undefined,
    undefined,
    { timeout: 10_000 }
  );
  expect(errors()).toEqual([]);
});

test('dead audio workers leave the music silent and the foley on the click-time chunked warmup', async ({
  page,
}) => {
  const errors = watchErrors(page);
  await page.route('**/audio-worker.js', (route) => route.abort());
  await page.route('**/music-worker.js', (route) => route.abort());
  await gotoLive(page);
  await page.waitForFunction(
    () => (window as unknown as { __pixAudioPrewarm?: string }).__pixAudioPrewarm === 'failed',
    undefined,
    { timeout: 30_000 }
  );
  await page.locator('#office-audio').click();
  await page.waitForFunction(
    () => (window as unknown as { __pixAudioReadyAt?: number }).__pixAudioReadyAt !== undefined,
    undefined,
    { timeout: 60_000 }
  );
  await page.waitForFunction(
    () => (window as unknown as { __pixMusic?: string }).__pixMusic === 'silent',
    undefined,
    { timeout: 30_000 }
  );
  await expect(page.locator('#office-audio'), 'the ♩ stays: the foley still plays').toBeVisible();
  expect(errors()).toEqual([]);
});

test('enabling ♩ sets navigator.audioSession = playback so iOS silent mode does not mute the opt-in', async ({
  page,
}) => {
  // iOS Safari routes default WebAudio to the ambient channel (the hardware Ring/Silent switch mutes it), so the ♩ opt-in
  // sets audioSession.type to 'playback'. The API is Safari-only — mocked here to verify the WIRING.
  await page.addInitScript(() => {
    Object.defineProperty(navigator, 'audioSession', {
      value: { type: 'auto' },
      configurable: true,
      writable: true,
    });
    // Capture the category AT construction: a context inherits its routing then, so setting 'playback' afterwards ends AS
    // 'playback' yet still plays on the ambient channel — a bare end-state check would pass that broken reorder.
    const w = window as unknown as { __acTypeAtCtor: string | null };
    w.__acTypeAtCtor = null;
    const Real = window.AudioContext;
    if (!Real) return;
    const Wrapped = function (this: unknown, ...args: unknown[]) {
      w.__acTypeAtCtor = (
        navigator as unknown as { audioSession: { type: string } }
      ).audioSession.type;
      return new (Real as new (..._a: unknown[]) => AudioContext)(...args);
    } as unknown as typeof AudioContext;
    Wrapped.prototype = Real.prototype;
    window.AudioContext = Wrapped;
  });
  const errors = watchErrors(page);
  await gotoLive(page);
  const btn = page.locator('#office-audio');
  await expect(btn).toBeVisible({ timeout: 15_000 });
  const atCtor = () =>
    page.evaluate(() => (window as unknown as { __acTypeAtCtor: string | null }).__acTypeAtCtor);
  expect(
    await page.evaluate(
      () => (navigator as unknown as { audioSession: { type: string } }).audioSession.type
    )
  ).toBe('auto');
  expect(await atCtor()).toBeNull();
  await btn.click();
  await expect.poll(atCtor).toBe('playback');
  expect(errors()).toEqual([]);
});

test('a remembered ♩ choice never inverts a direct first click on the button', async ({ page }) => {
  // On a FIRST gesture that is a direct ♩ click, the remembered-"on" restore must not fire (→on) and let the button's own
  // click toggle it back (→off). Pass = playing, or gracefully hidden where WebAudio has no backend.
  const errors = watchErrors(page);
  await gotoLive(page);
  await page.evaluate(() => localStorage.setItem('pix:audio', '1'));
  await page.reload();
  await expect(page.locator('.backdrop.is-live')).toBeAttached({ timeout: 15_000 });
  const btn = page.locator('#office-audio');
  await expect(btn).toBeVisible({ timeout: 15_000 });
  await btn.click();
  await expect
    .poll(async () => {
      const pressed = await btn.getAttribute('aria-pressed');
      const hidden = await btn.evaluate((el) => (el as HTMLElement).hidden);
      return (pressed === 'true' && !hidden) || (hidden && pressed === 'false');
    })
    .toBe(true);
  const invertedBug =
    (await btn.getAttribute('aria-pressed')) === 'false' &&
    !(await btn.evaluate((el) => (el as HTMLElement).hidden));
  expect(
    invertedBug,
    'the ♩ is visible with aria-pressed=false — the restore inverted the first click'
  ).toBe(false);
  expect(errors()).toEqual([]);
});

test('a remembered ♩ choice waits for a gesture that grants activation, not an Escape', async ({
  page,
}) => {
  // HTML's activation-triggering events exclude an Escape keydown (and a touch's
  // pointerdown): restoring on one would "resume" audio that stays suspended.
  // Automated Chromium reports activation from navigation on, so the page reads
  // a stub the test flips.
  const errors = watchErrors(page);
  await page.addInitScript(() => {
    const w = window as unknown as { __activated: boolean };
    w.__activated = false;
    Object.defineProperty(navigator, 'userActivation', {
      value: {
        get isActive() {
          return w.__activated;
        },
      },
    });
  });
  await gotoLive(page);
  await page.evaluate(() => localStorage.setItem('pix:audio', '1'));
  await page.reload();
  await expect(page.locator('.backdrop.is-live')).toBeAttached({ timeout: 15_000 });
  const btn = page.locator('#office-audio');
  await expect(btn).toBeVisible({ timeout: 15_000 });
  await page.keyboard.press('Escape');
  await page.waitForTimeout(300);
  await expect(btn, 'a gesture without activation restored the remembered ♩').toHaveAttribute(
    'aria-pressed',
    'false'
  );
  await page.evaluate(() => {
    (window as unknown as { __activated: boolean }).__activated = true;
  });
  await page.keyboard.press('Shift');
  await expect
    .poll(async () => {
      const pressed = await btn.getAttribute('aria-pressed');
      const hidden = await btn.evaluate((el) => (el as HTMLElement).hidden);
      return pressed === 'true' || hidden;
    })
    .toBe(true);
  expect(errors()).toEqual([]);
});

test('the dracula sequence honours the keyboard-shortcut off-switch (WCAG 2.1.4)', async ({
  page,
}) => {
  await page.addInitScript(() => sessionStorage.setItem('pix-booted', '1'));
  await page.goto('./');
  await page.evaluate(() => localStorage.setItem('pix-keys', 'off'));
  await page.reload();
  await page.locator('body').click({ position: { x: 5, y: 5 } });
  await page.keyboard.type('dracula');
  await expect(page.locator('html')).not.toHaveAttribute('data-theme', 'dracula');
  await page.evaluate(() => localStorage.setItem('pix-keys', 'on'));
  await page.reload();
  await page.locator('body').click({ position: { x: 5, y: 5 } });
  await page.keyboard.type('dracula');
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'dracula');
});

test('crisp AA captions overlay the live office (name badges + neon board)', async ({ page }) => {
  const errors = watchErrors(page);
  await gotoLive(page);
  // The caption layer fades in only AFTER the reveal roll settles, so wait on
  // is-on, not merely is-live.
  await expect(page.locator('#office-overlay.is-on')).toBeAttached({ timeout: 10_000 });
  // 10s covers the cast's staggered walk-in at loop start.
  const label = page.locator('#office-overlay .ov-label').first();
  await expect(label).toHaveText(/\S/, { timeout: 10_000 });
  const labelFont = await label.evaluate((el) => getComputedStyle(el).fontFamily);
  expect(labelFont).toContain('Monaspace Neon');
  const parts = await label.evaluate((el) =>
    Array.from(el.children).map((c) => ({
      text: c.textContent,
      color: (c as HTMLElement).style.color,
    }))
  );
  expect(parts).toHaveLength(2);
  expect(parts[0].text).toBe('●');
  expect(parts[1].text).toMatch(/^[a-z]{2}·/);
  expect(parts[1].color).not.toEqual('');
  const brand = page.locator('#office-overlay .ov-board .ov-brow--top span').first();
  await expect(brand).toHaveText(/\S/, { timeout: 10_000 });
  const brandFont = await brand.evaluate((el) => getComputedStyle(el).fontFamily);
  expect(brandFont).toContain('Monaspace Neon');
  expect(errors()).toEqual([]);
});

test('the neon board keeps one monospace column per char, through a flap roll', async ({
  page,
}) => {
  const errors = watchErrors(page);
  await gotoLive(page);
  await expect(page.locator('#office-overlay.is-on')).toBeAttached({ timeout: 10_000 });
  // Only a multi-segment row exercises `.ov-brow`'s `pre` (WHY on the rule).
  const mood = page.locator('#office-overlay .ov-board .ov-brow').nth(1);
  await expect.poll(() => mood.locator('span').count(), { timeout: 15_000 }).toBeGreaterThan(1);
  // scene `neon_sign.rs` FLAP_HALF_MS — one half always crosses a roll; the
  // "crossed a roll" assert below reds if the half grows.
  const samples = await mood.evaluate(async (row) => {
    const HALF_MS = 4_000;
    const probe = document.createElement('span');
    probe.textContent = 'M'.repeat(10);
    probe.style.cssText = 'position:absolute;visibility:hidden;white-space:pre';
    const out: { text: string; left: number; width: number; ch: number }[] = [];
    const t0 = performance.now();
    while (performance.now() - t0 < HALF_MS) {
      await new Promise(requestAnimationFrame);
      const spans = Array.from(row.children) as HTMLElement[];
      if (spans.length === 0) continue;
      row.appendChild(probe);
      const ch = probe.getBoundingClientRect().width / 10;
      probe.remove();
      const left = spans[0].getBoundingClientRect().left;
      const right = spans[spans.length - 1].getBoundingClientRect().right;
      out.push({ text: spans.map((c) => c.textContent).join(''), left, width: right - left, ch });
    }
    return out;
  });
  expect(samples.length).toBeGreaterThan(30);
  expect(new Set(samples.map((s) => s.text)).size, 'the window crossed a roll').toBeGreaterThan(2);
  for (const s of samples) {
    expect(Math.abs(s.width - s.text.length * s.ch), `"${s.text}"`).toBeLessThan(s.ch / 2);
    expect(Math.abs(s.left - samples[0].left), `"${s.text}"`).toBeLessThan(1);
  }
  // The context row bakes its separators the same way; one settled read covers it.
  const ctx = await page
    .locator('#office-overlay .ov-board .ov-brow')
    .nth(2)
    .evaluate((row) => {
      const spans = Array.from(row.children) as HTMLElement[];
      const left = spans[0].getBoundingClientRect().left;
      const right = spans[spans.length - 1].getBoundingClientRect().right;
      return { text: spans.map((c) => c.textContent).join(''), width: right - left };
    });
  expect(Math.abs(ctx.width - ctx.text.length * samples[0].ch), `"${ctx.text}"`).toBeLessThan(
    samples[0].ch / 2
  );
  expect(errors()).toEqual([]);
});

test('reduced motion hides the caption overlay (still poster, no captions)', async ({
  browser,
}) => {
  const context = await browser.newContext({ reducedMotion: 'reduce' });
  const page = await context.newPage();
  await page.addInitScript(() => sessionStorage.setItem('pix-booted', '1'));
  await page.goto('./');
  await expect(page.locator('.backdrop.is-live')).not.toBeAttached();
  await expect(page.locator('#office-overlay.is-on')).not.toBeAttached();
  await expect(page.locator('#office-overlay')).toBeHidden();
  await context.close();
});

test('the install Copy click hires without breaking the page', async ({ page, context }) => {
  await context.grantPermissions(['clipboard-write']);
  const errors = watchErrors(page);
  await gotoLive(page);
  await page.evaluate(() =>
    document.getElementById('install')!.scrollIntoView({ block: 'center', behavior: 'instant' })
  );
  const copy = page.locator('.install__panel.is-active .install__copy');
  await copy.click();
  // The copy flash proves the click handler ran to completion — the post-copy
  // pix:install-copy dispatch didn't throw.
  await expect(copy).toHaveText(/Copied|Select & copy/);
  expect(errors()).toEqual([]);
});

test('the hire cap stops the receipt at 3 but keeps hiring every time', async ({
  page,
  context,
}) => {
  // The engine's own bool return is the ONE admission signal — no JS-side
  // mirror of `VisitorHires::MAX_LIVE` to drift out of lockstep.
  await context.grantPermissions(['clipboard-write']);
  const errors = watchErrors(page);
  await gotoLive(page);
  await page.evaluate(() =>
    document.getElementById('install')!.scrollIntoView({ block: 'center', behavior: 'instant' })
  );
  await page.evaluate(() => {
    (window as unknown as { __hired: string[] }).__hired = [];
    document.addEventListener('pix:hired', (e) =>
      (window as unknown as { __hired: string[] }).__hired.push(
        (e as CustomEvent<{ name: string }>).detail.name
      )
    );
    // Instrument the REAL Office.hire() BEFORE any copy fires — it must forward
    // its bool return, or the admission signal gating pix:hired goes missing.
    const real = window.__pixHire!;
    (window as unknown as { __hireResults: boolean[] }).__hireResults = [];
    window.__pixHire = function () {
      const admitted = real();
      (window as unknown as { __hireResults: boolean[] }).__hireResults.push(admitted);
      return admitted;
    };
  });
  const copy = page.locator('.install__panel.is-active .install__copy');
  for (let i = 0; i < 4; i++) {
    await copy.click();
    // wait for THIS click's hire() result before firing the next — the
    // clipboard-write → pix:install-copy → hire() chain is async.
    await expect
      .poll(() =>
        page.evaluate(
          () => (window as unknown as { __hireResults: boolean[] }).__hireResults.length
        )
      )
      .toBe(i + 1);
  }
  await expect
    .poll(() => page.evaluate(() => (window as unknown as { __hired: string[] }).__hired))
    .toEqual(['cc·yours', 'cc·yours', 'cc·yours']);
  expect(
    await page.evaluate(() => (window as unknown as { __hireResults: boolean[] }).__hireResults)
  ).toEqual([true, true, true, false]);
  expect(errors()).toEqual([]);
});

test('reduced motion: an install copy writes the clipboard but hires nobody', async ({
  browser,
}) => {
  const context = await browser.newContext({
    reducedMotion: 'reduce',
    permissions: ['clipboard-read', 'clipboard-write'],
  });
  const page = await context.newPage();
  const errors = watchErrors(page);
  await page.addInitScript(() => sessionStorage.setItem('pix-booted', '1'));
  await page.goto('./');
  await expect(page.locator('.backdrop.is-live')).not.toBeAttached();
  await page.evaluate(() => {
    (window as unknown as { __hired: string[] }).__hired = [];
    document.addEventListener('pix:hired', (e) =>
      (window as unknown as { __hired: string[] }).__hired.push(
        (e as CustomEvent<{ name: string }>).detail.name
      )
    );
  });
  await page.evaluate(() =>
    document.getElementById('install')!.scrollIntoView({ block: 'center', behavior: 'instant' })
  );
  const copy = page.locator('.install__panel.is-active .install__copy');
  await copy.click();
  await expect(copy).toHaveText(/Copied|Select & copy/);
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe('brew install pixtuoid');
  await page.waitForTimeout(500); // settle window: no late/async hire lands
  expect(await page.evaluate(() => (window as unknown as { __hired: string[] }).__hired)).toEqual(
    []
  );
  expect(errors()).toEqual([]);
  await context.close();
});

test('docs pages keep the sticky nav with section links', async ({ page }) => {
  const errors = watchErrors(page);
  await page.goto('./config');
  const nav = page.locator('.nav');
  await expect(nav).not.toHaveClass(/nav--floating/);
  await expect
    .poll(() => page.evaluate(() => getComputedStyle(document.querySelector('.nav')!).position))
    .toBe('sticky');
  await expect(page.locator('.nav__section-link').first()).toBeVisible();
  expect(errors()).toEqual([]);
});

test('reduced motion stays on the still poster without errors', async ({ browser }) => {
  const context = await browser.newContext({ reducedMotion: 'reduce' });
  const page = await context.newPage();
  const errors = watchErrors(page);
  const wasmRequests: string[] = [];
  page.on('request', (r) => {
    if (r.url().includes('/wasm/')) wasmRequests.push(r.url());
  });
  await page.addInitScript(() => sessionStorage.setItem('pix-booted', '1'));
  await page.goto('./');
  await expect(page.locator('.backdrop__poster')).toBeVisible();
  // By network-idle a would-be boot would have fetched the wasm glue and
  // published __pixHire — assert neither happened.
  await page.waitForLoadState('networkidle');
  expect(wasmRequests).toEqual([]);
  await expect(page.locator('.backdrop.is-live')).not.toBeAttached();
  // Reduced motion is the ONLY path that hides the pause switch — nothing
  // auto-animates here.
  await expect(page.locator('#office-pause')).toBeHidden();
  const video = page.locator('[data-stage="agents"] video');
  await expect(video).toHaveAttribute('controls', '');
  await expect.poll(() => video.evaluate((v) => (v as HTMLVideoElement).paused)).toBe(true);
  const proofVid = page.locator('.proof__video--wide');
  await page.evaluate(() =>
    document.getElementById('proof')!.scrollIntoView({ block: 'center', behavior: 'instant' })
  );
  // The poster first: it auto-waits for the observers, so the source count below
  // is read AFTER they had their chance to hydrate.
  await expect(proofVid).toHaveAttribute('poster', /proof-poster/);
  expect(await proofVid.evaluate((v) => v.querySelectorAll('source').length)).toBe(0);
  await expect.poll(() => proofVid.evaluate((v) => (v as HTMLVideoElement).paused)).toBe(true);
  expect(errors()).toEqual([]);
  await context.close();
});

test('wasm fetch failure keeps the still poster without an uncaught error', async ({ browser }) => {
  // The pause control must stay present even so: it governs the wasm-independent ambient motion
  // (ticker/dust/clips), which a dead office must not strand uncontrollable (#456).
  const context = await browser.newContext();
  const page = await context.newPage();
  const errors = watchErrors(page);
  await page.route('**/wasm/**', (r) => r.abort());
  const wasmTried: string[] = [];
  page.on('request', (r) => {
    if (r.url().includes('/wasm/')) wasmTried.push(r.url());
  });
  await page.addInitScript(() => sessionStorage.setItem('pix-booted', '1'));
  await page.goto('./');
  // the boot is deferred to load+idle — wait until it actually attempted the fetch
  await expect.poll(() => wasmTried.length, { timeout: 15_000 }).toBeGreaterThan(0);
  await page.waitForLoadState('networkidle');
  await expect(page.locator('.backdrop__poster')).toBeVisible();
  await expect(page.locator('.backdrop.is-live')).not.toBeAttached();
  await expect(page.locator('[data-sl-onair]')).toHaveText('○ STATIC');
  const pauseBtn = page.locator('#office-pause');
  await expect(pauseBtn).toBeVisible();
  const paused = page.evaluate(
    () =>
      new Promise<boolean>((resolve) => {
        document.addEventListener('pix:paused', (e) => resolve((e as CustomEvent).detail.paused), {
          once: true,
        });
      })
  );
  await pauseBtn.click();
  expect(await paused).toBe(true);
  await expect(pauseBtn).toHaveAttribute('aria-pressed', 'true');
  expect(errors().filter((e) => !e.includes('Failed to load resource'))).toEqual([]);
  await context.close();
});

test('a transient wasm-fetch drop self-heals: the office still goes live via retry', async ({
  browser,
}) => {
  // Abort ONLY the FIRST pixtuoid_web_bg.wasm request (the big binary, the likeliest drop) and let every retry through:
  // one dropped fetch used to reject the shared __pixWasm promise and strand the office until a reload.
  const context = await browser.newContext();
  const page = await context.newPage();
  const errors = watchErrors(page);
  let wasmHits = 0;
  let abortedFirst = false;
  await page.route('**/pixtuoid_web_bg.wasm', (route) => {
    wasmHits += 1;
    if (wasmHits === 1) {
      abortedFirst = true;
      return route.abort();
    }
    return route.continue();
  });
  await page.addInitScript(() => sessionStorage.setItem('pix-booted', '1'));
  await page.goto('./');
  await expect(page.locator('.backdrop.is-live')).toBeAttached({ timeout: 20_000 });
  await expect(page.locator('[data-sl-onair]')).toHaveText('● LIVE', { timeout: 10_000 });
  expect(abortedFirst).toBe(true);
  expect(wasmHits).toBeGreaterThan(1);
  expect(errors().filter((e) => !e.includes('Failed to load resource'))).toEqual([]);
  await context.close();
});

test('#671 cross-trigger recovery: after the hero exhausts its retries, a later VIBING boot re-attempts and comes up', async ({
  page,
}) => {
  // Fail the hero's 3 attempts (initial + 2 retries) — VIBING is below the fold,
  // so it only boots after the network recovers, off the nulled shared promise.
  const errors = watchErrors(page);
  let wasmHits = 0;
  await page.route('**/pixtuoid_web_bg.wasm', (route) => {
    wasmHits += 1;
    if (wasmHits <= 3) return route.abort();
    return route.continue();
  });
  await page.addInitScript(() => sessionStorage.setItem('pix-booted', '1'));
  await page.goto('./');
  await expect
    .poll(() => page.evaluate(() => window.__pixWasm === null), { timeout: 15_000 })
    .toBe(true);
  await expect(page.locator('.backdrop.is-live')).not.toBeAttached();
  await page.evaluate(() =>
    document.getElementById('studio')!.scrollIntoView({ block: 'center', behavior: 'instant' })
  );
  await expect
    .poll(
      () =>
        page.evaluate(() => {
          const c = document.querySelector('[data-vibing-canvas]') as HTMLCanvasElement | null;
          if (!c) return false;
          const d = c.getContext('2d')!.getImageData(0, 0, c.width, c.height).data;
          return d.some((v) => v !== 0);
        }),
      { timeout: 15_000 }
    )
    .toBe(true);
  expect(await page.evaluate(() => window.__pixWasm !== null)).toBe(true);
  expect(wasmHits).toBeGreaterThan(3);
  expect(errors().filter((e) => !e.includes('Failed to load resource'))).toEqual([]);
});

test('key vocabulary: digits ride globally, typing surfaces stay guarded, t keeps its gate', async ({
  page,
}) => {
  await gotoLive(page);
  await page.keyboard.press('3');
  await expect(page.locator('[data-lift-digit]')).toHaveText('3F', { timeout: 10_000 });
  await page.locator('#office-pause').focus();
  await page.keyboard.press('1');
  await expect(page.locator('[data-lift-digit]')).toHaveText('1F', { timeout: 10_000 });
  await page.evaluate(() => {
    const inp = document.createElement('input');
    inp.id = 'e2e-typing-probe';
    document.body.appendChild(inp);
    inp.focus();
  });
  await page.keyboard.press('3');
  await expect(page.locator('[data-lift-digit]')).toHaveText('1F');
  await page.evaluate(() => document.getElementById('e2e-typing-probe')!.remove());
  await page.locator('#office-pause').focus();
  await page.evaluate(() => document.documentElement.style.removeProperty('--coral'));
  await page.keyboard.press('t');
  expect(
    await page.evaluate(() => document.documentElement.style.getPropertyValue('--coral'))
  ).toBe('');
});

test('statusline install chip is a link that jumps to Install (href, scroll, keyboard)', async ({
  page,
}) => {
  const errors = watchErrors(page);
  await gotoLive(page);
  const link = page.locator('#sl-install [data-sl-install-link]');
  expect(await link.evaluate((el) => el.tagName)).toBe('A');
  await expect(link).toHaveAttribute('href', '#install');
  await expect(link).toHaveAttribute('aria-label', 'Jump to the install section');
  await expect(page.locator('#sl-install .sl__copy-label')).toHaveText('install');
  await expect(page.locator('#sl-install .sl__stars')).toBeVisible();

  await link.click();
  await expect
    .poll(() =>
      page.evaluate(() => document.getElementById('install')!.getBoundingClientRect().top)
    )
    .toBeLessThan(50);
  expect(await page.evaluate(() => document.activeElement && document.activeElement.id)).toBe(
    'install'
  );

  await page.evaluate(() => window.scrollTo({ top: 0, behavior: 'instant' }));
  await link.focus();
  await page.keyboard.press('Enter');
  await expect
    .poll(() =>
      page.evaluate(() => document.getElementById('install')!.getBoundingClientRect().top)
    )
    .toBeLessThan(50);
  expect(errors()).toEqual([]);
});

test('statusline install chip: reduced motion jumps instantly (no smooth scroll)', async ({
  browser,
}) => {
  const context = await browser.newContext({ reducedMotion: 'reduce' });
  const page = await context.newPage();
  const errors = watchErrors(page);
  await page.addInitScript(() => sessionStorage.setItem('pix-booted', '1'));
  await page.goto('./');
  await page.locator('#sl-install [data-sl-install-link]').click();
  await expect
    .poll(() =>
      page.evaluate(() => document.getElementById('install')!.getBoundingClientRect().top)
    )
    .toBeLessThan(50);
  expect(errors()).toEqual([]);
  await context.close();
});

test('statusline install chip on mobile: label stays readable at rest, flash swaps to the glyph', async ({
  page,
  context,
}) => {
  await context.grantPermissions(['clipboard-write']);
  const errors = watchErrors(page);
  await page.addInitScript(() => {
    (window as unknown as { __chipPulses: number }).__chipPulses = 0;
    document.addEventListener('animationstart', (e) => {
      if ((e as AnimationEvent).animationName === 'chip-pulse') {
        (window as unknown as { __chipPulses: number }).__chipPulses++;
      }
    });
  });
  await gotoLive(page);
  await page.setViewportSize({ width: 375, height: 800 });
  const chip = page.locator('#sl-install .sl__copy');
  const label = page.locator('#sl-install .sl__copy-label');
  const flashIcon = page.locator('#sl-install .sl__copy-icon-flash');
  await expect(chip).not.toHaveClass(/is-flash/);
  await expect(flashIcon).toBeHidden();
  await expect(label).toBeVisible();
  await expect(label).toHaveText('install');

  await page.evaluate(() =>
    document.getElementById('install')!.scrollIntoView({ block: 'center', behavior: 'instant' })
  );
  await page.locator('.install__panel.is-active .install__copy').click();

  await expect(chip).toHaveClass(/is-flash/);
  await expect(flashIcon).toBeVisible();
  await expect(label).toBeHidden();
  await expect(page.locator('#sl-install .sl__copy-label')).toHaveText('install', {
    timeout: 8_000,
  });
  await expect(chip).not.toHaveClass(/is-flash/);
  await expect(flashIcon).toBeHidden();
  // ONE pulse: only the hire-receipt flash fires (the chip has no copy flash).
  expect(
    await page.evaluate(() => (window as unknown as { __chipPulses: number }).__chipPulses)
  ).toBe(1);
  expect(errors()).toEqual([]);
});

test('statusline install chip: the ★ star segment renders the pinned count, never a literal null/undefined', async ({
  page,
}) => {
  // `just site-e2e` and CI both build with GH_STARS_E2E — a build made without it fails here, and the broad shape
  // guard keeps the stringified-null class red.
  await gotoLive(page);
  const stars = page.locator('#sl-install .sl__stars');
  await expect(stars).toBeVisible();
  await expect(stars).toHaveText(`★ ${E2E_STAR_COUNT}`);
  await expect(stars).toHaveText(/^\s*★\s*\d+\s*$/);
});

test('WCAG 2.1.4: the statusline keys toggle turns the digit shortcuts off, then back on', async ({
  page,
}) => {
  await gotoLive(page);
  await page.keyboard.press('2');
  await expect(page.locator('[data-lift-digit]')).toHaveText('2F', { timeout: 10_000 });
  await page.locator('[data-floor-toggle]').click();
  const keysToggle = page.locator('[data-keys-toggle]');
  await keysToggle.click();
  await expect(keysToggle).toHaveAttribute('aria-checked', 'false');
  await page.keyboard.press('3');
  await expect(page.locator('[data-lift-digit]')).toHaveText('2F');
  expect(await page.evaluate(() => localStorage.getItem('pix-keys'))).toBe('off');
  await keysToggle.click();
  await expect(keysToggle).toHaveAttribute('aria-checked', 'true');
  await page.keyboard.press('3');
  await expect(page.locator('[data-lift-digit]')).toHaveText('3F', { timeout: 10_000 });
});

test('the clock forces night after hours and clears on an explicit theme act', async ({ page }) => {
  await page.clock.setFixedTime(new Date('2026-01-01T23:00:00'));
  await page.emulateMedia({ colorScheme: 'light' }); // the clock must win over a light OS
  await page.addInitScript(() => sessionStorage.setItem('pix-booted', '1'));
  await page.goto('./');
  await page.evaluate(() => localStorage.removeItem('pix-theme'));
  await page.reload();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'night');
  await expect(page.locator('html')).toHaveAttribute('data-clock-night', '1');
  await page.locator('#theme-toggle').click();
  await expect(page.locator('html')).not.toHaveAttribute('data-clock-night', '1');
  await page.clock.setFixedTime(new Date('2026-01-01T12:00:00'));
  await page.evaluate(() => localStorage.removeItem('pix-theme'));
  await page.reload();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'day');
});

test('first visit: boot intro auto-runs, reveals the page, seeds the gate', async ({ page }) => {
  await page.goto('./'); // NO pix-booted seed — the real first visit
  await expect(page.locator('#boot')).toBeVisible();
  await expect(page.locator('html')).not.toHaveAttribute('data-booting', '1', { timeout: 10_000 });
  await expect.poll(() => page.evaluate(() => sessionStorage.getItem('pix-booted'))).toBe('1');
  expect(
    await page.evaluate(() => performance.getEntriesByName('pixtuoid-revealed', 'mark').length)
  ).toBe(1);
  expect(await page.evaluate(() => document.getElementById('main')!.hasAttribute('inert'))).toBe(
    false
  );
  // opacity:0 still counts as "visible" to Playwright — assert the CLASS.
  await expectSectionReveal(page, 'install');
  await page.reload();
  await expect(page.locator('#boot')).not.toBeVisible();
  await expectSectionReveal(page, 'install');
});

test('the reveal roll survives a main-thread stall instead of snapping past it', async ({
  page,
}) => {
  // Safari blocks the main thread for ~1.3-1.5s right after a first visit settles, inside its own tab-snapshot IPC lock, and a
  // wall-clock ramp keeps advancing while nothing paints — hence a roll driven by PAINTED frames.
  const errors = watchErrors(page);
  await page.goto('./'); // real first visit — the boot path is the only one that rolls
  await expect(page.locator('.backdrop')).toHaveClass(/\bis-live\b/, { timeout: 15_000 });
  const settled = () =>
    page.evaluate(() => !!document.getElementById('office-overlay')?.classList.contains('is-on'));
  expect(await settled()).toBe(false);
  // Block the main thread for a full REVEAL_MS the way the snapshot IPC does.
  await page.evaluate(() => {
    const until = performance.now() + 1600;
    while (performance.now() < until) {
      /* synchronous stall — no frames can paint */
    }
  });
  expect(
    await settled(),
    'a clock-driven ramp is now past REVEAL_MS and would have snapped to the settled office; a frame-driven one has barely advanced'
  ).toBe(false);
  await expect.poll(settled, { timeout: 10_000 }).toBe(true);
  expect(errors()).toEqual([]);
});

test('un-reducing motion mid-session rolls the office in again, it does not snap', async ({
  page,
}) => {
  // Frame-accumulated progress that SURVIVES the de-live leaves rt >= 1, so the
  // office blits its settled frame and SNAPS. Nothing else here un-reduces.
  const errors = watchErrors(page);
  const captionsOn = () =>
    page.evaluate(() => !!document.getElementById('office-overlay')?.classList.contains('is-on'));
  await page.goto('./');
  await expect(page.locator('.backdrop')).toHaveClass(/\bis-live\b/, { timeout: 15_000 });
  // let the first roll finish, so accumulated progress is past REVEAL_MS
  await expect.poll(captionsOn, { timeout: 15_000 }).toBe(true);
  await page.emulateMedia({ reducedMotion: 'reduce' });
  await expect(page.locator('.backdrop')).not.toHaveClass(/\bis-live\b/, { timeout: 5_000 });
  await page.emulateMedia({ reducedMotion: 'no-preference' });
  await expect(page.locator('.backdrop')).toHaveClass(/\bis-live\b/, { timeout: 15_000 });
  expect(await captionsOn()).toBe(false);
  await expect.poll(captionsOn, { timeout: 15_000 }).toBe(true);
  expect(errors()).toEqual([]);
});

test('the office still goes live on a device that never meets the frame budget', async ({
  page,
}) => {
  // The roll's wait for a few on-budget frames is a COURTESY, never a precondition: a machine that can never
  // meet the budget must still get an office rather than sit on the cover forever.
  const errors = watchErrors(page);
  await page.addInitScript(() => {
    // Only ONCE the engine is up — starving the wasm boot itself never reaches the readiness gate this pins.
    const raf = window.requestAnimationFrame.bind(window);
    window.requestAnimationFrame = (cb) =>
      raf((t) => {
        if ((window as unknown as { __pixEngineReady?: boolean }).__pixEngineReady) {
          const until = performance.now() + 70;
          while (performance.now() < until) {
            /* every frame is a hitch */
          }
        }
        cb(t);
      });
  });
  await page.goto('./');
  await expect(page.locator('.backdrop')).toHaveClass(/\bis-live\b/, { timeout: 20_000 });
  expect(errors()).toEqual([]);
});

test('a keypress during the Level-2 engine hold force-settles the splash immediately', async ({
  page,
}) => {
  const errors = watchErrors(page);
  // Hang the wasm fetch forever (never fulfilled/aborted) so __pixEngineReady never resolves and an unforced finish() holds the full cap.
  await page.route('**/wasm/**', () => {});
  await page.goto('./'); // real first visit — no pix-booted seed
  await expect(page.locator('#boot')).toBeVisible();
  // Last line lit = the moment finish() runs and enters the waitForEngine hold.
  await expect(page.locator('.boot__line').last()).toHaveClass(/\bin\b/, { timeout: 5_000 });
  await page.keyboard.press('Space');
  await expect(
    page.locator('html'),
    'the splash must clear almost immediately — nowhere near the MAX_ENGINE_WAITS cap'
  ).not.toHaveAttribute('data-booting', '1', { timeout: 700 });
  expect(await page.evaluate(() => document.getElementById('main')!.hasAttribute('inert'))).toBe(
    false
  );
  expect(errors()).toEqual([]);
});

test('first visit on an office-less page lifts the splash promptly (no engine-gate hang)', async ({
  page,
}) => {
  // window.__pixEngineReady is set ONLY by OfficeBackdrop (index-only), so an office-less page's Level-2 gate must fall back to the flat delay.
  const errors = watchErrors(page);
  await page.goto('./architecture/'); // real first visit (no pix-booted), no OfficeBackdrop
  await expect(page.locator('#boot')).toBeVisible();
  await expect(page.locator('#office-live')).toHaveCount(0);
  await expect(
    page.locator('html'),
    '~2.1s nominal vs the unguarded gate hanging past 5s — 3s separates them'
  ).not.toHaveAttribute('data-booting', '1', { timeout: 3_000 });
  expect(errors()).toEqual([]);
});

test('first visit: splash displays 4-line log with per-line dwell (~390ms)', async ({ page }) => {
  const errors = watchErrors(page);
  // Test on docs page (no office, no engine wait) for pure splash-timing measurement.
  await page.goto('./config/'); // NO pix-booted seed — the real first visit
  await expect(page.locator('#boot')).toBeVisible();
  await expect(page.locator('#boot .boot__log')).toContainText('pixtuoid');
  await expect(page.locator('#boot .boot__log')).toContainText('booting office');
  await expect(page.locator('#boot .boot__log')).toContainText('loading themes');
  await expect(page.locator('#boot .boot__log')).toContainText(
    `${sourcesData.length} CLIs connected`
  );
  // ~2.1s nominal: 4 lines at the 390ms dwell, plus the 460ms fade.
  await expect(page.locator('html')).not.toHaveAttribute('data-booting', '1', {
    timeout: 3_000,
  });
  await expect.poll(() => page.evaluate(() => sessionStorage.getItem('pix-booted'))).toBe('1');
  expect(errors()).toEqual([]);
});

test('theme chain: saved choice, URL override, toggle persist, Escape restore, system dark', async ({
  page,
}) => {
  // Only the boot gate goes in addInitScript — an init-script THEME seed would
  // re-run on every navigation and clobber the later steps' seeds.
  await page.addInitScript(() => sessionStorage.setItem('pix-booted', '1'));
  await page.goto('./');
  await page.evaluate(() => localStorage.setItem('pix-theme', 'dracula'));
  await page.reload();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'dracula');
  await expect(page.locator('meta[name="theme-color"]')).toHaveAttribute('content', '#282a36');
  await page.goto('./?theme=night');
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'night');
  // Seed 'day' so the flip lands 'night' regardless of the wall clock.
  await page.evaluate(() => localStorage.setItem('pix-theme', 'day'));
  await page.goto('./');
  await expect(page.locator('.nav__mark')).toHaveAttribute('src', /favicon-32\.png$/);
  await page.locator('#theme-toggle').click();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'night');
  await expect(page.locator('.nav__mark')).toHaveAttribute('src', /favicon-32-night\.png$/);
  await expect(page.locator('.footer__mark')).toHaveAttribute('src', /favicon-32-night\.png$/);
  // Back to day proves the swap with teeth (the night filename only appears if
  // syncBrand ran), then back to night for the persistence checks below.
  await page.locator('#theme-toggle').click();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'day');
  await expect(page.locator('.nav__mark')).toHaveAttribute('src', /favicon-32\.png$/);
  await expect(page.locator('.footer__mark')).toHaveAttribute('src', /favicon-32\.png$/);
  await page.locator('#theme-toggle').click();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'night');
  expect(await page.evaluate(() => localStorage.getItem('pix-theme'))).toBe('night');
  await expect(page.locator('#theme-toggle .nav__toggle-icon')).toHaveText('☀️');
  await expect(page.locator('#theme-toggle')).toHaveAttribute('aria-label', 'Switch to day');
  await page.reload();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'night');
  await expect(page.locator('#theme-toggle .nav__toggle-icon')).toHaveText('☀️');
  await page.evaluate(() => localStorage.setItem('pix-theme', 'dracula'));
  await page.reload();
  await page.keyboard.press('t');
  await expect
    .poll(() => page.evaluate(() => document.documentElement.style.getPropertyValue('--coral')))
    .not.toBe('');
  await page.keyboard.press('Escape');
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'dracula');
  await expect
    .poll(() => page.evaluate(() => document.documentElement.style.getPropertyValue('--coral')))
    .toBe('');
  // No saved pick + a dark scheme lands 'night'; after-hours clocks land night
  // too, so this is TZ-proof.
  await page.emulateMedia({ colorScheme: 'dark' });
  await page.evaluate(() => localStorage.removeItem('pix-theme'));
  await page.reload();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'night');
});

test('install: the method switcher is APG tabs — one tab stop, arrows select', async ({ page }) => {
  await page.addInitScript(() => sessionStorage.setItem('pix-booted', '1'));
  await page.goto('./');
  const tabs = page.locator('.install__tabs[role="tablist"] [role="tab"]');
  const n = await tabs.count();
  expect(n).toBeGreaterThan(1);
  await expect(page.locator('.install__tab[tabindex="0"]')).toHaveCount(1);
  await tabs.first().focus();
  await page.keyboard.press('ArrowRight');
  await expect(tabs.nth(1)).toBeFocused();
  await expect(tabs.nth(1)).toHaveAttribute('aria-selected', 'true');
  await expect(tabs.nth(1)).toHaveAttribute('tabindex', '0');
  await expect(tabs.first()).toHaveAttribute('tabindex', '-1');
  const panel = await tabs.nth(1).getAttribute('aria-controls');
  await expect(page.locator(`#${panel}[role="tabpanel"]`)).toBeVisible();
  await page.keyboard.press('ArrowLeft');
  await page.keyboard.press('ArrowLeft');
  await expect(tabs.nth(n - 1)).toBeFocused(); // wraps
  await page.keyboard.press('Home');
  await expect(tabs.first()).toHaveAttribute('aria-selected', 'true');
});

test('install: tabs swap panels and both clipboard branches deliver', async ({ page, context }) => {
  await context.grantPermissions(['clipboard-read', 'clipboard-write']);
  await page.addInitScript(() => sessionStorage.setItem('pix-booted', '1'));
  await page.goto('./'); // no live-office wait — tabs/copy are wasm-independent
  await page.locator('.install__tab[data-tab="cargo"]').click();
  await expect(page.locator('.install__tab[data-tab="cargo"]')).toHaveAttribute(
    'aria-selected',
    'true'
  );
  await expect(page.locator('#install-panel-cargo')).toBeVisible();
  await expect(page.locator('#install-panel-brew')).toBeHidden();
  const copy = page.locator('.install__panel.is-active .install__copy');
  await copy.click();
  await expect(copy).toHaveText('Copied ✓');
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(
    await copy.getAttribute('data-copy')
  );
  // Force the manual branch on a fresh load (brew is the default active panel).
  await page.addInitScript(() =>
    Object.defineProperty(navigator, 'clipboard', { value: undefined })
  );
  await page.reload();
  const brewCopy = page.locator('.install__panel.is-active .install__copy');
  await brewCopy.click();
  await expect(brewCopy).toHaveText('Select & copy');
  expect(await page.evaluate(() => String(getSelection()))).toContain('brew install');
});

test('showcase studio: deep-links tune, dial and chips swap hydrated stages, the clip plays', async ({
  page,
}) => {
  const errors = watchErrors(page);
  await page.addInitScript(() => sessionStorage.setItem('pix-booted', '1'));
  await page.goto('./#showcase-spaces');
  await expect(page.locator('[data-stage="spaces"]')).toBeVisible();
  await expect(page.locator('button.mon[data-ch="spaces"]')).toHaveAttribute(
    'aria-selected',
    'true'
  );
  await expect(page.locator('[data-stage="spaces"] img.terminal__screen')).toHaveAttribute(
    'src',
    /space_/
  );
  await page.evaluate(() => {
    location.hash = '#showcase-dashboard';
  });
  await expect(page.locator('[data-stage="dashboard"]')).toBeVisible();
  await page.locator('button.mon[data-ch="spaces"]').click();
  await expect(page.locator('[data-stage="spaces"]')).toBeVisible();
  await expect(page.locator('[data-stage="dashboard"]')).toBeHidden();
  await expect(page.locator('button.mon[data-ch="spaces"]')).toHaveAttribute(
    'aria-selected',
    'true'
  );
  await expect(page).toHaveURL(/#showcase-spaces$/);
  const chip = page.locator('[data-stage="spaces"] .osd__chip', { hasText: 'Pantry' });
  await chip.click();
  await expect(chip).toHaveAttribute('aria-pressed', 'true');
  await expect(page.locator('[data-stage="spaces"] img.terminal__screen')).toHaveAttribute(
    'src',
    /space_pantry\.png/
  );
  // Muted autoplay is gesture-free in chromium, so the clip plays inline.
  await page.locator('button.mon[data-ch="agents"]').click();
  await page.evaluate(() =>
    document.getElementById('studio')!.scrollIntoView({ block: 'center', behavior: 'instant' })
  );
  await expect
    .poll(() =>
      page
        .locator('[data-stage="agents"] video')
        .evaluate((v) => !(v as HTMLVideoElement).paused && !v.hasAttribute('controls'))
    )
    .toBe(true);
  const clipPaused = () =>
    page.locator('[data-stage="agents"] video').evaluate((v) => (v as HTMLVideoElement).paused);
  await page.evaluate(() =>
    document.dispatchEvent(new CustomEvent('pix:paused', { detail: { paused: true } }))
  );
  await expect.poll(clipPaused).toBe(true);
  await page.evaluate(() =>
    document.dispatchEvent(new CustomEvent('pix:paused', { detail: { paused: false } }))
  );
  await expect.poll(clipPaused).toBe(false);
  expect(errors()).toEqual([]);
});

test('VIBING channel: live office paints, is pause-gated, chips drive it', async ({ page }) => {
  const errors = watchErrors(page);
  await gotoLive(page);
  // VIBING is the default channel — no dial/hash tune needed to see it.
  const stage = page.locator('[data-stage="vibing"]');
  await expect(stage).toBeVisible();
  await expect(page.locator('[data-vibing-canvas]')).toBeAttached();
  // The VIBING office is a SECOND wasm Office whose rAF loop is gated on the
  // studio scrolling into view.
  await page.evaluate(() =>
    document.getElementById('studio')!.scrollIntoView({ block: 'center', behavior: 'instant' })
  );
  const vibingShot = () =>
    page.evaluate(() =>
      (document.querySelector('[data-vibing-canvas]') as HTMLCanvasElement).toDataURL()
    );
  const vibingPainted = () =>
    page.evaluate(() => {
      const c = document.querySelector('[data-vibing-canvas]') as HTMLCanvasElement;
      const d = c.getContext('2d')!.getImageData(0, 0, c.width, c.height).data;
      return d.some((v) => v !== 0);
    });
  await expect.poll(vibingPainted, { timeout: 15_000 }).toBe(true);

  const beforeWeather = await vibingShot();
  const stormChip = page.locator('[data-stage="vibing"] .osd__chip[data-weather="storm"]');
  await stormChip.click();
  // Deterministic teeth: a frame-changed poll alone passes on ambient sprite motion regardless.
  await expect(stormChip).toHaveClass(/is-active/);
  await expect(stormChip).toHaveAttribute('aria-pressed', 'true');
  await expect.poll(vibingShot, { timeout: 5_000 }).not.toBe(beforeWeather);

  const coralBefore = await page.evaluate(() =>
    getComputedStyle(document.documentElement).getPropertyValue('--coral')
  );
  const themeChip = page.locator('[data-stage="vibing"] .osd__chip[data-theme="cyberpunk"]');
  await themeChip.click();
  await expect(themeChip).toHaveClass(/is-active/);
  await expect(themeChip).toHaveAttribute('aria-pressed', 'true');
  await expect
    .poll(() =>
      page.evaluate(() => getComputedStyle(document.documentElement).getPropertyValue('--coral'))
    )
    .not.toBe(coralBefore);
  await expect(stormChip).toHaveClass(/is-active/); // weather group untouched by the theme retint

  const timeInput = stage.locator('[data-vibing-time]');
  const timeWrap = stage.locator('.vibing__time');
  const setHour = (h: number) =>
    timeInput.evaluate((el, v) => {
      (el as HTMLInputElement).value = String(v);
      el.dispatchEvent(new Event('input', { bubbles: true }));
    }, h);
  const beforeSlider = await vibingShot();
  await setHour(6); // 06:00 — inside the engine's [5,20) sun window → day
  await expect(stage.locator('[data-vibing-time-label]')).toHaveText('06:00');
  await expect(timeInput).toHaveAttribute('aria-valuetext', '06:00');
  await expect(timeWrap).toHaveAttribute('data-phase', 'day');
  await expect.poll(vibingShot, { timeout: 5_000 }).not.toBe(beforeSlider);
  await setHour(22); // 22:00 — past sunset (≥ 20) → the moon branch
  await expect(stage.locator('[data-vibing-time-label]')).toHaveText('22:00');
  await expect(timeInput).toHaveAttribute('aria-valuetext', '22:00');
  await expect(timeWrap).toHaveAttribute('data-phase', 'night');

  const pauseBtn = page.locator('#office-pause');
  await pauseBtn.click();
  await expect(pauseBtn).toHaveAttribute('aria-pressed', 'true');
  const frozen = await vibingShot();
  await page.waitForTimeout(400); // >12 would-be frames at the 33ms cap
  expect(await vibingShot()).toBe(frozen);
  await pauseBtn.click();
  await expect(pauseBtn).toHaveAttribute('aria-pressed', 'false');
  await expect.poll(vibingShot, { timeout: 5_000 }).not.toBe(frozen);
  expect(errors()).toEqual([]);
});

test('nav menus + docs: dropdown, TOC scrollspy, 404, mobile burger', async ({ page, browser }) => {
  const errors = watchErrors(page);
  await page.goto('./config#themes'); // arrival-by-hash: the rail lights unscrolled
  await expect(page.locator('[data-toc-link="themes"]')).toHaveAttribute(
    'aria-current',
    'location'
  );
  const btn = page.locator('#docs-btn');
  await btn.click();
  await expect(page.locator('#docs-menu')).toHaveClass(/is-open/);
  await expect(btn).toHaveAttribute('aria-expanded', 'true');
  await page.locator('#docs-menu a').first().focus(); // focus INSIDE, or the return branch is skipped
  await page.keyboard.press('Escape');
  await expect(page.locator('#docs-menu')).not.toHaveClass(/is-open/);
  await expect(btn).toBeFocused();
  // The anchored heading must clear the 60px sticky nav.
  await page.locator('[data-toc-link="logging--troubleshooting"]').click();
  await expect(page.locator('[data-toc-link="logging--troubleshooting"]')).toHaveAttribute(
    'aria-current',
    'location'
  );
  await expect
    .poll(() =>
      page.evaluate(
        () => document.getElementById('logging--troubleshooting')!.getBoundingClientRect().top
      )
    )
    .toBeGreaterThan(60);
  // Park a heading at 20% viewport — inside the -15%/-75% reading band.
  await page.evaluate(() => {
    const h = document.getElementById('themes')!;
    window.scrollTo({
      top: h.getBoundingClientRect().top + window.scrollY - window.innerHeight * 0.2,
      behavior: 'instant',
    });
  });
  await expect(page.locator('[data-toc-link="themes"]')).toHaveAttribute(
    'aria-current',
    'location'
  );
  await page.goto('./no-such-desk');
  await expect(page.locator('.lost h1')).toContainText('Session not');
  await expect
    .poll(() =>
      page
        .locator('.lost__scene .terminal__screen')
        .evaluate((img) => (img as HTMLImageElement).naturalWidth)
    )
    .toBeGreaterThan(0);
  await expect(page.locator('.lost__cta .btn-primary')).toHaveAttribute('href', '/');
  expect(errors().filter((e) => !e.includes('Failed to load resource'))).toEqual([]);
  const ctx = await browser.newContext({ viewport: { width: 480, height: 800 } });
  const m = await ctx.newPage();
  await m.addInitScript(() => sessionStorage.setItem('pix-booted', '1'));
  await m.goto('./config');
  await m.locator('#nav-burger').click();
  await expect(m.locator('#nav-links')).toHaveClass(/is-open/);
  await expect(m.locator('#nav-burger')).toHaveAttribute('aria-expanded', 'true');
  await m.locator('#nav-links a').first().focus();
  await m.keyboard.press('Escape');
  await expect(m.locator('#nav-links')).not.toHaveClass(/is-open/);
  await expect(m.locator('#nav-burger')).toBeFocused();
  await ctx.close();
});

test('landing fixed chrome: floating nav, statusline readouts, floor popover', async ({ page }) => {
  await page.addInitScript(() => sessionStorage.setItem('pix-booted', '1'));
  await page.goto('./'); // no live-office wait — everything here is wasm-independent
  // The load-bearing half of the floating variant: no live blur filter over a
  // 30fps canvas (the compositor-flicker class).
  await expect(page.locator('.nav')).toHaveClass(/nav--floating/);
  expect(
    await page.evaluate(() => getComputedStyle(document.querySelector('.nav')!).backdropFilter)
  ).toBe('none');
  await expect(page.locator('[data-sl-lights]')).toHaveText(/lights \d+%/);
  await expect(page.locator('[data-sl-clock]')).toHaveText(/^\d{2}:\d{2} (day|night)$/);
  const toggle = page.locator('[data-floor-toggle]');
  await toggle.click();
  await expect(toggle).toHaveAttribute('aria-expanded', 'true');
  await expect(page.locator('#sl-floors')).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(page.locator('#sl-floors')).toBeHidden();
  await toggle.click();
  await page.locator('[data-floor-btn="1F"]').click();
  await expect(page.locator('#sl-floors')).toBeHidden();
  await expect(page.locator('[data-lift-digit]')).toHaveText('1F', { timeout: 10_000 });
});

test('no horizontal overflow at phone widths (mobile pan guard)', async ({ browser }) => {
  // `body { overflow-x: hidden }` masks the desktop scrollbar, so a full-width block whose ::before glow pokes past the viewport
  // is INVISIBLE on desktop yet PANS on mobile — and a pseudo-element dodges every querySelectorAll('*') scan.
  // What overflows instead scrolls inside a doc's own scroller, which config/rehype-scroll-focus.mjs makes a tab stop.
  let scrollersSeen = 0;
  for (const [path, width] of [
    ['./', 320], // iPhone SE — the narrowest supported
    ['./', 360],
    ['./', 390],
    ['./', 430],
    ['./', 768],
    ['./config', 390],
    ['./config', 768],
    ['./architecture', 375],
    ['./contributing', 375],
    ['./architecture', 320], // #503's wide ASCII pre
    ['./architecture', 768],
    ['./contributing', 320], // #503's long link: `crates/pixtuoid-core/tests/AGENTS.md`
  ] as const) {
    const context = await browser.newContext({
      viewport: { width, height: 820 },
      isMobile: true,
      hasTouch: true,
    });
    const page = await context.newPage();
    await page.addInitScript(() => sessionStorage.setItem('pix-booted', '1'));
    await page.goto(path);
    // The reported symptom is a drag at the BOTTOM — measure there, after any late layout settles.
    await page.evaluate(() => window.scrollTo(0, document.documentElement.scrollHeight));
    const { scrollW, clientW, innerW } = await page.evaluate(() => ({
      scrollW: document.documentElement.scrollWidth,
      clientW: document.documentElement.clientWidth,
      innerW: window.innerWidth,
    }));
    expect(
      scrollW,
      `${path} at ${width}px is ${scrollW - clientW}px wider than the viewport (horizontal pan)`
    ).toBeLessThanOrEqual(clientW);
    expect(
      innerW,
      `${path} at ${width}px: window.innerWidth expanded to ${innerW}px (${innerW - width}px past the device width — over-wide content grew the emulated viewport)`
    ).toBeLessThanOrEqual(width);
    const scrollers = await page.evaluate(() =>
      [...document.querySelectorAll('article.prose *')]
        .filter(
          (e) => /auto|scroll/.test(getComputedStyle(e).overflowX) && e.scrollWidth > e.clientWidth
        )
        .map((e) => ({ tag: e.tagName.toLowerCase(), tabIndex: (e as HTMLElement).tabIndex }))
    );
    scrollersSeen += scrollers.length;
    for (const s of scrollers) {
      expect(s.tabIndex, `${path} at ${width}px: a ${s.tag} scroller is not a tab stop`).toBe(0);
    }
    await context.close();
  }
  expect(scrollersSeen, 'no doc scroller to hold to the tab-stop check').toBeGreaterThan(0);
});

test('the hero copy clears the floating nav at phone viewports (vertical overlap guard)', async ({
  browser,
}) => {
  // The hero is min-height:100svh with the copy BOTTOM-anchored, so a copy that outgrows the viewport gives way at its TOP and the
  // eyebrow slides under the floating nav. reducedMotion pins `rise` to its SETTLED position — a mid-animation read masks regressions.
  for (const [width, height] of [
    [402, 700],
    [360, 640],
    [402, 874],
  ] as const) {
    const context = await browser.newContext({
      viewport: { width, height },
      isMobile: true,
      hasTouch: true,
      reducedMotion: 'reduce',
    });
    const page = await context.newPage();
    await page.addInitScript(() => sessionStorage.setItem('pix-booted', '1'));
    await page.goto('./');
    const { navBottom, eyebrowTop } = await page.evaluate(() => ({
      navBottom: document.querySelector('.nav .nav__inner')!.getBoundingClientRect().bottom,
      eyebrowTop: document.querySelector('.hero__copy .eyebrow')!.getBoundingClientRect().top,
    }));
    expect(
      eyebrowTop,
      `at ${width}x${height} the hero eyebrow (top ${eyebrowTop}px) sits under the floating nav (bottom ${navBottom}px)`
    ).toBeGreaterThanOrEqual(navBottom);
    await context.close();
  }
});

test('docs-table code cells render single-line (column-collapse guard)', async ({ browser }) => {
  // `.prose :not(pre) > code`'s overflow-wrap:anywhere feeds its soft-wrap opportunities into MIN-CONTENT intrinsic sizing (unlike
  // break-word), so table auto-layout crushed the /config Key column to ~1ch. The pan guard above never sees it: a collapse doesn't widen the page.
  const context = await browser.newContext({
    viewport: { width: 390, height: 820 },
    isMobile: true,
    hasTouch: true,
  });
  const page = await context.newPage();
  await page.addInitScript(() => sessionStorage.setItem('pix-booted', '1'));
  await page.goto('./config');
  const cells = await page.evaluate(() => {
    const code = [...document.querySelectorAll('.prose table th code, .prose table td code')];
    return {
      total: code.length,
      wrapped: code.filter((c) => c.getClientRects().length > 1).map((c) => c.textContent),
    };
  });
  expect(
    cells.total,
    'the /config tables rendered no code cells — selector drifted?'
  ).toBeGreaterThan(0);
  expect(cells.wrapped, 'code tokens inside table cells wrapped mid-token').toEqual([]);
  await context.close();
});

test('text over the live office carries its own scrim (.text-scrim)', async ({ page }) => {
  await gotoLive(page);
  // The hero copy is deliberately BARE (no plate): legibility comes from the --office-ink tokens, graded by the WCAG test below.
  const heroBg = await page.evaluate(
    () => getComputedStyle(document.querySelector('.hero .statement-sub')!).backgroundColor
  );
  expect(heroBg).toBe('rgba(0, 0, 0, 0)');
  const ghostBg = await page.evaluate(
    () => getComputedStyle(document.querySelector('.hero__ghost')!).backgroundColor
  );
  expect(ghostBg).toBe('rgba(0, 0, 0, 0)');

  expect(await page.locator('.install__note.text-scrim').count()).toBe(1);
  // The roster rows stay BARE (user verdict) — the plate must never come back.
  expect(await page.locator('#showcase .roster__row.text-scrim').count()).toBe(0);
  // Inside the card, text-scrim's negative office-margin is zeroed so the
  // plate's visible edge aligns with the tabs/command column.
  const [noteX, tabsX] = await page.evaluate(() => [
    document.querySelector('.install__note')!.getBoundingClientRect().x,
    document.querySelector('.install__tabs')!.getBoundingClientRect().x,
  ]);
  expect(Math.abs(noteX - tabsX)).toBeLessThan(1);
});

test('bare hero text clears WCAG AA at the real office composite (day + night)', async ({
  page,
}) => {
  // The hero copy has no plate, so legibility rests entirely on the ink token clearing contrast against what the office
  // ACTUALLY renders behind it — hence `paintedContrast` over real canvas pixels, not a --screen proxy.
  for (const theme of ['day', 'night'] as const) {
    await page.addInitScript((t) => {
      sessionStorage.setItem('pix-booted', '1');
      localStorage.setItem('pix-theme', t);
    }, theme);
    await page.goto('./');
    await expect(page.locator('html')).toHaveAttribute('data-theme', theme);
    await settleReveals(page);

    for (const selector of [
      '.hero .eyebrow',
      '.hero .statement-sub',
      '.hero__avail',
      '#showcase .section-head .lead',
      '#showcase .eyebrow',
      '.roster__body',
      ".dial__ch:not([aria-selected='true'])",
      '.dial__desc',
      // the PRESSED row overrides its number's colour, so the `:not()` above
      // misses it; `live` is the only marker for the one interactive demo.
      ".dial__ch[aria-selected='true'] .dial__num",
      '.dial__live',
      '#how .eyebrow',
      '#tools .section-head .lead',
      '#install .section-head .lead',
      '#amenities .eyebrow',
      '.pantry__cite',
    ]) {
      const ratio = await paintedContrast(page, selector);
      expect(
        ratio,
        `${theme} ${selector}: WCAG AA floor is 4.5:1; measured ${ratio.toFixed(2)}:1`
      ).toBeGreaterThanOrEqual(4.5);
    }
  }
});

test('plate and chip text clears WCAG AA in every theme (day + night + dracula)', async ({
  page,
}) => {
  // Two DOM-plate populations: the page's OPAQUE plates, and the TRANSLUCENT --screen chips whose ground is the office pixel behind them.
  // DRACULA is the point — visitor-reachable via `?theme=dracula`, yet the office sweep runs day+night and Lighthouse scores one pinned theme.
  const PLATE_SURFACES: Record<string, string[]> = {
    './': [
      '.terminal__title',
      '.osd__chip',
      '.stage__caption',
      '.vibing__ticks span',
      '.footer__line a.footer__coffee',
      '.footer__sep',
      '.nav__version',
    ],
    './404': ['.terminal__title'],
    './architecture': ['.prose :not(pre) > code', '.prose a > code', '.docs__pager-dir'],
  };

  for (const theme of ['day', 'night', 'dracula'] as const) {
    await page.addInitScript((t) => {
      sessionStorage.setItem('pix-booted', '1');
      localStorage.setItem('pix-theme', t);
    }, theme);
    let swept = 0;
    for (const [route, selectors] of Object.entries(PLATE_SURFACES)) {
      await page.goto(route);
      await expect(page.locator('html')).toHaveAttribute('data-theme', theme);
      await settleReveals(page);
      for (const selector of selectors) {
        const count = await page.locator(selector).count();
        expect(count, `${theme} ${route}: no ${selector} to sweep`).toBeGreaterThan(0);
        for (let i = 0; i < count; i++) {
          const ratio = await paintedContrast(page, selector, i);
          expect(
            ratio,
            `${theme} ${route} ${selector}[${i}]: WCAG AA floor is 4.5:1; measured ${ratio.toFixed(2)}:1`
          ).toBeGreaterThanOrEqual(4.5);
          swept++;
        }
      }
    }
    expect(swept, `${theme}: swept nothing`).toBeGreaterThan(0);
  }
});

test('the architecture diagram: every SVG text clears WCAG AA and every box has a 3:1 boundary, in every theme', async ({
  page,
}) => {
  // The diagram's ink is SVG `fill`, which axe / Lighthouse color-contrast never
  // read, and its plate is the last <rect> before the <text> in its own <g>
  // (node fill, subgraph header, edge-label backing) — never a CSS background.
  // Every shape in the committed diagram is a rectangle; a <path> shape
  // (diamond, stadium) would fall back to the frame and grade vacuously.
  for (const theme of ['day', 'night', 'dracula'] as const) {
    await page.addInitScript((t) => {
      sessionStorage.setItem('pix-booted', '1');
      localStorage.setItem('pix-theme', t);
    }, theme);
    await page.goto('./architecture/');
    await expect(page.locator('html')).toHaveAttribute('data-theme', theme);
    const boxes = await page.evaluate(() =>
      Array.from(
        document.querySelectorAll('.prose svg[role="img"] :is(.node, .subgraph) > rect:first-child')
      ).map((rect) => ({
        label: rect.parentElement?.getAttribute('data-label') ?? '',
        fill: getComputedStyle(rect).fill,
        stroke: getComputedStyle(rect).stroke,
      }))
    );
    expect(boxes.length, `${theme}: no diagram boxes to sweep`).toBeGreaterThan(0);
    for (const { label, fill, stroke } of boxes) {
      // A box a reader must parse needs a 3:1 boundary (WCAG 1.4.11); the
      // page's 12% --border hairline measured 1.27:1 before `.prose svg`
      // routed the library's --border hook to --fg-muted.
      const interior = parseRgb(fill).slice(0, 3) as [number, number, number];
      const [r, g, b, a] = parseRgb(stroke);
      const ratio = contrastRatio(compositeOver([r, g, b, a], interior), interior);
      expect(
        ratio,
        `${theme} diagram box "${label}": stroke vs fill floor is 3:1; measured ${ratio.toFixed(2)}:1`
      ).toBeGreaterThanOrEqual(3);
    }
    const samples = await page.evaluate(() => {
      const svg = document.querySelector('.prose svg[role="img"]');
      if (!svg) return [];
      const frame = getComputedStyle(svg).backgroundColor;
      return Array.from(svg.querySelectorAll('text')).map((text) => {
        let plate: Element | null = null;
        for (const sib of Array.from(text.parentElement?.children ?? [])) {
          if (sib === text) break;
          if (sib.tagName === 'rect') plate = sib;
        }
        return {
          label: text.textContent?.trim() ?? '',
          ink: getComputedStyle(text).fill,
          plate: plate ? getComputedStyle(plate).fill : frame,
          frame,
        };
      });
    });
    expect(samples.length, `${theme}: no diagram text to sweep`).toBeGreaterThan(0);
    for (const { label, ink, plate, frame } of samples) {
      const ground = compositeOver(
        parseRgb(plate),
        parseRgb(frame).slice(0, 3) as [number, number, number]
      );
      const [r, g, b, a] = parseRgb(ink);
      const ratio = contrastRatio(compositeOver([r, g, b, a], ground), ground);
      expect(
        ratio,
        `${theme} diagram "${label}": WCAG AA floor is 4.5:1; measured ${ratio.toFixed(2)}:1`
      ).toBeGreaterThanOrEqual(4.5);
    }
  }
});

test('hero badge row: one chip per registered source, matching the tools-table row count', async ({
  page,
}) => {
  await page.addInitScript(() => sessionStorage.setItem('pix-booted', '1'));
  await page.goto('./');
  const chips = page.locator('.hero__badges .hero__badge:not(.hero__badge--more)');
  await expect(chips).toHaveCount(supportedSources.length);
  await expect(chips.first().locator('.hero__badge-code')).toHaveText(
    supportedSources[0].badge.replace('·', '')
  );
  await expect(chips.first()).toHaveAttribute('aria-label', supportedSources[0].name);
  const more = page.locator('.hero__badge--more a');
  await expect(more).toHaveText(`${supportedSources.length} CLIs →`);
  await expect(more).toHaveAttribute('href', '#tools');
  // Chips are not copy text: a UA/inherit regression here re-introduces the
  // per-glyph arrow↔I-beam flicker.
  await expect(chips.first()).toHaveCSS('cursor', 'default');
  await expect(more).toHaveCSS('cursor', 'pointer');

  const tableRows = page.locator('.tools tbody:not(.tools__planned) tr');
  await expect(tableRows).toHaveCount(supportedSources.length);
});

test('hero badge hover expands the full CLI name in place', async ({ page }) => {
  // Raw mouse.move, NOT page.hover(): the page's html { scroll-behavior: smooth } lets hover()'s actionability pass
  // queue a smooth CDP scroll that slides the page out from under the pointer.
  await page.addInitScript(() => sessionStorage.setItem('pix-booted', '1'));
  await page.goto('./');
  // Let the hero's `rise` entrance settle first — a one-shot mouse.move to a mid-animation center lands off the settled chip.
  await page.waitForLoadState('networkidle');
  const chip = page.locator('.hero__badges .hero__badge:not(.hero__badge--more)').nth(6);
  const restBox = (await chip.boundingBox())!;
  await page.mouse.move(restBox.x + restBox.width / 2, restBox.y + restBox.height / 2);
  await expect
    .poll(async () => (await chip.boundingBox())!.width)
    .toBeGreaterThan(restBox.width + 20);
  const hoverBox = (await chip.boundingBox())!;
  expect(
    Math.abs(hoverBox.x - restBox.x),
    'the chip grows RIGHTWARD — its own left edge must not move (jitter-free)'
  ).toBeLessThan(1);
  await expect(chip.locator('.hero__badge-name')).toHaveCSS('opacity', '1');
});

test('reduced motion: the badge hover-expand is instant but still works', async ({ browser }) => {
  // Under RM the name track's transition is zeroed by global.css's UNIVERSAL clamp — Hero.astro deliberately carries NO
  // per-component arm (dead code under that !important). Motion removed ≠ feature removed: the hover rule still flips the 0fr track.
  const context = await browser.newContext({ reducedMotion: 'reduce' });
  const page = await context.newPage();
  await page.addInitScript(() => sessionStorage.setItem('pix-booted', '1'));
  await page.goto('./');
  await page.waitForLoadState('networkidle');
  const chip = page.locator('.hero__badges .hero__badge:not(.hero__badge--more)').nth(6);
  const durationSecs = await chip
    .locator('.hero__badge-name')
    .evaluate((el) => parseFloat(getComputedStyle(el).transitionDuration));
  expect(durationSecs).toBeLessThan(0.001);
  const restBox = (await chip.boundingBox())!;
  await page.mouse.move(restBox.x + restBox.width / 2, restBox.y + restBox.height / 2);
  await expect
    .poll(async () => (await chip.boundingBox())!.width)
    .toBeGreaterThan(restBox.width + 20);
  await context.close();
});

test('hero badge hues clear WCAG AA against their theme-aware chip surface (day + night)', async ({
  page,
}) => {
  // Same-hue text on same-hue ground is exactly where contrast silently dies: the cell is TINTED with the badge's own hue
  // per theme, so sweep every rendered hue in both themes, and both text pairs (code + expanded name).
  for (const theme of ['day', 'night'] as const) {
    await page.addInitScript((t) => {
      sessionStorage.setItem('pix-booted', '1');
      localStorage.setItem('pix-theme', t);
    }, theme);
    await page.goto('./');
    await expect(page.locator('html')).toHaveAttribute('data-theme', theme);

    const chips = await page.evaluate(() =>
      [...document.querySelectorAll('.hero__badge:not(.hero__badge--more)')].map((li) => ({
        codeColor: getComputedStyle(li.querySelector('.hero__badge-code')!).color,
        nameColor: getComputedStyle(li.querySelector('.hero__badge-name')!).color,
        chipBg: getComputedStyle(li).backgroundColor,
        label: li.getAttribute('aria-label'),
      }))
    );
    expect(chips.length, `${theme}: no .hero__badge chips rendered`).toBe(supportedSources.length);
    for (const { codeColor, nameColor, chipBg, label } of chips) {
      const bg = parseRgb(chipBg).slice(0, 3) as [number, number, number];
      for (const [what, color] of [
        ['code', codeColor],
        ['hover-expanded name', nameColor],
      ] as const) {
        const fg = parseRgb(color).slice(0, 3) as [number, number, number];
        const ratio = contrastRatio(fg, bg);
        expect(
          ratio,
          `${theme} "${label}" ${what}: WCAG AA floor is 4.5:1; ${color} on cell ${chipBg} measured ${ratio.toFixed(2)}:1`
        ).toBeGreaterThanOrEqual(4.5);
      }
    }

    // REST ink only — the count-link's hover ink is --coral, the same
    // transient-hover-dips-below-AA exception .hero__ghost documents.
    const moreColor = await page.evaluate(() => {
      const a = document.querySelector('.hero__badge--more a')!;
      return {
        fg: getComputedStyle(a).color,
        bg: getComputedStyle(a.closest('.hero__badge')!).backgroundColor,
      };
    });
    const moreRatio = contrastRatio(
      parseRgb(moreColor.fg).slice(0, 3) as [number, number, number],
      parseRgb(moreColor.bg).slice(0, 3) as [number, number, number]
    );
    expect(
      moreRatio,
      `${theme} count-link: WCAG AA floor is 4.5:1; measured ${moreRatio.toFixed(2)}:1`
    ).toBeGreaterThanOrEqual(4.5);
  }
});

test('tenant board text (badges, legend, planned rows, soon marks, star plaque) clears WCAG AA against the dark board ground (day + night)', async ({
  page,
}) => {
  // The board's --screen ground is a THEME-INDEPENDENT literal, so sweeping
  // both themes is defense-in-depth, not an expected-to-move ratio.
  for (const theme of ['day', 'night'] as const) {
    await page.addInitScript((t) => {
      sessionStorage.setItem('pix-booted', '1');
      localStorage.setItem('pix-theme', t);
    }, theme);
    await page.goto('./');
    await expect(page.locator('html')).toHaveAttribute('data-theme', theme);
    await page.evaluate(() =>
      document.getElementById('tools')!.scrollIntoView({ block: 'center', behavior: 'instant' })
    );

    const boardBg = await page.evaluate(
      () => getComputedStyle(document.querySelector('.tools__board')!).backgroundColor
    );
    const bg = parseRgb(boardBg).slice(0, 3) as [number, number, number];
    const assertAA = (codeColor: string, label: string) => {
      const fg = parseRgb(codeColor).slice(0, 3) as [number, number, number];
      const ratio = contrastRatio(fg, bg);
      expect(
        ratio,
        `${theme} "${label}": WCAG AA floor is 4.5:1; color ${codeColor} on board ${boardBg} measured ${ratio.toFixed(2)}:1`
      ).toBeGreaterThanOrEqual(4.5);
    };

    const badges = await page.evaluate(() =>
      [...document.querySelectorAll('.tools__board .tools__badge')].map((b) => ({
        codeColor: getComputedStyle(b).color,
        label: b.textContent?.trim(),
      }))
    );
    expect(badges.length, `${theme}: no .tools__badge chips rendered`).toBe(
      supportedSources.length
    );
    for (const { codeColor, label } of badges) assertAA(codeColor, `badge ${label}`);

    // the legend always renders: the manifest always has both 'yes' and
    // 'experimental' states on screen.
    const legendColor = await page.evaluate(
      () => getComputedStyle(document.querySelector('.tools__legend')!).color
    );
    assertAA(legendColor, 'legend');

    // the plaque is its OWN .hw-panel — assert its own bg, so a future
    // divergence between the two panels' grounds can't go unnoticed.
    const plaqueBg = await page.evaluate(
      () => getComputedStyle(document.querySelector('.tools__plaque')!).backgroundColor
    );
    const assertPlaqueAA = (codeColor: string, label: string) => {
      const fg = parseRgb(codeColor).slice(0, 3) as [number, number, number];
      const ratio = contrastRatio(fg, parseRgb(plaqueBg).slice(0, 3) as [number, number, number]);
      expect(
        ratio,
        `${theme} "${label}": WCAG AA floor is 4.5:1; color ${codeColor} on plaque ${plaqueBg} measured ${ratio.toFixed(2)}:1`
      ).toBeGreaterThanOrEqual(4.5);
    };
    const plaqueColors = await page.evaluate(() => ({
      stars: getComputedStyle(document.querySelector('.tools__plaque-stars')!).color,
      engraving: getComputedStyle(document.querySelector('.tools__plaque-engraving')!).color,
      link: getComputedStyle(document.querySelector('.tools__plaque-link')!).color,
    }));
    assertPlaqueAA(plaqueColors.stars, 'plaque stars');
    assertPlaqueAA(plaqueColors.engraving, 'plaque engraving');
    assertPlaqueAA(plaqueColors.link, 'plaque link');

    // sources.json currently has zero "planned" rows, so probe the markup SupportedTools.astro emits, injected into the real table
    // for the live cascade. PAIRED-COPY PIN: MARK() is inline and unexported, so this literal MUST track its markup by hand.
    const planned = await page.evaluate(() => {
      const table = document.querySelector('.tools__board table')!;
      const tbody = document.createElement('tbody');
      tbody.className = 'tools__planned';
      tbody.innerHTML =
        '<tr><th scope="row">Probe Tool</th><td class="tools__cell" data-state="planned">' +
        '<span class="tools__mark tools__soon" aria-hidden="true">soon</span></td></tr>';
      table.appendChild(tbody);
      const result = {
        rowColor: getComputedStyle(tbody.querySelector('th')!).color,
        soonColor: getComputedStyle(tbody.querySelector('.tools__soon')!).color,
      };
      tbody.remove();
      return result;
    });
    assertAA(planned.rowColor, 'planned row');
    assertAA(planned.soonColor, 'soon mark');
  }
});

test('pantry chitchat bubble text clears WCAG AA against its own dark ground (day + night)', async ({
  page,
}) => {
  // .pantry__bubble paints its OWN opaque --screen background, so read fg/bg
  // off the bubble element directly rather than off a panel ancestor.
  for (const theme of ['day', 'night'] as const) {
    await page.addInitScript((t) => {
      sessionStorage.setItem('pix-booted', '1');
      localStorage.setItem('pix-theme', t);
    }, theme);
    await page.goto('./');
    await expect(page.locator('html')).toHaveAttribute('data-theme', theme);
    await page.evaluate(() =>
      document.querySelector('.pantry')!.scrollIntoView({ block: 'center', behavior: 'instant' })
    );
    const bubbles = await page.evaluate(() =>
      [...document.querySelectorAll('.pantry__bubble')].map((b) => {
        const cs = getComputedStyle(b);
        return { color: cs.color, bg: cs.backgroundColor, label: b.textContent?.trim() };
      })
    );
    expect(bubbles.length, `${theme}: no .pantry__bubble rendered`).toBeGreaterThan(0);
    for (const { color, bg, label } of bubbles) {
      const fg = parseRgb(color).slice(0, 3) as [number, number, number];
      const bgRgb = parseRgb(bg).slice(0, 3) as [number, number, number];
      const ratio = contrastRatio(fg, bgRgb);
      expect(
        ratio,
        `${theme} "${label}": WCAG AA floor is 4.5:1; ${color} on ${bg} measured ${ratio.toFixed(2)}:1`
      ).toBeGreaterThanOrEqual(4.5);
    }
  }
});

test('docs callout body copy clears WCAG AA against the callout screen (day + night + dracula)', async ({
  page,
}) => {
  // `.prose p`/`.prose li` match the callout's own <p>/<li> DIRECTLY, and a direct match always beats the --chip-ink
  // .callout__body hands DOWN — hence the sibling `a`/`code` overrides, and hence sweeping every tag.
  const TEXT_TAGS = 'p, li, strong, em, a, code';
  for (const theme of ['day', 'night', 'dracula'] as const) {
    await page.addInitScript((t) => localStorage.setItem('pix-theme', t), theme);
    await page.goto('./config');
    await expect(page.locator('html')).toHaveAttribute('data-theme', theme);
    const routes = await page.evaluate(() =>
      [...document.querySelectorAll('.docs__sidebar .docs__list:not(.docs__list--building) a')].map(
        (a) => (a as HTMLAnchorElement).getAttribute('href')!
      )
    );
    expect(routes.length, `${theme}: no doc routes in the sidebar`).toBeGreaterThan(0);

    let swept = 0;
    for (const route of routes) {
      await page.goto(`.${route}`);
      const rows = await page.evaluate((tags) => {
        const transparent = (c: string) => /,\s*0\)\s*$/.test(c);
        return [...document.querySelectorAll('.callout__body')].flatMap((body) => {
          const screen = getComputedStyle(body.closest('.callout')!).backgroundColor;
          return [...body.querySelectorAll(tags)].map((el) => {
            const cs = getComputedStyle(el);
            return {
              tag: el.tagName.toLowerCase(),
              color: cs.color,
              bg: transparent(cs.backgroundColor) ? screen : cs.backgroundColor,
              text: (el.textContent || '').trim().slice(0, 48),
            };
          });
        });
      }, TEXT_TAGS);
      for (const { tag, color, bg, text } of rows) {
        const ratio = contrastRatio(
          parseRgb(color).slice(0, 3) as [number, number, number],
          parseRgb(bg).slice(0, 3) as [number, number, number]
        );
        expect(
          ratio,
          `${theme} ${route} <${tag}> "${text}": WCAG AA floor is 4.5:1; ${color} on ${bg} measured ${ratio.toFixed(2)}:1`
        ).toBeGreaterThanOrEqual(4.5);
      }
      swept += rows.length;
    }
    // teeth: a docs tree that stopped emitting callouts would pass vacuously
    expect(
      swept,
      `${theme}: no .callout__body text swept across ${routes.length} doc routes`
    ).toBeGreaterThan(0);
  }
});

test('the statusline feed ellipsizes on the wrapping text span, not the flex row', async ({
  page,
}) => {
  // A flex container's own overflow/text-overflow never applies to its children (the badge `<b>` and the " · {what}" run are
  // separate anonymous flex items), so the ellipsis must live on `.sl__text` — and must actually be clipping.
  await page.setViewportSize({ width: 1280, height: 720 });
  await gotoLive(page);
  // The feed's real content is a build-time GH API fetch, so its length varies
  // build to build — force the overflow deterministically instead.
  const info = await page.evaluate(() => {
    const text = document.querySelector('.sl__item .sl__text') as HTMLElement;
    text.textContent =
      'cc·pixtuoid · merged #999 · this is a deliberately very long line of feed text to force an overflow';
    const cs = getComputedStyle(text);
    return {
      overflow: cs.overflow,
      textOverflow: cs.textOverflow,
      whiteSpace: cs.whiteSpace,
      scrollWidth: text.scrollWidth,
      clientWidth: text.clientWidth,
    };
  });
  expect(info.overflow).toBe('hidden');
  expect(info.textOverflow).toBe('ellipsis');
  expect(info.whiteSpace).toBe('nowrap');
  expect(info.scrollWidth).toBeGreaterThan(info.clientWidth);
});

test('the feed hides itself, rather than show an unreadably short fragment, at a squeezed width', async ({
  page,
}) => {
  // 768-860px: .sl__text's width drops to a sliver even with a clean ellipsis —
  // hiding reads better than a fragment too short to convey anything.
  await page.setViewportSize({ width: 800, height: 720 });
  await gotoLive(page);
  await expect(page.locator('.sl__feed')).toBeHidden();
  await page.setViewportSize({ width: 1024, height: 720 });
  await expect(page.locator('.sl__feed')).toBeVisible();
});

test('the feed pauses while the tab is hidden — no ghosted double-exposure on refocus', async ({
  page,
}) => {
  // A hidden tab freezes CSS transitions but NOT setInterval: a feed that kept rotating would replay its queued
  // `is-on` fades AT ONCE on refocus (the ghosting bug).
  await page.setViewportSize({ width: 1280, height: 720 });
  await gotoLive(page);
  await expect(page.locator('.sl__item.is-on')).toHaveCount(1);

  const litIndex = () =>
    page.evaluate(() =>
      Array.from(document.querySelectorAll('.sl__item')).findIndex((el) =>
        el.classList.contains('is-on')
      )
    );

  // Force "hidden" AND read the lit line in the SAME synchronous evaluate: visibilitychange runs stopFeed → showOnlyFeed,
  // so the read can't land in the fade gap (findIndex → -1) or race a free-running 6s tick.
  const before = await page.evaluate(() => {
    Object.defineProperty(document, 'hidden', { configurable: true, get: () => true });
    document.dispatchEvent(new Event('visibilitychange'));
    return Array.from(document.querySelectorAll('.sl__item')).findIndex((el) =>
      el.classList.contains('is-on')
    );
  });
  expect(before).toBeGreaterThanOrEqual(0);

  // A pix:paused{false} can fire WHILE hidden, and startFeed's document.hidden
  // guard must refuse to re-arm the rotation; then wait past one 6s rotation.
  await page.evaluate(() =>
    document.dispatchEvent(new CustomEvent('pix:paused', { detail: { paused: false } }))
  );
  await page.waitForTimeout(6500);
  await expect(page.locator('.sl__item.is-on')).toHaveCount(1);
  expect(await litIndex()).toBe(before);

  await page.evaluate(() => {
    Object.defineProperty(document, 'hidden', { configurable: true, get: () => false });
    document.dispatchEvent(new Event('visibilitychange'));
  });
  await expect(page.locator('.sl__item.is-on')).toHaveCount(1);

  // stopFeed must collapse an illegal 2+-lit state back to exactly one line.
  await page.evaluate(() => {
    document.querySelectorAll('.sl__item').forEach((el) => el.classList.add('is-on'));
    document.dispatchEvent(new CustomEvent('pix:paused', { detail: { paused: true } }));
  });
  await expect(page.locator('.sl__item.is-on')).toHaveCount(1);
});

test('footer separators never strand alone at a wrap boundary', async ({ page }) => {
  // Each "·" is grouped with the item it introduces into ONE flex item, so
  // flex-wrap can only break BETWEEN groups; pin the structure, not a pixel wrap.
  await gotoLive(page);
  const seps = await page.locator('.footer .footer__sep').all();
  expect(seps.length).toBeGreaterThan(0);
  for (const sep of seps) {
    await expect(sep.locator('xpath=..')).toHaveClass(/\bfooter__grp\b/);
  }
});

test('no footer line begins or ends with a separator dot once the row wraps', async ({ page }) => {
  // Each dot introduces its FOLLOWING item, so a group that itself wraps still leads the new line with its own dot.
  // Check RENDERED rows (grouped by top position): a display:none dot still shows up in textContent.
  await gotoLive(page);
  await page.setViewportSize({ width: 768, height: 900 });
  await page.evaluate(() =>
    window.scrollTo({ top: document.documentElement.scrollHeight, behavior: 'instant' })
  );
  const bad = await page.evaluate(() => {
    const line = document.querySelector('.footer__line')!;
    const items = Array.from(line.children).filter(
      (el) => (el as HTMLElement).offsetParent !== null
    );
    const rows = new Map<number, Element[]>();
    for (const el of items) {
      const top = Math.round(el.getBoundingClientRect().top);
      (rows.get(top) ?? rows.set(top, []).get(top)!).push(el);
    }
    const findings: string[] = [];
    for (const rowEls of rows.values()) {
      rowEls.sort((a, b) => a.getBoundingClientRect().left - b.getBoundingClientRect().left);
      const leadingSep = rowEls[0].querySelector('.footer__sep');
      if (leadingSep && getComputedStyle(leadingSep).display !== 'none') {
        findings.push(`leading: "${(rowEls[0].textContent || '').trim()}"`);
      }
      const lastText = (rowEls[rowEls.length - 1].textContent || '').trim();
      if (lastText.endsWith('·')) {
        findings.push(`trailing: "${lastText}"`);
      }
    }
    return findings;
  });
  expect(bad).toEqual([]);
});

test('the pause control never overlaps a footer link across the mobile wrap range', async ({
  page,
}) => {
  // The footer's wrap count is non-monotonic across viewport widths (3 lines in the 360-460px band), so no flat clearance offset works — sweep the whole range.
  await gotoLive(page);
  const widths = [360, 375, 390, 393, 412, 460, 480, 768, 960];
  for (const width of widths) {
    await page.setViewportSize({ width, height: 844 });
    // expect.poll tolerates the async ResizeObserver round-trip that updates --footer-h, and re-settles scroll-bottom on each retry.
    // behavior:'instant' is load-bearing: under global.css's scroll-behavior:smooth, scrollTo(x, y) is still animating when the rect is read.
    await expect
      .poll(() =>
        page.evaluate((w) => {
          window.scrollTo({
            top: document.documentElement.scrollHeight,
            left: 0,
            behavior: 'instant',
          });
          const btn = document.getElementById('office-pause');
          if (!btn || btn.hidden) return [];
          const b = btn.getBoundingClientRect();
          return Array.from(document.querySelectorAll<HTMLAnchorElement>('.footer a'))
            .filter((a) => {
              const r = a.getBoundingClientRect();
              return !(
                r.right <= b.left ||
                r.left >= b.right ||
                r.bottom <= b.top ||
                r.top >= b.bottom
              );
            })
            .map((a) => `${w}px: ${(a.textContent || '').trim()}`);
        }, width)
      )
      .toEqual([]);
  }
});

test('the pause control never occludes in-page copy at mobile widths', async ({ page }) => {
  // .office-ctl is position:fixed, so its band is CONSTANT across the whole scroll — every section's copy passes under it, not just the footer's.
  // Each spot scrolls its OWN midpoint onto the band's midpoint (worst case); scrollIntoView({block:'center'}) centers in the VIEWPORT and misses it.
  await gotoLive(page);
  await page.setViewportSize({ width: 390, height: 844 });

  async function assertClearOfPause(selector: string): Promise<void> {
    const overlap = await page.evaluate((sel) => {
      const el = document.querySelector(sel) as HTMLElement | null;
      const btn = document.getElementById('office-pause') as HTMLElement | null;
      if (!el || !btn || btn.hidden) return { found: !!el, overlap: false };
      const b = btn.getBoundingClientRect();
      const r = el.getBoundingClientRect();
      const elAbsMid = (r.top + r.bottom) / 2 + window.scrollY;
      const bandMid = (b.top + b.bottom) / 2;
      window.scrollTo({ top: Math.max(0, Math.round(elAbsMid - bandMid)), behavior: 'instant' });
      const r2 = el.getBoundingClientRect();
      const b2 = btn.getBoundingClientRect();
      const overlap = !(
        r2.right <= b2.left ||
        r2.left >= b2.right ||
        r2.bottom <= b2.top ||
        r2.top >= b2.bottom
      );
      return { found: true, overlap };
    }, selector);
    expect(overlap.found, `${selector} not found`).toBe(true);
    expect(overlap.overlap, `${selector} overlaps #office-pause's fixed band`).toBe(false);
  }

  await assertClearOfPause('.hero__ghost[href="#showcase-vibing"]');
  await assertClearOfPause('.office-gap:not(.office-gap--closer) .gap-caption');
  await assertClearOfPause('.how__step:first-child .how__detail p');
  await assertClearOfPause('[data-vibing-time-label]');
});

test('the elevator shaft never overlaps the studio panel copy at 390 or 768', async ({ page }) => {
  // .shaft is position:fixed at every width and .container reserves no gutter
  // for it. Horizontal position doesn't depend on scroll — pure geometry.
  await gotoLive(page);
  for (const width of [390, 768]) {
    await page.setViewportSize({ width, height: 844 });
    const overlaps = await page.evaluate(() => {
      const shaft = document.querySelector('.shaft');
      if (!shaft) return [];
      const shaftLeft = shaft.getBoundingClientRect().left;
      return Array.from(document.querySelectorAll<HTMLElement>('.roster__row'))
        .map((el) => el.getBoundingClientRect().right - shaftLeft)
        .filter((over) => over > 0);
    });
    expect(
      overlaps,
      `${width}px: roster rows reach ${overlaps}px past the shaft's left edge`
    ).toEqual([]);
  }
});

test('an install copy from the Install section hires a coworker: pix:install-copy → pix:hired', async ({
  page,
  context,
}) => {
  await context.grantPermissions(['clipboard-write']);
  const errors = watchErrors(page);
  await page.addInitScript(() => {
    sessionStorage.setItem('pix-booted', '1');
    (window as { __hired?: boolean }).__hired = false;
    document.addEventListener(
      'pix:hired',
      () => ((window as { __hired?: boolean }).__hired = true)
    );
  });
  await page.goto('./');
  // hire() is a no-op before the first live frame — wait for the office.
  await expect(page.locator('.backdrop.is-live')).toBeAttached({ timeout: 15_000 });
  await page.evaluate(() =>
    document.getElementById('install')!.scrollIntoView({ block: 'center', behavior: 'instant' })
  );
  await page.locator('.install__panel.is-active .install__copy').click();
  await expect
    .poll(() => page.evaluate(() => (window as { __hired?: boolean }).__hired), {
      timeout: 10_000,
    })
    .toBe(true);
  expect(errors()).toEqual([]);
});

test('proof split: replay clip plays in view and obeys the page pause', async ({ page }) => {
  const errors = watchErrors(page);
  await gotoLive(page);
  await page.evaluate(() =>
    document.getElementById('proof')!.scrollIntoView({ block: 'center', behavior: 'instant' })
  );
  const vid = page.locator('.proof__video--wide');
  await expect(page.locator('.proof__video--tall')).toBeHidden();
  await expect.poll(() => vid.evaluate((v) => !(v as HTMLVideoElement).paused)).toBe(true);
  const paused = () => vid.evaluate((v) => (v as HTMLVideoElement).paused);
  await page.evaluate(() =>
    document.dispatchEvent(new CustomEvent('pix:paused', { detail: { paused: true } }))
  );
  await expect.poll(paused).toBe(true);
  await page.evaluate(() =>
    document.dispatchEvent(new CustomEvent('pix:paused', { detail: { paused: false } }))
  );
  await expect.poll(paused).toBe(false);
  await expect(page.locator('.proof__coda')).toContainText('pixtuoid floating');
  expect(errors()).toEqual([]);
});

// Pins ProofSplit's deferred-poster markup; the WHY is on the component.
for (const variant of [
  { name: 'wide', viewport: { width: 1280, height: 800 }, mobile: false },
  { name: 'narrow', viewport: { width: 390, height: 820 }, mobile: true },
]) {
  test(`proof split (${variant.name}): a poster is fetched only for the active variant, and only once the section nears the viewport`, async ({
    browser,
  }) => {
    const context = await browser.newContext({
      viewport: variant.viewport,
      isMobile: variant.mobile,
      hasTouch: variant.mobile,
    });
    const page = await context.newPage();
    const posters: string[] = [];
    page.on('request', (r) => {
      const m = /\/demos\/(proof(?:-tall)?-poster\.png)/.exec(r.url());
      if (m) posters.push(m[1]);
    });
    await page.addInitScript(() => sessionStorage.setItem('pix-booted', '1'));
    await page.goto('./');
    await page.waitForLoadState('networkidle');
    expect(posters, 'nothing at load — the section is screens away').toEqual([]);
    await page.evaluate(() =>
      document.getElementById('proof')!.scrollIntoView({ block: 'center', behavior: 'instant' })
    );
    const active = variant.mobile ? 'proof-tall-poster.png' : 'proof-poster.png';
    await expect.poll(() => posters).toContain(active);
    // The CAUSE, not a race with the network: the hidden variant was never promoted.
    const idle = page.locator(variant.mobile ? '.proof__video--wide' : '.proof__video--tall');
    await expect(idle).toHaveAttribute('data-poster', /poster\.png/);
    await expect(idle).not.toHaveAttribute('poster');
    expect(new Set(posters), 'never the hidden variant').toEqual(new Set([active]));
    await context.close();
  });
}

test('proof split: without JS the poster still shows, as a plain image', async ({ browser }) => {
  // Nothing promotes a data-poster without script, so the fallback is markup.
  const context = await browser.newContext({ javaScriptEnabled: false });
  const page = await context.newPage();
  await page.goto('./');
  const fallback = page.locator('#proof img.proof__video');
  await expect(fallback).toHaveAttribute('src', /proof-poster/);
  await fallback.scrollIntoViewIfNeeded();
  await expect(fallback).toBeVisible();
  await expect(page.locator('.proof__video--wide')).toBeHidden();
  await expect(page.locator('.proof__video--tall')).toBeHidden();
  await context.close();
});

test('proof split: narrow viewport swaps to the tall stack of the SAME render', async ({
  browser,
}) => {
  const context = await browser.newContext({
    viewport: { width: 390, height: 820 },
    isMobile: true,
    hasTouch: true,
  });
  const page = await context.newPage();
  await page.addInitScript(() => sessionStorage.setItem('pix-booted', '1'));
  await page.goto('./');
  await page.evaluate(() =>
    document.getElementById('proof')!.scrollIntoView({ block: 'center', behavior: 'instant' })
  );
  await expect(page.locator('.proof__video--tall')).toBeVisible();
  await expect(page.locator('.proof__video--wide')).toBeHidden();
  await context.close();
});

test('proof split: narrow + reduced motion shows the tall poster, not a blank box', async ({
  browser,
}) => {
  // The reduced-motion arm returns before hydrate() ever runs, and hydrate() is
  // the only other place data-poster gets promoted.
  const context = await browser.newContext({
    viewport: { width: 390, height: 844 },
    isMobile: true,
    hasTouch: true,
    reducedMotion: 'reduce',
  });
  const page = await context.newPage();
  await page.addInitScript(() => sessionStorage.setItem('pix-booted', '1'));
  await page.goto('./');
  await page.evaluate(() =>
    document.getElementById('proof')!.scrollIntoView({ block: 'center', behavior: 'instant' })
  );
  const tall = page.locator('.proof__video--tall');
  const wide = page.locator('.proof__video--wide');
  await expect(tall).toBeVisible();
  await expect(wide).toBeHidden();
  await expect(tall).toHaveAttribute('poster', /proof-tall-poster/);
  expect(await tall.evaluate((v) => v.querySelectorAll('source').length)).toBe(0);
  expect(await wide.evaluate((v) => v.querySelectorAll('source').length)).toBe(0);
  await expect.poll(() => tall.evaluate((v) => (v as HTMLVideoElement).paused)).toBe(true);
  await context.close();
});

async function resolvedCoral(page: Page): Promise<string> {
  return page.evaluate(() => {
    const probe = document.createElement('span');
    probe.style.color = 'var(--coral)';
    document.body.appendChild(probe);
    const c = getComputedStyle(probe).color;
    probe.remove();
    return c;
  });
}

test('audit C1: night theme does not repaint chrome anchors coral (a:not(.btn) is :where()-scoped)', async ({
  page,
}) => {
  await page.addInitScript(() => {
    sessionStorage.setItem('pix-booted', '1');
    localStorage.setItem('pix-theme', 'night');
  });
  await page.goto('./');
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'night');
  // The global `:root[data-theme='night'] a:not(.btn)` rule (0,3,1) outranks
  // these anchors' own (0,2,0) colours unless it stays :where()-scoped.
  const coral = await resolvedCoral(page);
  for (const sel of ['.hero__ghost', '.nav__logo', '.tools__plaque-link']) {
    const color = await page
      .locator(sel)
      .first()
      .evaluate((e) => getComputedStyle(e).color);
    expect(color, `${sel} must not be the a:not(.btn) coral in night`).not.toBe(coral);
  }
});

test('audit C1 (docs): the /config pager link is not coral in night', async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem('pix-theme', 'night'));
  await page.goto('/config/');
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'night');
  const coral = await resolvedCoral(page);
  const color = await page
    .locator('.docs__pager-link')
    .first()
    .evaluate((e) => getComputedStyle(e).color);
  expect(color).not.toBe(coral);
});

test('audit C3/C4: tools board How column is un-dimmed (opacity 1) and OS cells centre', async ({
  page,
}) => {
  await gotoLive(page);
  const how = await page
    .locator('.tools__board td.tools__how')
    .first()
    .evaluate((e) => getComputedStyle(e).opacity);
  expect(how, 'tools__how must be opacity 1 on the board, not the base 0.7 (audit C3)').toBe('1');
  const align = await page
    .locator('.tools__cell')
    .first()
    .evaluate((e) => getComputedStyle(e).textAlign);
  expect(align, 'tools__cell must centre its LED (audit C4)').toBe('center');
});

test('audit C9: pausing the office before the pantry reveals keeps the FAQ visible', async ({
  page,
}) => {
  await gotoLive(page);
  // Pause while the pantry is off-screen, THEN reveal it — that order is the
  // repro: the pre-fix inline animationPlayState froze the pop at opacity:0.
  await page.evaluate(() =>
    document.dispatchEvent(new CustomEvent('pix:paused', { detail: { paused: true } }))
  );
  await page.evaluate(() =>
    document
      .querySelector('.pantry__scene')!
      .scrollIntoView({ block: 'center', behavior: 'instant' })
  );
  const bubble = page.locator('.pantry__bubble').first();
  await expect(bubble).toBeVisible();
  await expect
    .poll(() => bubble.evaluate((e) => parseFloat(getComputedStyle(e).opacity)))
    .toBeGreaterThan(0.5);
});

test('audit C7: the doc-page install chip links cross-page, not a dead same-page fragment', async ({
  page,
}) => {
  await page.goto('/config/');
  const href = await page.locator('[data-sl-install-link]').first().getAttribute('href');
  expect(href).not.toBe('#install');
  expect(href).toContain('#install');
});

test('audit C5: ♩ hides and never persists a silent "playing" when the AudioContext constructor throws', async ({
  page,
}) => {
  // WebAudio disabled (Tor / hardened Firefox / enterprise) → `new AudioContext()`
  // THROWS, but headless Chromium's ctor SUCCEEDS — force the throw path here.
  const errors = watchErrors(page);
  await page.addInitScript(() => {
    const Throwing = function () {
      throw new Error('WebAudio disabled');
    } as unknown as typeof AudioContext;
    window.AudioContext = Throwing;
  });
  await gotoLive(page);
  const btn = page.locator('#office-audio');
  await expect(btn).toBeVisible({ timeout: 15_000 });
  await btn.click();
  await expect(btn).toBeHidden();
  await expect(btn).toHaveAttribute('aria-pressed', 'false');
  expect(await page.evaluate(() => localStorage.getItem('pix:audio'))).not.toBe('1');
  expect(errors()).toEqual([]);
});
