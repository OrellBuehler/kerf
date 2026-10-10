import { open } from './lib.mjs';
const { browser, page } = await open({});
// layout info
const info = await page.evaluate(() => [...document.querySelectorAll('.dv-groupview')].map(g => { const r = g.getBoundingClientRect(); return { x: r.x, y: r.y, w: r.width, h: r.height, t: g.innerText.slice(0,30).replace(/\n/g,' ') }; }));
console.log(JSON.stringify(info));
for (const t of ['Media','Titles','Effects','Transitions','Audio','Transcript']) {
  await page.locator(`[data-library-tab][aria-label="${t}"]`).click();
  await page.mouse.move(700, 400);
  await page.waitForTimeout(400);
  await page.screenshot({ path: `lib-${t.toLowerCase()}-noclip.png`, clip: { x: 0, y: 36, width: 400, height: 630 } });
}
await browser.close();
