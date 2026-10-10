import { open, S } from './lib.mjs';
// base screenshots across themes/sizes + 2x crops
for (const th of ['kerf-dark', 'kerf-light', 'high-contrast']) {
  for (const [w, h] of [[1920, 1080], [1366, 768]]) {
    const { browser, page } = await open({ w, h, theme: th });
    await page.screenshot({ path: `${S}/base-${th}-${w}.png` });
    await browser.close();
  }
}
// 2x crops
for (const th of ['kerf-dark', 'kerf-light']) {
  const { browser, page } = await open({ w: 1920, h: 1080, dsf: 2, theme: th });
  await page.screenshot({ path: `${S}/crop-titlebar-left-${th}.png`, clip: { x: 0, y: 0, width: 640, height: 80 } });
  await page.screenshot({ path: `${S}/crop-titlebar-center-${th}.png`, clip: { x: 640, y: 0, width: 640, height: 80 } });
  await page.screenshot({ path: `${S}/crop-titlebar-right-${th}.png`, clip: { x: 1280, y: 0, width: 640, height: 80 } });
  await page.screenshot({ path: `${S}/crop-statusbar-${th}.png`, clip: { x: 1280, y: 1050, width: 640, height: 30 } });
  await page.screenshot({ path: `${S}/crop-statusbar-left-${th}.png`, clip: { x: 0, y: 1050, width: 640, height: 30 } });
  await page.screenshot({ path: `${S}/crop-library-${th}.png`, clip: { x: 0, y: 36, width: 420, height: 460 } });
  await browser.close();
}
