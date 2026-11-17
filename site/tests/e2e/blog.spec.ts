import { expect, test } from '@playwright/test';
import { BLOG_DIR, BLOG_ROUTE, FEED_ROUTE, postIds, postRoute } from '../../config/blog.mjs';
import { floorById } from '../../config/floors.mjs';
import { REPO_BLOB } from '../../config/github.mjs';

// Runs against the PRODUCTION build (see playwright.config.ts).

test("the blog's lobby lists every post, newest first, and each links its page", async ({
  page,
}) => {
  await page.goto(`./${BLOG_ROUTE}`);
  const links = await page
    .locator('.posts a')
    .evaluateAll((as) => as.map((a) => a.getAttribute('href')));
  expect([...links].sort()).toEqual(
    postIds()
      .map((id) => `/${postRoute(id)}`)
      .sort()
  );
  const days = await page
    .locator('.posts time')
    .evaluateAll((times) => times.map((t) => t.getAttribute('datetime')));
  expect(days).toEqual([...days].sort().reverse());
  await expect(page.locator('#statusline')).toContainText('~ pixtuoid blog');
});

test('a post mounts the wing shell: its title, its day, its lit sidebar entry and its source', async ({
  page,
}) => {
  for (const id of postIds()) {
    await page.goto(`./${postRoute(id)}`);
    const title = await page.locator('article.prose h1').innerText();
    await expect(page, id).toHaveTitle(`${title} — pixtuoid`);
    await expect(page.locator('article.prose time'), id).toHaveAttribute(
      'datetime',
      /^\d{4}-\d{2}-\d{2}$/
    );
    await expect(page.locator('.wing__sidebar a[aria-current="page"]'), id).toHaveText(title);
    await expect(page.locator('.wing__source a'), id).toHaveAttribute(
      'href',
      `${REPO_BLOB}/site/${BLOG_DIR}/${id}.md`
    );
    await expect(page.locator('#statusline'), id).toContainText(`~ pixtuoid blog · /${id}`);
  }
});

test('the feed lists every post, and every page advertises the feed', async ({ page, request }) => {
  await page.goto('./');
  const feed = await page
    .locator('link[rel="alternate"][type="application/rss+xml"]')
    .getAttribute('href');
  expect(new URL(feed!).pathname).toBe(`/${FEED_ROUTE}`);
  const xml = await (await request.get(`./${FEED_ROUTE}`)).text();
  const site = new URL(feed!).origin;
  for (const id of postIds()) expect(xml, id).toContain(`<link>${site}/${postRoute(id)}</link>`);
});

test('the footer links the blog', async ({ page }) => {
  await page.goto('./');
  await expect(page.locator('.footer__line a', { hasText: 'Blog' })).toHaveAttribute(
    'href',
    `/${BLOG_ROUTE}`
  );
});

test("a post ends on a link to the site's install floor", async ({ page }) => {
  for (const id of postIds()) {
    await page.goto(`./${postRoute(id)}`);
    await expect(
      page.locator('article.prose').getByRole('link', { name: 'Install pixtuoid' })
    ).toHaveAttribute('href', `/#${floorById('install').id}`);
  }
});

// WCAG 2.2's SC 2.2.2 asks for a way to stop motion that "starts automatically" and "lasts more
// than five seconds" (www.w3.org/TR/WCAG22/#pause-stop-hide): these start in view and end sooner.
const MOTION_LIMIT_S = 5;

test('a moving figure plays once the reader reaches it, and ends by itself', async ({ page }) => {
  let moving = 0;
  for (const id of postIds()) {
    await page.goto(`./${postRoute(id)}`);
    for (const figure of await page.locator('figure:has(.fig__moving)').all()) {
      const motion = () =>
        figure.locator('.fig__moving').evaluateAll((els) =>
          els.map((el) => {
            const { animationName, animationDuration, animationIterationCount } =
              getComputedStyle(el);
            return {
              name: animationName,
              seconds: parseFloat(animationDuration) * Number(animationIterationCount),
            };
          })
        );
      expect(new Set((await motion()).map(({ name }) => name))).toEqual(new Set(['none']));
      // Not yet reached, it already shows: a reader who prints the page gets the figure.
      await expect(figure).toHaveCSS('opacity', '1');
      await figure.scrollIntoViewIfNeeded();
      await expect(figure).toHaveClass(/\bin\b/);
      for (const { name, seconds } of await motion()) {
        expect(name).not.toBe('none');
        expect(seconds).toBeLessThanOrEqual(MOTION_LIMIT_S);
        moving += 1;
      }
      const playheads = await figure.locator('.fig-budget__now').all();
      for (const playhead of playheads) await expect(playhead).toBeVisible();
      // Each run ends on the part's still state: an opacity its last keyframe sets is the one it
      // rests at, so nothing pops when the run ends.
      const pops = await figure.locator('.fig__moving').evaluateAll((els) =>
        els.flatMap((el) =>
          el.getAnimations().flatMap((run) => {
            const last = (run.effect as KeyframeEffect)
              .getKeyframes()
              .filter((frame) => frame.computedOffset === 1 && frame.opacity !== undefined)
              .map((frame) => String(frame.opacity));
            run.finish();
            return last.filter((opacity) => opacity !== getComputedStyle(el).opacity);
          })
        )
      );
      expect(pops).toEqual([]);
      for (const playhead of playheads) await expect(playhead).toBeHidden();
    }
  }
  // No post with a moving figure would pass the loop having checked nothing.
  expect(moving).toBeGreaterThan(0);
});

for (const [why, options] of [
  ['who asked for less motion', { reducedMotion: 'reduce' }],
  ['with no script', { javaScriptEnabled: false }],
] as const) {
  test(`a reader ${why} gets figures that stand still and show`, async ({ browser }) => {
    const context = await browser.newContext(options);
    const page = await context.newPage();
    let still = 0;
    for (const id of postIds()) {
      await page.goto(`./${postRoute(id)}`);
      for (const figure of await page.locator('figure:has(.fig__moving)').all()) {
        await figure.scrollIntoViewIfNeeded();
        await expect(figure).toHaveCSS('opacity', '1');
        for (const playhead of await figure.locator('.fig-budget__now').all()) {
          await expect(playhead).toBeHidden();
        }
        const seconds = await figure.locator('.fig__moving').evaluateAll((els) =>
          els.map((el) => {
            const style = getComputedStyle(el);
            return style.animationName === 'none' ? 0 : parseFloat(style.animationDuration);
          })
        );
        expect(Math.max(...seconds)).toBeLessThan(0.01);
        still += seconds.length;
      }
    }
    expect(still).toBeGreaterThan(0);
    await context.close();
  });
}
