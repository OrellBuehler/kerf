import pkg from '/home/orell/github/bissbilanz/node_modules/playwright/index.js'; const { chromium } = pkg;
const b = await chromium.launch({ executablePath: '/usr/bin/google-chrome', headless: true });
const out = process.argv[2] || 'a';
const variants = [
  ['dark-1920', 1920, 1080, 1, ''],
  ['dark-1366', 1366, 768, 1, ''],
  ['music-1920', 1920, 1080, 1, '?music=1'],
  ['staged-1920', 1920, 1080, 1, '?staged=1'],
];
for (const [name, w, h, dsf, q] of variants) {
  const ctx = await b.newContext({ viewport: { width: w, height: h }, deviceScaleFactor: dsf });
  const p = await ctx.newPage();
  const logs = [];
  p.on('console', m => logs.push(m.type() + ': ' + m.text()));
  p.on('pageerror', e => logs.push('PAGEERROR ' + e.message));
  await p.goto('http://localhost:1420/' + q, { waitUntil: 'networkidle' });
  await p.waitForTimeout(1500);
  await p.screenshot({ path: `${out}-${name}.png` });
  console.log(name, logs.slice(0, 8));
  await ctx.close();
}
await b.close();
