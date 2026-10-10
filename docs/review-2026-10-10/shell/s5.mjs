import { open, S } from './lib.mjs';
const { browser, page, logs } = await open({ w: 1920, h: 1080, q: '?staged=1&music=1' });
for (const n of ['Color', 'Audio', 'Motion', 'Deliver']) {
  await page.locator(`[data-workspace="${n.toLowerCase()}"]`).click();
  await page.waitForTimeout(900);
  await page.screenshot({ path: `${S}/ws-${n}.png` });
}
console.log(logs);
await browser.close();
