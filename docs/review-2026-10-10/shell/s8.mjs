import { open, S } from './lib.mjs';
for (const [th, os] of [['kerf-dark', 'light'], ['kerf-light', 'dark'], ['kerf-dark', 'dark'], ['kerf-light', 'light']]) {
  const { browser, ctx, page } = await open({ w: 1920, h: 1080, dsf: 2, theme: th });
  await page.emulateMedia({ colorScheme: os });
  await page.reload({ waitUntil: 'load' });
  await page.waitForTimeout(2000);
  await page.locator('button[title="Remove track"]').nth(1).click();
  await page.waitForTimeout(600);
  await page.screenshot({ path: `${S}/toast-undo-${th}-os${os}.png`, clip: { x: 1500, y: 960, width: 420, height: 120 } });
  const info = await page.evaluate(() => { const t = document.querySelector('[data-sonner-toast]'); if (!t) return null; const cs = getComputedStyle(t); const a = t.querySelector('[data-button]'); const acs = a && getComputedStyle(a); return { theme: document.querySelector('[data-sonner-toaster]')?.getAttribute('data-sonner-theme'), bg: cs.backgroundColor, color: cs.color, radius: cs.borderRadius, shadow: cs.boxShadow, border: cs.border, fs: cs.fontSize, btnBg: acs?.backgroundColor, btnColor: acs?.color }; });
  console.log(th, 'OS', os, JSON.stringify(info));
  await browser.close();
}
