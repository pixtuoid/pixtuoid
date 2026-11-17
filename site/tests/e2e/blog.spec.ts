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
test("a moving figure's switch stops it from the keyboard, with no script", async ({ browser }) => {
  const context = await browser.newContext({ javaScriptEnabled: false });
  const page = await context.newPage();
  const [id] = postIds();
  await page.goto(`./${postRoute(id)}`);
  for (const figure of await page.locator('figure:has(.fig__pause)').all()) {
    const moving = figure.locator('.fig-budget__now, .fig-pane__wash').first();
    const state = () => moving.evaluate((el) => getComputedStyle(el).animationPlayState);
    expect(await state()).toBe('running');
    await figure.locator('.fig__pause input').focus();
    await page.keyboard.press('Space');
    expect(await state()).toBe('paused');
    await page.keyboard.press('Space');
    expect(await state()).toBe('running');
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
  expect(
    await page
      .locator('.fig-pane__wash')
      .first()
      .evaluate((el) => getComputedStyle(el).animationName)
  ).toBe('none');
  await context.close();
});
