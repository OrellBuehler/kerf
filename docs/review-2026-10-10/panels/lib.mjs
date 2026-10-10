import pkg from '/home/orell/github/bissbilanz/node_modules/playwright/index.js';
const { chromium } = pkg;
export async function open({ w = 1920, h = 1080, dsf = 1, theme = 'dark', q = '?music=1&staged=1' } = {}) {
  const browser = await chromium.launch({ executablePath: '/usr/bin/google-chrome', headless: true });
  const ctx = await browser.newContext({ viewport: { width: w, height: h }, deviceScaleFactor: dsf });
  const page = await ctx.newPage();
  page.on('pageerror', e => console.log('PAGEERR', e.message));
  await page.goto('http://localhost:1420/' + q);
  await page.waitForSelector('text=Library', { timeout: 30000 });
  await page.waitForTimeout(1500);
  if (theme === 'light') await setLight(page);
  return { browser, page };
}
export async function setLight(page) {
  await page.keyboard.press('Control+,');
  await page.waitForTimeout(600);
  const btn = page.getByText('Appearance', { exact: true }).first();
  await btn.click();
  await page.waitForTimeout(300);
  await page.getByRole('button', { name: /Kerf Light/ }).first().click().catch(async () => { await page.getByText('Kerf Light').first().click(); });
  await page.waitForTimeout(500);
  await page.keyboard.press('Escape');
  await page.waitForTimeout(500);
}
export async function tab(page, label) { await page.locator(`[data-library-tab]`).filter({ has: page.locator('svg') }).locator(`xpath=self::*[@aria-label="${label}"]`).click(); await page.waitForTimeout(400); }
export async function panelClip(page, sel) {
  const b = await page.locator(sel).first().boundingBox();
  return b;
}
