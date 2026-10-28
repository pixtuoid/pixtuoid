import { expect, test } from '@playwright/test';
import featuresData from '../../src/features.json' with { type: 'json' };
import sourcesData from '../../src/sources.json' with { type: 'json' };

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

test('the six floors declare the elevator anchor contract, top floor down', async ({ page }) => {
  await page.addInitScript(() => sessionStorage.setItem('pix-booted', '1'));
  await page.goto('./');
  const floors = await page.$$eval('[data-floor]', (els) =>
    els.map((e) => ({
      fl: e.getAttribute('data-floor'),
      label: e.getAttribute('data-floor-label'),
      id: e.id,
    }))
  );
  expect(floors).toEqual([
    { fl: '6F', label: 'penthouse — welcome', id: 'lobby' },
    { fl: '5F', label: 'studio — demos', id: 'showcase' },
    { fl: '4F', label: 'amenities — see it real', id: 'amenities' },
    { fl: '3F', label: 'machine room — quickstart', id: 'how' },
    { fl: '2F', label: 'tenants — compatibility', id: 'tools' },
    { fl: '1F', label: 'front desk — install', id: 'install' },
  ]);
  await expect(page.locator('[data-lift-digit]')).toHaveText(/^\dF$/);
  // inbound /#features deep links must still land on the floor holding the roster
  const shimFloor = await page.$eval('#features', (el) =>
    el.closest('[data-floor]')?.getAttribute('data-floor')
  );
  expect(shimFloor).toBe('5F');
});

test('the floating-window still gap is retired (wb-4 owns the slot)', async ({ page }) => {
  await page.addInitScript(() => sessionStorage.setItem('pix-booted', '1'));
  await page.goto('./');
  await expect(page.locator('[data-gap-still]')).toHaveCount(0);
  await expect(page.locator('[data-gap-daynight]')).toHaveCount(0);
  // the two holds that stay: #1 "the real thing" (a locked decision) and the closer
  await expect(page.locator('.office-gap')).toHaveCount(2);
});

test('elevator shaft: click-to-ride lands the floor, LED + lift readout agree', async ({
  page,
}) => {
  await page.addInitScript(() => sessionStorage.setItem('pix-booted', '1'));
  await page.goto('./');
  await expect(page.locator('[data-shaft-stop]')).toHaveCount(6);
  await expect(page.locator('[data-shaft-stop="6F"]')).toHaveAttribute('aria-current', 'true');
  const carY = () =>
    page.evaluate(
      () =>
        new DOMMatrix(getComputedStyle(document.querySelector('[data-shaft-car]')!).transform).m42
    );
  // 6F's resting Y is already > 0, so a bare "> 0" after the ride would pass even
  // if the car never moved — record a baseline and assert the DELTA instead.
  const preY = await carY();
  await page.locator('[data-shaft-stop="1F"]').click();
  await expect(page.locator('[data-shaft-stop="1F"]')).toHaveAttribute('aria-current', 'true', {
    timeout: 10_000,
  });
  await expect
    .poll(() =>
      page.evaluate(() => {
        const r = document.querySelector('[data-floor="1F"]')!.getBoundingClientRect();
        return r.top < window.innerHeight * 0.55 && r.bottom > window.innerHeight * 0.45;
      })
    )
    .toBe(true);
  await expect(page.locator('[data-lift-digit]')).toHaveText('1F');
  // Threshold is half an inter-stop gap — derived from the rail's own geometry
  // rather than a literal, and well above layout jitter but far below the
  // several gaps a real 6F→1F ride covers.
  const gap = await page.evaluate(() => {
    const stops = Array.from(document.querySelectorAll<HTMLElement>('[data-shaft-stop]'));
    return (stops[stops.length - 1].offsetTop - stops[0].offsetTop) / (stops.length - 1);
  });
  const postY = await carY();
  expect(postY - preY).toBeGreaterThan(gap / 2);
});

test('elevator shaft: the current-floor LED dot is actually lit, not just text-colored', async ({
  page,
}) => {
  await page.addInitScript(() => sessionStorage.setItem('pix-booted', '1'));
  await page.goto('./');
  // Resolve --led through a live probe rather than a literal, so the comparison
  // never depends on hex vs. rgb() string formatting.
  const litColor = await page.evaluate(() => {
    const probe = document.createElement('span');
    probe.style.background = 'var(--led)';
    document.body.appendChild(probe);
    const c = getComputedStyle(probe).backgroundColor;
    probe.remove();
    return c;
  });
  const dotColor = (fl: string) =>
    page
      .locator(`[data-shaft-stop="${fl}"] .led-dot`)
      .evaluate((el) => getComputedStyle(el).backgroundColor);
  await expect(page.locator('[data-shaft-stop="6F"]')).toHaveAttribute('aria-current', 'true');
  expect(await dotColor('6F')).toBe(litColor);
  expect(await dotColor('5F')).not.toBe(litColor);
  await page.locator('[data-shaft-stop="3F"]').click();
  await expect(page.locator('[data-shaft-stop="3F"]')).toHaveAttribute('aria-current', 'true', {
    timeout: 10_000,
  });
  expect(await dotColor('3F')).toBe(litColor);
  expect(await dotColor('6F')).not.toBe(litColor);
});

test('elevator shaft: every floor is reachable and reads current on BOTH the shaft LED and the statusline digit', async ({
  page,
}) => {
  await page.addInitScript(() => sessionStorage.setItem('pix-booted', '1'));
  await page.goto('./');
  // A floor whose section is too short to ever own the floor-spy center band
  // settles on a DIFFERENT floor here instead of its own.
  for (const fl of ['6F', '5F', '4F', '3F', '2F', '1F']) {
    await page.locator(`[data-shaft-stop="${fl}"]`).click();
    await expect(page.locator(`[data-shaft-stop="${fl}"]`)).toHaveAttribute(
      'aria-current',
      'true',
      {
        timeout: 10_000,
      }
    );
    await expect
      .poll(() =>
        page.evaluate((f) => {
          const r = document.querySelector(`[data-floor="${f}"]`)!.getBoundingClientRect();
          return r.top < window.innerHeight * 0.55 && r.bottom > window.innerHeight * 0.45;
        }, fl)
      )
      .toBe(true);
    await expect(page.locator('[data-shaft-stop][aria-current="true"]')).toHaveCount(1);
    await expect(page.locator('[data-lift-digit]')).toHaveText(fl);
  }

  // 1F + the footer rarely fill the center band the observer keys off, so at the
  // TRUE scroll max both readouts must clamp to 1F whatever the observer reported.
  await page.evaluate(() => window.scrollTo(0, document.documentElement.scrollHeight));
  await expect(page.locator('[data-shaft-stop="1F"]')).toHaveAttribute('aria-current', 'true', {
    timeout: 10_000,
  });
  await expect(page.locator('[data-shaft-stop][aria-current="true"]')).toHaveCount(1);
  await expect(page.locator('[data-lift-digit]')).toHaveText('1F');
});

test('elevator shaft: reduced motion is a static indicator', async ({ browser }) => {
  const ctx = await browser.newContext({ reducedMotion: 'reduce' });
  const page = await ctx.newPage();
  await page.addInitScript(() => sessionStorage.setItem('pix-booted', '1'));
  await page.goto('./');
  // The reduced-motion reset forces transition-duration near-zero but not
  // literally 0, so transitionend still fires — assert "no perceptible motion"
  // rather than an exact value.
  const styles = await page.evaluate(() => {
    const car = getComputedStyle(document.querySelector('[data-shaft-car]')!);
    return { transition: car.transitionDuration };
  });
  expect(parseFloat(styles.transition)).toBeLessThan(0.001);
  await page.locator('[data-shaft-stop="3F"]').click();
  await expect(page.locator('[data-shaft-stop="3F"]')).toHaveAttribute('aria-current', 'true', {
    timeout: 10_000,
  });
  await ctx.close();
});

test('elevator shaft: the ding pulse joins the pix:paused set', async ({ page }) => {
  await page.addInitScript(() => sessionStorage.setItem('pix-booted', '1'));
  await page.goto('./');
  await page.evaluate(() =>
    document.dispatchEvent(new CustomEvent('pix:paused', { detail: { paused: true } }))
  );
  await page.locator('[data-shaft-stop="1F"]').click();
  await expect(page.locator('[data-shaft-stop="1F"]')).toHaveAttribute('aria-current', 'true', {
    timeout: 10_000,
  });
  expect(
    await page.evaluate(() =>
      document.querySelector('[data-shaft-stop="1F"]')!.classList.contains('is-ding')
    )
  ).toBe(false);
});

test('scroll budget: the page fits its roster-aware viewport-height budget at 1440×900', async ({
  browser,
}) => {
  // Roster-aware so adding a source never edits this test: a new source costs
  // only its tools-table row, the hero's code strip being O(1). That holds only
  // while the strip's codes fit ONE line at 1440 (~20 sources); past that it
  // wraps a step PER_SOURCE doesn't model, so re-measure BASE there.
  // Headroom over the measured fit: an exact fit to CI's renderer reds another font stack.
  const SCROLL_BUDGET_BASE_VH = 8.15;
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
