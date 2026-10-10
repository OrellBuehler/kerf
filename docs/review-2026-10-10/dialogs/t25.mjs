import { open, DIR } from './lib.mjs';
const { browser, page, logs } = await open({ w: 1920, h: 1080 });
const btn = page.getByRole('button', { name: /Voiceover/ }).first();
await btn.focus();
await btn.press('Enter');
await page.waitForTimeout(500);
// tab around: does focus leave the dialog?
const seq = [];
for (let i = 0; i < 14; i++) { await page.keyboard.press('Tab'); seq.push(await page.evaluate(() => { const a = document.activeElement; return (a?.closest('[role=dialog]') ? 'in:' : 'OUT:') + a?.tagName + ':' + (a?.textContent||'').trim().slice(0,14); })); }
console.log(seq.join(' | '));
await page.keyboard.press('Escape');
await page.waitForTimeout(500);
console.log('after Esc:', await page.evaluate(() => document.activeElement?.tagName + ':' + (document.activeElement?.textContent||'').trim().slice(0,20)));
// Settings: chips a11y
await page.keyboard.press('Control+,'); await page.waitForTimeout(400);
const a11y = await page.evaluate(() => [...document.querySelectorAll('[role=dialog] button')].filter(b => ['Background','Balanced','Full speed'].includes(b.textContent.trim())).map(b => b.textContent.trim() + ' pressed=' + b.getAttribute('aria-pressed') + ' role=' + b.getAttribute('role')));
console.log(a11y.join(' | '));
const rail = await page.evaluate(() => [...document.querySelectorAll('[role=dialog] button')].filter(b => b.textContent.trim() === 'Performance').map(b => 'rail role=' + b.getAttribute('role') + ' aria-current=' + b.getAttribute('aria-current')));
console.log(rail.join(' | '));
await browser.close();
