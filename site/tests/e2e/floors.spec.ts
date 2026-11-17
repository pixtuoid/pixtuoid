import { expect, test } from '@playwright/test';
import { FLOORS, floorById } from '../../config/floors.mjs';
import featuresData from '../../src/features.json' with { type: 'json' };
import sourcesData from '../../src/sources.json' with { type: 'json' };

const TOP = FLOORS[0].fl;

// Read the manifests directly rather than hand-copying expected strings, which
// would silently go stale.
type Feature = { name: string; desc: string; channel?: string };
const features = featuresData as Feature[];
const descByChannel = new Map(features.filter((f) => f.channel).map((f) => [f.channel!, f.desc]));

test('cold load: the dial marks the default channel pressed, accordion shows its desc', async ({
  page,
}) => {
  await page.addInitScript(() => sessionStorage.setItem('pix-booted', '1'));
  await page.goto('./');
  // 'vibing' is the default channel (showcase.json's default:true)
  await expect(page.locator('button.mon[data-ch="vibing"]')).toHaveAttribute(
    'aria-selected',
    'true'
  );
  await expect(page.locator('#dial-desc')).toHaveText(descByChannel.get('vibing')!);
});

test('dial accordion: clicking a channel reveals its features.json desc under the dial', async ({
  page,
}) => {
  await page.addInitScript(() => sessionStorage.setItem('pix-booted', '1'));
  await page.goto('./');
  const btn = page.locator('button.mon[data-ch="openclaw"]');
  await expect(btn).toHaveAttribute('aria-selected', 'false');
  await btn.click();
  await expect(btn).toHaveAttribute('aria-selected', 'true');
  await expect(page.locator('button.mon[data-ch="vibing"]')).toHaveAttribute(
    'aria-selected',
    'false'
  );
  await expect(page.locator('#dial-desc')).toHaveText(descByChannel.get('openclaw')!);
});

test('the below-stage roster is exactly the features WITHOUT a channel', async ({ page }) => {
  await page.addInitScript(() => sessionStorage.setItem('pix-booted', '1'));
  await page.goto('./');
  await expect(page.locator('#showcase [data-feature-ch]')).toHaveCount(0);
  await expect(page.locator('#showcase button.roster__row')).toHaveCount(0);
  await expect(page.locator('#showcase a[href^="#showcase-"]')).toHaveCount(0);
  const expectedNames = features.filter((f) => !f.channel).map((f) => f.name);
  expect(expectedNames.length).toBeGreaterThan(0);
  const renderedNames = await page.locator('#showcase .roster__name').allTextContents();
  expect(renderedNames).toEqual(expectedNames);
});

test('the feature roster stays a quiet, non-interactive grid — the dial is the switcher', async ({
  page,
}) => {
  await page.addInitScript(() => sessionStorage.setItem('pix-booted', '1'));
  await page.goto('./');
  const btn = page.locator('button.mon[data-ch="meetings"]');
  await btn.scrollIntoViewIfNeeded();
  await btn.click();
  await expect(page.locator('[data-stage="meetings"]')).toBeVisible();
  await expect(page.locator('[data-stage="vibing"]')).toBeHidden();
  await expect(btn).toHaveAttribute('aria-selected', 'true');
  await expect(page.locator('section.features')).toHaveCount(0);
});

test('the dial is APG tabs: one tab stop, arrows tune the next live channel', async ({ page }) => {
  await page.addInitScript(() => sessionStorage.setItem('pix-booted', '1'));
  await page.goto('./');
  const tabs = page.locator('.dial[role="tablist"] button[role="tab"]');
  await expect(page.locator('.dial button[role="tab"][tabindex="0"]')).toHaveCount(1);
  const selected = page.locator('.dial button[role="tab"][aria-selected="true"]');
  await selected.focus();
  const from = await selected.getAttribute('data-ch');
  await page.keyboard.press('ArrowRight');
  const now = page.locator('.dial button[role="tab"][aria-selected="true"]');
  await expect(now).toBeFocused();
  expect(await now.getAttribute('data-ch')).not.toBe(from);
  const panel = await now.getAttribute('aria-controls');
  await expect(page.locator(`#${panel}[role="tabpanel"]`)).toBeVisible();
  await page.keyboard.press('End');
  await expect(tabs.last()).toHaveAttribute('aria-selected', 'true');
});

test('CRT channel keys: a digit tunes the channel and does NOT ride the floor elevator', async ({
  page,
}) => {
  await page.addInitScript(() => sessionStorage.setItem('pix-booted', '1'));
  await page.goto('./');
  // focus must land inside the data-keys-scope region for it to claim the digit
  await page.locator('button.dial__ch').first().focus();
  // channel 02 is 'openclaw' (showcase.json order)
  await page.keyboard.press('2');
  await expect(page.locator('[data-stage="openclaw"]')).toBeVisible();
  await expect(page.locator('button.dial__ch[data-ch="openclaw"]')).toHaveAttribute(
    'aria-selected',
    'true'
  );
  await expect(page.locator('[data-lift-digit]')).not.toHaveText('2F');
});

test('the floors declare the anchor contract, top floor down', async ({ page }) => {
  await page.addInitScript(() => sessionStorage.setItem('pix-booted', '1'));
  await page.goto('./');
  const floors = await page.$$eval('[data-floor]', (els) =>
    els.map((e) => ({
      fl: e.getAttribute('data-floor'),
      label: e.getAttribute('data-floor-label'),
      id: e.id,
    }))
  );
  expect(floors).toEqual(FLOORS.map(({ fl, label, id }) => ({ fl, label, id })));
  await expect(page.locator('[data-lift-digit]')).toHaveText(/^\dF$/);
  // inbound /#features deep links must still land on the floor holding the roster
  const shimFloor = await page.$eval('#features', (el) =>
    el.closest('[data-floor]')?.getAttribute('data-floor')
  );
  expect(shimFloor).toBe(floorById('showcase').fl);
});

test('the floating-window still gap is retired (wb-4 owns the slot)', async ({ page }) => {
  await page.addInitScript(() => sessionStorage.setItem('pix-booted', '1'));
  await page.goto('./');
  await expect(page.locator('[data-gap-still]')).toHaveCount(0);
  await expect(page.locator('[data-gap-daynight]')).toHaveCount(0);
  // the two holds that stay: #1 "the real thing" (a locked decision) and the closer
  await expect(page.locator('.office-gap')).toHaveCount(2);
});

test('every floor is reachable from the floor popover, and the statusline digit reads that floor', async ({
  page,
}) => {
  await page.addInitScript(() => sessionStorage.setItem('pix-booted', '1'));
  await page.goto('./');
  await expect(page.locator('[data-lift-digit]')).toHaveText(TOP);
  // A floor whose section is too short to ever own the floor-spy band
  // settles on a DIFFERENT floor here instead of its own.
  for (const { fl } of FLOORS) {
    await page.locator('[data-floor-toggle]').click();
    await page.locator(`#sl-floors [data-floor-btn="${fl}"]`).click();
    await expect(page.locator('[data-lift-digit]')).toHaveText(fl, { timeout: 10_000 });
    await expect(page.locator('#sl-floors [data-floor-btn][aria-current]')).toHaveCount(1);
    await expect(page.locator(`#sl-floors [data-floor-btn="${fl}"]`)).toHaveAttribute(
      'aria-current',
      'location'
    );
  }
});

test('scroll budget: the page fits its roster-aware viewport-height budget at 1440×900', async ({
  browser,
}) => {
  // Roster-aware so adding a source never edits this test: a new source costs
  // only its tools-table row, the hero's code strip being O(1). That holds only
  // while the strip's codes fit ONE line at 1440 (~20 sources); past that it
  // wraps a step PER_SOURCE doesn't model, so re-measure BASE there.
  // Headroom over the measured fit: an exact fit to CI's renderer reds another font stack.
  const SCROLL_BUDGET_BASE_VH = 9.07;
  const SCROLL_BUDGET_PER_SOURCE_VH = 0.075;
  const supported = sourcesData.filter((s) => s.status === 'supported').length;
  const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 } });
  const page = await ctx.newPage();
  await page.addInitScript(() => sessionStorage.setItem('pix-booted', '1'));
  await page.goto('./');
  await page.waitForLoadState('networkidle');
  const vh = await page.evaluate(() => document.documentElement.scrollHeight / window.innerHeight);
  expect(vh).toBeLessThanOrEqual(SCROLL_BUDGET_BASE_VH + supported * SCROLL_BUDGET_PER_SOURCE_VH);
  await ctx.close();
});
