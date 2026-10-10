import pkg from '/home/orell/github/bissbilanz/node_modules/playwright/index.js';
const { chromium } = pkg;
export const DIR = '/tmp/claude-1000/-home-orell-github-kerf/3a822707-e08c-4465-adf8-e4864e1d4032/scratchpad/review/timeline/';
export async function launch() {
  return chromium.launch({ executablePath: '/usr/bin/google-chrome', headless: true });
}
export async function open(b, { w = 1920, h = 1080, dsf = 1, q = '', theme = 'dark', pre } = {}) {
  const ctx = await b.newContext({ viewport: { width: w, height: h }, deviceScaleFactor: dsf });
  const p = await ctx.newPage();
  p.logs = [];
  p.on('console', (m) => { if (m.type() === 'error' || m.type() === 'warning') p.logs.push(m.type() + ': ' + m.text()); });
  p.on('pageerror', (e) => p.logs.push('PAGEERROR ' + e.message));
  await p.goto('http://localhost:1420/' + q, { waitUntil: 'networkidle' });
  if (theme === 'light') {
    await p.evaluate(async () => {
      const m = await import('/src/lib/theme.ts');
      localStorage.setItem('kerf.settings.theme', JSON.stringify(m.PRESETS['kerf-light']));
    });
    await p.reload({ waitUntil: 'networkidle' });
  }
  if (pre) await pre(p);
  await p.waitForTimeout(1200);
  return { ctx, p };
}
export const shot = (p, name, clip) => p.screenshot({ path: DIR + name + '.png', ...(clip ? { clip } : {}) });
export async function box(p, sel) {
  const el = p.locator(sel).first();
  const bb = await el.boundingBox();
  return bb;
}
export const pad = (bb, l = 0, t = 0, r = 0, btm = 0) => ({ x: Math.max(0, bb.x - l), y: Math.max(0, bb.y - t), width: bb.width + l + r, height: bb.height + t + btm });
