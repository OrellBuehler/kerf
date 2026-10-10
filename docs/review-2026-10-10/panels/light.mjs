import { open } from './lib.mjs';
const { browser, page } = await open({ theme: 'light' });
await page.screenshot({ path: 'light-full.png' });
console.log(await page.evaluate(() => document.documentElement.className + ' ' + getComputedStyle(document.body).backgroundColor));
await browser.close();
