import { open, S } from './lib.mjs';
const { browser, page, logs } = await open({});
await page.screenshot({ path: `${S}/01-dark-1920.png` });
console.log(logs.join('\n'));
console.log(await page.evaluate(() => getComputedStyle(document.body).fontFamily + ' | ' + document.fonts.size + ' | ' + [...document.fonts].map(f=>f.family+':'+f.status).join(',')));
await browser.close();
