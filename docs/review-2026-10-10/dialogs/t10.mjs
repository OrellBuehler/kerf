import { open, DIR } from './lib.mjs';
for (const [w,h] of [[1920,1080],[1366,768]]) {
const { browser, page, logs } = await open({ w, h });
const dlg = async (n) => { await page.waitForTimeout(300); const el = await page.$('[role=dialog] > div'); await el.screenshot({ path: `${DIR}/${n}.png` }); };
await page.getByRole('button', { name: /Voiceover/ }).first().click();
await page.waitForTimeout(700);
await dlg(`70-vo-empty-${w}`);
if (w === 1920) {
console.log('active:', await page.evaluate(() => document.activeElement?.tagName));
await page.locator('textarea').fill('Welcome to Kerf. This is a short script to test the voiceover dialog. It has a few sentences, and a long tail that goes on.\n\nA second paragraph for a pause.');
await dlg('71-vo-filled');
await page.getByRole('button', { name: 'Generate' }).click();
await page.waitForTimeout(500);
await dlg('72-vo-running-download');
await page.waitForTimeout(1000);
await dlg('73-vo-running-synth');
await page.getByRole('button', { name: 'Cancel' }).click();
await page.waitForTimeout(600);
await dlg('74-vo-after-cancel');
await page.getByRole('button', { name: 'Generate' }).click();
await page.waitForTimeout(6000);
await page.screenshot({ path: `${DIR}/75-vo-done.png` });
}
console.log(logs.filter(l=>!l.includes('404')).join('\n'));
await browser.close();
}
