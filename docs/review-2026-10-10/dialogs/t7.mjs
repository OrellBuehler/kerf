import { open, DIR } from './lib.mjs';
const { browser, page, logs } = await open({ w: 1920, h: 1080 });
await page.keyboard.press('Control+,');
await page.waitForTimeout(500);
await page.locator('[role=dialog] button', { hasText: /^\s*Keyboard\s*$/ }).click();
await page.waitForTimeout(300);
const dlg = async (n) => { await page.waitForTimeout(250); await (await page.$('[role=dialog] > div')).screenshot({ path: `${DIR}/${n}.png` }); };
await page.locator('[data-key-row="file.new"]').first().click();
await dlg('50-kbd-recording');
await page.keyboard.press('Control+o');
await dlg('51-kbd-conflict');
console.log('active after conflict:', await page.evaluate(() => document.activeElement?.outerHTML.slice(0, 120)));
// cancel
await page.getByRole('button', { name: 'Cancel' }).first().click();
await page.locator('[data-key-row="file.new"]').first().click();
await page.keyboard.press('Control+Shift+9');
await dlg('52-kbd-changed');
// Reset all
await page.locator('[data-key-reset-all]').click();
await dlg('53-kbd-resetall');
console.log('active:', await page.evaluate(() => document.activeElement?.outerHTML.slice(0, 160)));
await page.keyboard.press('Escape');
// search no results
await page.locator('[data-key-search]').fill('zzzz');
await dlg('54-kbd-noresult');
await page.locator('[data-key-search]').fill('ripple');
await dlg('55-kbd-search');
await browser.close();
