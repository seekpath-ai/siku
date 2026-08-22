import { chromium } from '@playwright/test';

const browser = await chromium.launch({ headless: true });
const context = await browser.newContext({ viewport: { width: 1280, height: 800 } });
const page = await context.newPage();

await page.goto('http://localhost:1420/notes');
await page.waitForTimeout(2000);
const skip = page.locator('text=跳过');
if (await skip.count() > 0) await skip.click();
await page.waitForTimeout(800);
await page.screenshot({ path: 'notes-tree.png' });

// collapse list panel
const collapse = page.locator('button[title="折叠列表面板"]').first();
if (await collapse.count() > 0) {
  await collapse.evaluate((el) => el.click());
  await page.waitForTimeout(400);
  await page.screenshot({ path: 'notes-tree-collapsed.png' });
}

await browser.close();
console.log('screenshots saved');
