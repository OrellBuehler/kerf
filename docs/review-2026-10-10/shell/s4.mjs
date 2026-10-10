import { open, S } from './lib.mjs';
const { browser, page } = await open({ w: 1920, h: 1080, dsf: 2 });
const names = ['File', 'Edit', 'View', 'Playback', 'Window', 'Help'];
for (const n of names) {
  await page.getByRole('menuitem', { name: n, exact: true }).first().click();
  await page.waitForTimeout(250);
  await page.screenshot({ path: `${S}/menu-${n}.png`, clip: { x: 0, y: 0, width: 520, height: 700 } });
  await page.keyboard.press('Escape');
  await page.waitForTimeout(100);
}
// Edit menu with submenu open
await page.getByRole('menuitem', { name: 'Edit', exact: true }).first().click();
await page.waitForTimeout(200);
const tool = page.getByRole('menuitem', { name: /^Tool/ }).first();
await tool.hover();
await page.waitForTimeout(400);
await page.screenshot({ path: `${S}/menu-Edit-sub.png`, clip: { x: 0, y: 0, width: 760, height: 560 } });
// metrics
const m = await page.evaluate(() => {
  const items = [...document.querySelectorAll('.kerf-menu-item')];
  return items.slice(0, 5).map(i => ({ h: i.getBoundingClientRect().height, fs: getComputedStyle(i).fontSize, text: i.innerText.replace(/\n/g,'|') }));
});
console.log(JSON.stringify(m));
await browser.close();
