import { expect, test } from '@playwright/test';
import { BLOG_DIR, BLOG_ROUTE, FEED_ROUTE, postIds, postRoute } from '../../config/blog.mjs';
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

// The switch is a checkbox the stylesheet reads, so it has to work with no script at all.
test('each moving figure has a switch that stops it alone, from the keyboard, with no script', async ({
  browser,
}) => {
  const context = await browser.newContext({ javaScriptEnabled: false });
  const page = await context.newPage();
  const [id] = postIds();
  await page.goto(`./${postRoute(id)}`);
  const figures = await page.locator('figure:has(.fig__moving)').all();
  expect(figures.length).toBeGreaterThan(1);
  const states = () =>
    Promise.all(
      figures.map((figure) =>
        figure
          .locator('.fig__moving')
          .first()
          .evaluate((el) => getComputedStyle(el).animationPlayState)
      )
    );
  for (const [at, figure] of figures.entries()) {
    const others = figures.map((_, i) => (i === at ? 'paused' : 'running'));
    await figure.locator('.fig__pause input').focus();
    await page.keyboard.press('Space');
    expect(await states()).toEqual(others);
    await page.keyboard.press('Space');
    expect(await states()).toEqual(figures.map(() => 'running'));
  }
  await context.close();
});

test('a reader who asked for less motion gets still figures and no switch', async ({ browser }) => {
  const context = await browser.newContext({ reducedMotion: 'reduce' });
  const page = await context.newPage();
  const [id] = postIds();
  await page.goto(`./${postRoute(id)}`);
  await expect(page.locator('.fig__pause').first()).toBeHidden();
  await expect(page.locator('.fig-budget__now').first()).toBeHidden();
  const names = await page
    .locator('.fig__moving')
    .evaluateAll((els) => els.map((el) => getComputedStyle(el).animationName));
  expect(new Set(names)).toEqual(new Set(['none']));
  await context.close();
});
