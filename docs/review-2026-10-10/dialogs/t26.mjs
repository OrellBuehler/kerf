import { open, DIR } from './lib.mjs';
const { browser, page, logs } = await open({ w: 1920, h: 1080, dsf: 2 });
await page.keyboard.press('Control+e'); await page.waitForTimeout(600);
const box = await (await page.$('[role=dialog] > div')).boundingBox();
await page.screenshot({ path: `${DIR}/190-export-2x-top.png`, clip: { x: box.x, y: box.y, width: 600, height: 330 } });
await page.screenshot({ path: `${DIR}/191-export-2x-mid.png`, clip: { x: box.x, y: box.y + 480, width: 600, height: 290 } });
await page.screenshot({ path: `${DIR}/192-export-2x-bottom.png`, clip: { x: box.x, y: box.y + box.height - 160, width: 600, height: 160 } });
await browser.close();
