import { expect, test, type Page } from '@playwright/test';

// Runs against the PRODUCTION build (see playwright.config.ts).

function watchErrors(page: Page): () => string[] {
  const errors: string[] = [];
  page.on('pageerror', (e) => errors.push(`pageerror: ${e.message}`));
  page.on('console', (msg) => {
    if (msg.type() === 'error') errors.push(`console.error: ${msg.text()}`);
  });
  return () => errors;
}

test('docs mount the statusline: route path left, install chip right, no landing organs', async ({
  page,
}) => {
  const errors = watchErrors(page);
  await page.goto('./config');
  const bar = page.locator('#statusline');
  await expect(bar).toBeVisible();
  await expect(bar).toContainText('~ pixtuoid docs · /config');
  await expect(bar.locator('#sl-install')).toBeAttached();
  await expect(bar.locator('[data-floor-toggle]')).toHaveCount(0);
  await expect(bar.locator('.sl__feed')).toHaveCount(0);
  expect(errors()).toEqual([]);
});

test('404 mounts the statusline too', async ({ page }) => {
  await page.goto('./no-such-desk');
  await expect(page.locator('#statusline')).toContainText('~ pixtuoid docs · /404');
});

test('the docs sidebar is an elevator panel: current-doc LED + a building bank from FLOORS', async ({
  page,
}) => {
  await page.setViewportSize({ width: 1280, height: 900 }); // rails hide below 1000px
  await page.goto('./config');
  const panel = page.locator('.docs__sidebar');
  await expect(panel).toHaveClass(/hw-panel/);
  await expect(panel.locator('a[aria-current="page"] .led-dot')).toHaveClass(/led-selected/);
  expect(await panel.locator('.led-dot.led-selected').count()).toBe(1);
  // the building bank reads the shared FLOORS manifest
  const building = panel.locator('.docs__list--building a');
  await expect(building).toHaveCount(6);
  await expect(building.first()).toHaveAttribute('href', /^\/#/);
});

test('markdown callouts render as terminal windows (note + warning)', async ({ page }) => {
  // CONTRIBUTING.md carries a "**Don't …**" blockquote → warning;
  // ARCHITECTURE.md a plain editorial one → note.
  await page.goto('./contributing');
  const warn = page.locator('.callout--warn').first();
  await expect(warn).toBeVisible();
  await expect(warn.locator('.callout__title')).toHaveText('~ warning');
  await expect(warn.locator('.terminal__dot--r')).toBeAttached();
  await page.goto('./architecture');
  const note = page.locator('.callout--note').first();
  await expect(note).toBeVisible();
  await expect(note.locator('.callout__title')).toHaveText('~ note');
});

test('tenant board: hw-panel screen, LED dots with shape distinction, sr-only values survive', async ({
  page,
}) => {
  await page.addInitScript(() => sessionStorage.setItem('pix-booted', '1'));
  await page.goto('./');
  await page.evaluate(() =>
    document.getElementById('tools')!.scrollIntoView({ block: 'center', behavior: 'instant' })
  );
  const board = page.locator('#tools .tools__board');
  await expect(board).toHaveClass(/hw-panel/);
  // ten supported CLIs × macOS = at least ten lit LEDs
  expect(await board.locator('.tools__led--on').count()).toBeGreaterThanOrEqual(10);
  expect(await board.locator('.tools__led--exp').count()).toBeGreaterThan(0);
  await expect(board.locator('td.tools__cell .sr-only').first()).toHaveText(
    /supported|experimental/
  );
  await expect(page.locator('#tools')).toHaveAttribute('data-floor', '2F');
});

test('the star plaque hangs beside the tenant board', async ({ page }) => {
  await page.addInitScript(() => sessionStorage.setItem('pix-booted', '1'));
  await page.goto('./');
  const plaque = page.locator('#tools .tools__plaque');
  await expect(plaque).toHaveClass(/hw-panel/);
  await expect(plaque).toContainText('★');
});

test('pantry FAQ: an indented answer stays inside the chat column', async ({ page }) => {
  await page.emulateMedia({ reducedMotion: 'reduce' });
  await page.addInitScript(() => sessionStorage.setItem('pix-booted', '1'));
  for (const width of [390, 1280]) {
    await page.setViewportSize({ width, height: 844 });
    await page.goto('./');
    const overhangs = await page.evaluate(() => {
      const column = document.querySelector('.pantry__chat')!.getBoundingClientRect().right;
      return [...document.querySelectorAll('.pantry__bubble')].map(
        (b) => b.getBoundingClientRect().right - column
      );
    });
    expect(overhangs.length, `${width}px: no bubble rendered`).toBeGreaterThan(0);
    expect(Math.max(...overhangs), `${width}px`).toBeLessThanOrEqual(0);
  }
});

test('pantry FAQ: every answer cites its source as a link to that file in the repo', async ({
  page,
}) => {
  await page.goto('./');
  const answers = await page.locator('.pantry__bubble--a').count();
  const cites = page.locator('.pantry__cite a');
  await expect(cites).toHaveCount(answers);
  for (const cite of await cites.all())
    await expect(cite).toHaveAttribute('href', /\/blob\/main\/[\w.-]+\.md#[\w-]+$/);
});

test('pantry FAQ: bubbles pop in sequence, join pix:paused, reduced-motion is static', async ({
  page,
  browser,
}) => {
  await page.addInitScript(() => sessionStorage.setItem('pix-booted', '1'));
  await page.goto('./');
  await page.evaluate(() =>
    document.querySelector('.pantry')!.scrollIntoView({ block: 'center', behavior: 'instant' })
  );
  const first = page.locator('.pantry__bubble').first();
  await expect
    .poll(() => first.evaluate((el) => getComputedStyle(el).opacity), { timeout: 10_000 })
    .toBe('1');
  // pausing toggles .is-paused, which DROPS the pop animation so a bubble rests
  // at its base opacity:1 — never frozen at the backwards-fill opacity:0.
  await page.evaluate(() =>
    document.dispatchEvent(new CustomEvent('pix:paused', { detail: { paused: true } }))
  );
  await expect(page.locator('.pantry__scene')).toHaveClass(/\bis-paused\b/);
  await expect.poll(() => first.evaluate((el) => getComputedStyle(el).animationName)).toBe('none');
  await expect(first).toBeVisible();
  const ctx = await browser.newContext({ reducedMotion: 'reduce' });
  const m = await ctx.newPage();
  await m.goto('./');
  const mb = m.locator('.pantry__bubble').first();
  expect(await mb.evaluate((el) => getComputedStyle(el).animationName)).toBe('none');
  expect(await mb.evaluate((el) => getComputedStyle(el).opacity)).toBe('1');
  await ctx.close();
});
