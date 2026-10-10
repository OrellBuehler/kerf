import { open, S } from './lib.mjs';
// update chip + dialog
{
  const { browser, page, logs } = await open({ w: 1920, h: 1080, dsf: 2, q: '?update=1' });
  await page.waitForTimeout(1500);
  await page.screenshot({ path: `${S}/update-chip.png`, clip: { x: 1280, y: 0, width: 640, height: 40 } });
  await page.screenshot({ path: `${S}/update-dialog.png` });
  await browser.close();
}
// narrow widths
for (const [w, h] of [[1000, 700], [960, 640]]) {
  const { browser, page } = await open({ w, h, dsf: 1 });
  await page.screenshot({ path: `${S}/narrow-${w}.png` });
  await browser.close();
}
// New project (empty state)
{
  const { browser, page } = await open({ w: 1920, h: 1080, dsf: 1 });
  await page.getByRole('menuitem', { name: 'File', exact: true }).first().click();
  await page.getByRole('menuitem', { name: /^New project/ }).first().click();
  await page.waitForTimeout(1200);
  await page.screenshot({ path: `${S}/empty-project.png` });
  await browser.close();
}
