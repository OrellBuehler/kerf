import { open, DIR } from './lib.mjs';
const { browser, page, logs } = await open({ w: 1920, h: 1080, query: 'music=1' });
await page.screenshot({ path: `${DIR}/80-music-app.png` });
// find the clip on A2
const clips = await page.$$eval('[data-clip-id]', (els) => els.map(e => ({ id: e.getAttribute('data-clip-id'), t: e.textContent.slice(0, 40), r: e.getBoundingClientRect().toJSON() })));
console.log(JSON.stringify(clips, null, 0));
await browser.close();
