import { expect, test, type Page } from '@playwright/test';
import { createRequire } from 'node:module';
import { readFileSync } from 'node:fs';

declare global {
  interface Window {
    __pixtuoidCls: number;
  }
}

const CLS_BUDGET = 0.1;
const FONT_DELAY_MS = 500;
const LIGHTHOUSE_VIEWPORT = { width: 412, height: 823 };
const require = createRequire(import.meta.url);
const webVitalsSource = readFileSync(require.resolve('web-vitals'), 'utf8');

async function observeCumulativeLayoutShift(page: Page): Promise<void> {
  await page.addInitScript(`${webVitalsSource}
self.__pixtuoidCls = 0;
self.webVitals.onCLS(
  ({ value }) => { self.__pixtuoidCls = value; },
  { reportAllChanges: true },
);
`);
}

for (const route of ['how-it-works/']) {
  test(`${route} stays stable when web fonts miss first paint`, async ({ page }) => {
    await page.setViewportSize(LIGHTHOUSE_VIEWPORT);
    await page.route(`**/${route}`, async (request) => {
      const response = await request.fetch();
      const body = await response.text();
      await request.fulfill({
        response,
        body: body.replace(
          '</head>',
          "<style>:root{--font-body:'Lora','Courier New',monospace!important;--font-mono:'Monaspace Neon',Arial,sans-serif!important}</style></head>"
        ),
      });
    });
    await page.route('**/*.woff2', async (request) => {
      await new Promise((resolve) => setTimeout(resolve, FONT_DELAY_MS));
      await request.continue();
    });
    await observeCumulativeLayoutShift(page);

    await page.goto(`./${route}`);
    await page.waitForLoadState('networkidle');

    expect(await page.evaluate(() => window.__pixtuoidCls)).toBeLessThanOrEqual(CLS_BUDGET);
  });
}
