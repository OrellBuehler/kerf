import { open, DIR } from './lib.mjs';
const { browser, page, logs } = await open({ w: 1920, h: 1080 });
await page.getByRole('button', { name: /Voiceover/ }).first().click();
await page.locator('textarea').fill('A long script I typed carefully.');
const ring = await page.evaluate(() => { const t = document.querySelector('textarea'); const cs = getComputedStyle(t); return { outline: cs.outline, border: cs.border, boxShadow: cs.boxShadow }; });
console.log('textarea focus:', JSON.stringify(ring));
const sel = await page.evaluate(() => { document.querySelector('textarea').blur(); const b = document.querySelector('[role=dialog] select'); b.focus(); const cs = getComputedStyle(b); return { outline: cs.outline }; });
console.log('select focus:', JSON.stringify(sel));
await page.mouse.click(30, 500);
await page.waitForTimeout(400);
console.log('voiceover dialog open after scrim click:', await page.locator('[role=dialog]').count());
await page.getByRole('button', { name: /Voiceover/ }).first().click();
console.log('script after reopen:', JSON.stringify(await page.locator('textarea').inputValue()));
await page.keyboard.press('Escape');
// Export: scrim click
await page.keyboard.press('Control+e'); await page.waitForTimeout(400);
await page.locator('[role=dialog] select').first().selectOption('gif');
await page.mouse.click(30, 500); await page.waitForTimeout(300);
console.log('export open after scrim click:', await page.locator('[role=dialog]').count());
await page.keyboard.press('Control+e'); await page.waitForTimeout(400);
console.log('preset after reopen:', await page.locator('[role=dialog] select').first().inputValue());
// close button size
const btn = await page.evaluate(() => { const b = document.querySelector('[role=dialog] button'); const r = b.getBoundingClientRect(); return { w: r.width, h: r.height, text: b.textContent, aria: b.getAttribute('aria-label'), fs: getComputedStyle(b).fontSize }; });
console.log('close btn', JSON.stringify(btn));
await browser.close();
