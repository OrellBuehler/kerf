import { open, DIR } from './lib.mjs';
const { browser, page, logs } = await open({ w: 1920, h: 1080 });
const shot = async (n, sel = '[role=dialog] > div') => { await page.waitForTimeout(250); const el = await page.$(sel); await el.screenshot({ path: `${DIR}/${n}.png` }); };
await page.keyboard.press('Control+e');
await page.waitForTimeout(500);
const presets = await page.$$eval('[role=dialog] select option', (os) => os.slice(0, 14).map(o => o.value + '=' + o.textContent));
console.log(presets.join('\n'));
const sel = page.locator('[role=dialog] select').first();
for (const id of ['youtube_4k','prores_master','target_size_2pass','vertical_reel','gif','audio_mp3','audio_wav','av1_web']) {
  await sel.selectOption(id);
  await shot('20-export-preset-' + id);
}
// audio_mp3 with advanced open
await sel.selectOption('web_1080p');
// custom resolution
await page.locator('[role=dialog] select').nth(1).selectOption('custom');
await shot('21-export-customres');
await page.locator('[role=dialog] select').nth(1).selectOption('source');
await shot('21b-export-projectframe');
// Deliver to ticks
await page.getByRole('button', { name: '9:16', exact: true }).click();
await page.getByRole('button', { name: '1:1', exact: true }).click();
await shot('22-export-deliver-two');
console.log(logs.join('\n'));
await browser.close();
