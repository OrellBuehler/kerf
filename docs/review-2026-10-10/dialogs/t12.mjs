import { open, DIR } from './lib.mjs';
for (const [w,h] of [[1920,1080],[1366,768]]) {
const { browser, page, logs } = await open({ w, h, query: 'music=1' });
const dlg = async (n) => { await page.waitForTimeout(400); const el = await page.$('[role=dialog] > div'); await el.screenshot({ path: `${DIR}/${n}.png` }); };
const clip = page.locator('[data-clip-id="c4"]');
await clip.click({ button: 'right', position: { x: 200, y: 20 } });
await page.waitForTimeout(400);
await page.screenshot({ path: `${DIR}/81-music-contextmenu-${w}.png` });
const items = await page.$$eval('[role=menu] [role=menuitem], [role=menu] button', (els) => els.map(e => e.textContent.trim().replace(/\s+/g,' ')));
console.log(w, items.join(' | '));
await page.getByRole('menuitem', { name: 'Fit to video…' }).click();
await dlg(`82-fit-default-${w}`);
if (w === 1920) {
  await page.getByRole('button', { name: 'Custom' }).click();
  await dlg('83-fit-custom');
  const inp = page.getByLabel('Length in seconds');
  await inp.fill('abc'); await dlg('84-fit-badnum');
  await inp.fill('3'); await dlg('85-fit-3s');
  await inp.fill('45.5'); await dlg('86-fit-45');
  await inp.fill('200'); await dlg('87-fit-200');
  await inp.fill('0'); await dlg('88-fit-0');
  // tab order
  const out = [];
  for (let i=0;i<8;i++){ await page.keyboard.press('Tab'); out.push(await page.evaluate(()=>{const a=document.activeElement;return a.tagName+':'+(a.getAttribute('aria-label')||a.textContent.trim().slice(0,25))})); }
  console.log(out.join(' | '));
}
console.log(logs.filter(l=>!l.includes('404')).join('\n'));
await browser.close();
}
