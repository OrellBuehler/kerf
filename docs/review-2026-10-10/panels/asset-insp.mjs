import { open } from './lib.mjs';
const { browser, page } = await open({ dsf: 1 });
await page.locator('div[role=button][draggable=true]').nth(1).click();
await page.waitForTimeout(400);
await page.getByText('Inspector', { exact: true }).first().click(); await page.waitForTimeout(300);
const t = await page.evaluate(() => { const g = [...document.querySelectorAll('.dv-groupview')].find(g => g.innerText.startsWith('Inspector')); return g.innerText.slice(0, 200).replace(/\n/g,' | '); });
console.log(t);
await page.screenshot({ path: 'asset-sel.png', clip: { x: 0, y: 36, width: 1920, height: 628 } });
await browser.close();
