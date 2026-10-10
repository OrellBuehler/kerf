import { open } from './lib.mjs';
const [name, theme = 'dark', w = '1920', h = '1080', dsf = '1', tall = '0', q = '?music=1&staged=1'] = process.argv.slice(2);
const { browser, page } = await open({ theme, w: +w, h: +h, dsf: +dsf, q });
await page.getByText('Agent', { exact: true }).first().click();
await page.waitForTimeout(600);
if (tall === '1') { await page.setViewportSize({ width: +w, height: 2200 }); await page.waitForTimeout(800); }
const b = await page.evaluate(() => { const g = [...document.querySelectorAll('.dv-groupview')].find(g => g.innerText.startsWith('Inspector')); const r = g.getBoundingClientRect(); return { x: r.x, y: r.y, w: r.width, h: r.height }; });
await page.screenshot({ path: `${name}.png`, clip: { x: b.x, y: b.y, width: b.w, height: b.h } });
await browser.close();
