import { open, S } from './lib.mjs';
const { browser, page, logs } = await open({ w: 1920, h: 1080, dsf: 1 });
// select a clip & copy
await page.mouse.click(450, 860);
await page.keyboard.press('Control+c');
await page.waitForTimeout(500);
await page.screenshot({ path: `${S}/toast-1.png`, clip: { x: 1300, y: 760, width: 620, height: 320 } });
// Try file menu -> Save project / Open / Export
for (const [menu, item] of [['File', 'Save project'], ['File', 'Open project'], ['File', 'Import captions']]) {
  await page.getByRole('menuitem', { name: menu, exact: true }).first().click();
  await page.getByRole('menuitem', { name: new RegExp('^' + item) }).first().click();
  await page.waitForTimeout(700);
  await page.screenshot({ path: `${S}/toast-${item.replace(/ /g, '_')}.png` });
  await page.keyboard.press('Escape');
  await page.waitForTimeout(200);
}
console.log(logs);
await browser.close();
