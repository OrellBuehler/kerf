import { open, DIR } from './lib.mjs';
const { browser, page, logs } = await open({ w: 1920, h: 1080 });
for (const m of ['File','Edit','View','Playback','Window','Help']) {
  await page.getByRole('menuitem', { name: m, exact: true }).click();
  await page.waitForTimeout(250);
  const items = await page.$$eval('[role=menu] [role=menuitem], [role=menu] [role=menuitemcheckbox], [role=menu] [role=menuitemradio]', (els) => els.map(e => e.textContent.trim().replace(/\s+/g,' ') + (e.getAttribute('aria-disabled')==='true'?' [disabled]':'')));
  console.log('## ' + m + '\n' + items.join('\n'));
  if (m === 'File' || m === 'Help') await page.screenshot({ path: `${DIR}/140-menu-${m}.png`, clip: { x: 0, y: 0, width: 520, height: 420 } });
  await page.keyboard.press('Escape'); await page.keyboard.press('Escape');
}
// focus restore after dialog opened via File > Export
await page.getByRole('menuitem', { name: 'File', exact: true }).click();
await page.getByRole('menuitem', { name: /^Export…/ }).click();
await page.waitForTimeout(500);
await page.keyboard.press('Escape');
await page.waitForTimeout(500);
console.log('focus after export closed via menu:', await page.evaluate(() => document.activeElement?.tagName + ':' + (document.activeElement?.textContent||'').trim().slice(0,20)));
// settings via gear
await page.getByRole('button', { name: /settings/i }).first().click();
await page.waitForTimeout(500);
await page.keyboard.press('Escape'); await page.waitForTimeout(500);
console.log('focus after settings closed via gear:', await page.evaluate(() => document.activeElement?.tagName + ':' + (document.activeElement?.getAttribute('aria-label')||'')));
// About
await page.getByRole('menuitem', { name: 'Help', exact: true }).click();
await page.getByRole('menuitem', { name: /About/ }).click();
await page.waitForTimeout(500);
await page.screenshot({ path: `${DIR}/141-about-toast.png`, clip: { x: 1380, y: 900, width: 540, height: 180 } });
await browser.close();
