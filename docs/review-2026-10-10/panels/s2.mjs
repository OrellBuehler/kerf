import { open } from './lib.mjs';
const { browser, page } = await open({});
await page.mouse.click(400, 858); // V1 interview clip
await page.waitForTimeout(500);
for (const t of ['Effects','Transitions','Audio','Titles']) {
  await page.locator(`[data-library-tab][aria-label="${t}"]`).click();
  await page.mouse.move(700, 400);
  await page.waitForTimeout(400);
  await page.screenshot({ path: `lib-${t.toLowerCase()}-clip.png`, clip: { x: 0, y: 36, width: 400, height: 630 } });
}
await page.locator(`[data-library-tab][aria-label="Media"]`).click();
await page.waitForTimeout(300);
await page.locator(`[data-library-tab][aria-label="Media"]`).click();
await page.waitForTimeout(500);
await page.screenshot({ path: 'lib-media-after.png', clip: { x: 0, y: 36, width: 400, height: 630 } });
// inspector
await page.getByText('Inspector', { exact: true }).first().click();
await page.waitForTimeout(500);
await page.screenshot({ path: 'insp-video-collapsed.png', clip: { x: 1467, y: 36, width: 453, height: 628 } });
await browser.close();
