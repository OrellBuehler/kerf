import { open } from './lib.mjs';
const { browser, page } = await open({});
console.log(await page.locator('button[title="Import"]').getAttribute('aria-pressed'));
// rows of the bin: role=button containing nested role=listitem etc
console.log(await page.evaluate(() => { const r = document.querySelector('div[role=button][draggable=true]'); return { role: r.getAttribute('role'), nestedBtns: r.querySelectorAll('button').length, label: r.getAttribute('aria-label'), pressed: r.getAttribute('aria-pressed'), selected: r.getAttribute('aria-selected'), current: r.getAttribute('aria-current') }; }));
// does Space key select a bin row?
await page.locator('div[role=button][draggable=true]').nth(1).focus();
await page.keyboard.press('Space');
await page.waitForTimeout(200);
console.log('after space, selected border:', await page.evaluate(() => [...document.querySelectorAll('div[role=button][draggable=true]')].map(r => getComputedStyle(r).borderColor)));
await page.keyboard.press('Enter');
await page.waitForTimeout(200);
console.log('after enter, selected border:', await page.evaluate(() => [...document.querySelectorAll('div[role=button][draggable=true]')].map(r => getComputedStyle(r).borderColor)));
// summary value float / marker check
console.log(await page.evaluate(() => { const s = document.querySelector('details > summary'); const c = getComputedStyle(s); return [c.display, c.listStyleType]; }));
await browser.close();
