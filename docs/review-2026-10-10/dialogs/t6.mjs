import { open, DIR } from './lib.mjs';
const { browser, page, logs } = await open({ w: 1920, h: 1080 });
await page.keyboard.press('Control+,');
await page.waitForTimeout(500);
const scroll = (y) => page.evaluate((y) => { const dlg = document.querySelector('[role=dialog]'); const p = [...dlg.querySelectorAll('div')].find(d => getComputedStyle(d).overflowY === 'auto' && d.scrollHeight > d.clientHeight); if (p) p.scrollTop = y; return p?.scrollHeight; }, y);
await page.locator('[role=dialog] button', { hasText: /^\s*Preview\s*$/ }).click();
await page.waitForTimeout(400);
await scroll(10000);
await page.waitForTimeout(200);
await (await page.$('[role=dialog] > div')).screenshot({ path: `${DIR}/41-settings-preview-bottom.png` });
await page.locator('[role=dialog] button', { hasText: /^\s*Appearance\s*$/ }).click();
await page.waitForTimeout(400);
for (const [i, y] of [[0,0],[1,550],[2,1100],[3,10000]]) { await scroll(y); await page.waitForTimeout(200); await (await page.$('[role=dialog] > div')).screenshot({ path: `${DIR}/42-settings-appearance-${i}.png` }); }
await browser.close();
