import { open } from './lib.mjs';
const { browser, page } = await open({ dsf: 2 });
await page.mouse.click(400, 858);
await page.waitForTimeout(400);
await page.getByText('Inspector', { exact: true }).first().click();
await page.waitForTimeout(300);
await page.evaluate(() => document.querySelectorAll('details').forEach(d => d.open = true));
await page.setViewportSize({ width: 1920, height: 3300 });
await page.waitForTimeout(800);
const pos = await page.evaluate(() => {
  const out = {};
  for (const s of document.querySelectorAll('details > summary')) { const r = s.getBoundingClientRect(); out[s.innerText.split('\n')[0].trim()] = { x: r.x, y: r.y, w: r.width, h: r.height }; }
  return out;
});
console.log(JSON.stringify(pos));
// Transform + Color adjacent crops
const t = pos['Transform'], c = pos['Color'];
await page.screenshot({ path: 'c-transform.png', clip: { x: 1467, y: t.y - 6, width: 453, height: 180 } });
await page.screenshot({ path: 'c-color.png', clip: { x: 1467, y: c.y - 6, width: 453, height: 230 } });
const ti = pos['Timing'];
await page.screenshot({ path: 'c-timing.png', clip: { x: 1467, y: ti.y - 6, width: 453, height: 230 } });
const th = await page.evaluate(() => {
  const sl = [...document.querySelectorAll('input[type=range]')].map(i => { const r = i.getBoundingClientRect(); return [i.closest('label')?.innerText.trim().split('\n')[0].slice(0,14), Math.round(r.x), Math.round(r.right)]; });
  return sl;
});
console.log(JSON.stringify(th));
await browser.close();
