import { open, S } from './lib.mjs';
const { browser, page } = await open({ w: 1920, h: 1080, dsf: 2 });
await page.locator('button[title="Remove track"]').nth(1).click();
await page.waitForTimeout(700);
const info = await page.evaluate(() => {
  const t = document.querySelector('[data-sonner-toast]'); const r = t.getBoundingClientRect();
  const tt = document.querySelector('[data-sonner-toaster]'); const tr = tt.getBoundingClientRect();
  const sb = [...document.querySelectorAll('div')].find(d => getComputedStyle(d).height === '26px' && d.textContent.includes('assets'));
  return { toast: [r.x, r.y, r.width, r.height, r.bottom], toaster: [tr.x, tr.y, tr.width, tr.height], sb: sb && [sb.getBoundingClientRect().y], zi: getComputedStyle(tt).zIndex, vars: ['--offset', '--mobile-offset'].map(v => getComputedStyle(tt).getPropertyValue(v)) };
});
console.log(JSON.stringify(info));
// bell + notification panel
await page.locator('[data-notification-bell]').click();
await page.waitForTimeout(400);
await page.screenshot({ path: `${S}/notif-panel.png`, clip: { x: 1400, y: 0, width: 520, height: 420 } });
await page.screenshot({ path: `${S}/notif-bell-crop.png`, clip: { x: 1780, y: 0, width: 140, height: 36 } });
await browser.close();
