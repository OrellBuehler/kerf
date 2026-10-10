import { open } from './lib.mjs';
const { browser, page } = await open();
console.log(await page.evaluate(() => ({
  geist: document.fonts.check('12px Geist'), mono: document.fonts.check('12px "Geist Mono"'),
  ff: getComputedStyle(document.body).fontFamily,
  loaded: [...document.fonts].map(f => f.family + ':' + f.status),
})));
console.log(await page.evaluate(() => [...document.styleSheets].map(s => { try { return [...s.cssRules].filter(r=>r.type===3).map(r=>r.href) } catch(e){return 'x'} })));
await browser.close();
