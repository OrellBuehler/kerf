import { open } from './lib.mjs';
const { browser, page } = await open({ theme: 'light', dsf: 2 });
await page.getByRole('button', { name: 'Audio', exact: true }).click();
await page.waitForTimeout(1200);
const info = await page.evaluate(() => [...document.querySelectorAll('.dv-groupview')].map(g => { const r = g.getBoundingClientRect(); return { x: r.x, y: r.y, w: r.width, h: r.height, t: g.innerText.slice(0,20).replace(/\n/g,' ') }; }));
const m = info.find(i => i.t.startsWith('Mixer'));
await page.screenshot({ path: 'l-mixer.png', clip: { x: m.x, y: m.y, width: m.w, height: m.h } });
await browser.close();
