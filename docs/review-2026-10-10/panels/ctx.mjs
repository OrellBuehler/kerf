import { open } from './lib.mjs';
const theme = process.argv[2] || 'dark';
const { browser, page } = await open({ theme, dsf: 1 });
await page.locator('div[role=button][draggable=true]').nth(0).click({ button: 'right', position: { x: 200, y: 30 } });
await page.waitForTimeout(500);
await page.screenshot({ path: `ctx-bin-${theme}.png`, clip: { x: 0, y: 36, width: 700, height: 1000 } });
const m = await page.evaluate(() => { const e = document.querySelector('[role=menu]'); if (!e) return null; const r = e.getBoundingClientRect(); return { x: r.x, y: r.y, w: r.width, h: r.height }; });
console.log(m);
await browser.close();
