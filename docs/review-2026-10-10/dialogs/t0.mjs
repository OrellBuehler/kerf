import { open, DIR } from './lib.mjs';
const { browser, page, logs } = await open();
await page.screenshot({ path: DIR + '/00-initial.png' });
console.log(logs.join('\n'));
await browser.close();
