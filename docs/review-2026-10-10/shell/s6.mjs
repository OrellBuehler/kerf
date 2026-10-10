import { open, S } from './lib.mjs';
import fs from 'fs';
const th = process.argv[2] || 'kerf-dark';
const { browser, page } = await open({ w: 1920, h: 1080, dsf: 2, theme: th });
await page.mouse.click(960, 400); // click into preview area to start tab order from there? use body
await page.evaluate(() => { document.activeElement && document.activeElement.blur(); window.scrollTo(0,0); });
await page.keyboard.press('Tab');
const rows = [];
for (let i = 0; i < 60; i++) {
  const info = await page.evaluate(() => {
    const el = document.activeElement;
    if (!el || el === document.body) return null;
    const r = el.getBoundingClientRect();
    const cs = getComputedStyle(el);
    return { tag: el.tagName, role: el.getAttribute('role'), label: el.getAttribute('aria-label') || el.getAttribute('title') || el.innerText?.slice(0, 30), x: r.x, y: r.y, w: r.width, h: r.height, outline: cs.outlineStyle + ' ' + cs.outlineWidth + ' ' + cs.outlineColor + ' off ' + cs.outlineOffset, shadow: cs.boxShadow, cls: el.className?.toString?.().slice(0, 40) };
  });
  if (!info) { rows.push({ i, none: true }); await page.keyboard.press('Tab'); continue; }
  rows.push({ i, ...info });
  const pad = 14;
  const clip = { x: Math.max(0, info.x - pad), y: Math.max(0, info.y - pad), width: Math.min(1920, Math.max(60, info.w + pad * 2)), height: Math.min(1080 - Math.max(0, info.y - pad), Math.max(40, info.h + pad * 2)) };
  if (clip.x + clip.width > 1920) clip.width = 1920 - clip.x;
  try { await page.screenshot({ path: `${S}/focus/${th}-${String(i).padStart(2, '0')}.png`, clip }); } catch (e) {}
  await page.keyboard.press('Tab');
}
fs.writeFileSync(`${S}/focus/${th}-rows.json`, JSON.stringify(rows, null, 1));
for (const r of rows) console.log(r.i, r.tag, r.role || '', JSON.stringify(r.label), Math.round(r.x || 0), Math.round(r.y || 0), '|', r.outline, '|', (r.shadow || '').slice(0, 60));
await browser.close();
