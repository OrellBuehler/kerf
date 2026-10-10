import { createRequire } from 'module';
import fs from 'fs';
const require = createRequire(import.meta.url);
export const { chromium } = require('/home/orell/github/bissbilanz/node_modules/playwright/index.js');
export const S = '/tmp/claude-1000/-home-orell-github-kerf/3a822707-e08c-4465-adf8-e4864e1d4032/scratchpad/review/shell';
export const theme = (id) => fs.readFileSync(`${S}/theme-${id}.json`, 'utf8');
export async function open({ w = 1920, h = 1080, dsf = 1, theme: t = 'kerf-dark', q = '' } = {}) {
  const browser = await chromium.launch({ executablePath: '/usr/bin/google-chrome', headless: true, args: ['--no-sandbox'] });
  const ctx = await browser.newContext({ viewport: { width: w, height: h }, deviceScaleFactor: dsf });
  const th = theme(t);
  await ctx.addInitScript((th) => { try { if (!localStorage.getItem('kerf.settings.theme')) localStorage.setItem('kerf.settings.theme', th); } catch {} }, th);
  const page = await ctx.newPage();
  const logs = [];
  page.on('console', (m) => { if (['error','warning'].includes(m.type())) logs.push(m.type() + ': ' + m.text()); });
  page.on('pageerror', (e) => logs.push('pageerror: ' + e.message));
  await page.goto('http://localhost:1420/' + q, { waitUntil: 'load' });
  await page.waitForTimeout(2500);
  return { browser, ctx, page, logs };
}
