import { open } from './lib.mjs';
const { browser, page } = await open({ w: 1100, h: 700 });
await page.screenshot({ path: 's960-edit.png' });
const info = await page.evaluate(() => [...document.querySelectorAll('.dv-groupview')].map(g => { const r = g.getBoundingClientRect(); return { x: Math.round(r.x), y: Math.round(r.y), w: Math.round(r.width), h: Math.round(r.height), t: g.innerText.slice(0,20).replace(/\n/g,' ') }; }));
console.log(JSON.stringify(info));
await page.mouse.click(300, 560);
await page.getByText('Inspector', { exact: true }).first().click(); await page.waitForTimeout(300);
await page.screenshot({ path: 's960-insp.png' });
await browser.close();
