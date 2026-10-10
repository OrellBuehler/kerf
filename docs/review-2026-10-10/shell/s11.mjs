import { open, S } from './lib.mjs';
const th = process.argv[2] || 'kerf-dark';
const { browser, page } = await open({ w: 1920, h: 1080, dsf: 2, theme: th });
const hov = async (name, sel, clip, n = 0) => {
  const loc = page.locator(sel).nth(n);
  await loc.hover();
  await page.waitForTimeout(450);
  await page.screenshot({ path: `${S}/hover/${th}-${name}.png`, clip });
  await page.mouse.move(960, 500);
};
await hov('menu-top', '.kerf-menu-top', { x: 20, y: 0, width: 200, height: 36 }, 1);
await hov('ws-tab', '[data-workspace="color"]', { x: 800, y: 0, width: 320, height: 36 });
await hov('gear', 'button[aria-label=Settings]', { x: 1780, y: 0, width: 140, height: 36 });
await hov('version', 'button[title^="Kerf"]', { x: 1780, y: 0, width: 140, height: 36 });
await hov('dock-tab-inactive', '.dv-tab', { x: 1440, y: 36, width: 320, height: 40 }, 2);
await hov('dock-close', '.dv-default-tab-action', { x: 0, y: 36, width: 200, height: 40 }, 0);
await hov('rail', '[role=tablist] button', { x: 0, y: 70, width: 60, height: 200 }, 2);
// sash hover
const box = await page.locator('.dv-sash').nth(1).boundingBox();
await page.mouse.move(box.x + 2, box.y + 300);
await page.waitForTimeout(600);
await page.screenshot({ path: `${S}/hover/${th}-sash.png`, clip: { x: 340, y: 30, width: 130, height: 200 } });
await page.screenshot({ path: `${S}/hover/${th}-sash-wide.png`, clip: { x: 0, y: 30, width: 960, height: 640 } });
await browser.close();
