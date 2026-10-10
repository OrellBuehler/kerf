import pkg from '/home/orell/github/bissbilanz/node_modules/playwright/index.js'; const { chromium } = pkg;
export const DIR = '/tmp/claude-1000/-home-orell-github-kerf/3a822707-e08c-4465-adf8-e4864e1d4032/scratchpad/review/dialogs';
export async function open({ w = 1920, h = 1080, dsf = 1, query = '', theme = null } = {}) {
  const browser = await chromium.launch({ executablePath: '/usr/bin/google-chrome', headless: true, args: ['--no-sandbox'] });
  const ctx = await browser.newContext({ viewport: { width: w, height: h }, deviceScaleFactor: dsf });
  const page = await ctx.newPage();
  const logs = [];
  page.on('console', (m) => { if (['error','warning'].includes(m.type())) logs.push(m.type() + ': ' + m.text()); });
  page.on('pageerror', (e) => logs.push('pageerror: ' + e.message));
  page.on('dialog', async (d) => { logs.push('native dialog: ' + d.type() + ': ' + d.message()); await d.dismiss(); });
  await page.goto('http://localhost:1420/' + (query ? '?' + query : ''));
  await page.waitForTimeout(1500);
  return { browser, ctx, page, logs };
}
