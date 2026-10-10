import { open, S } from './lib.mjs';
const { browser, page } = await open({ w: 1920, h: 1080, dsf: 2 });
const q = async (sel, nth = 0) => page.evaluate(([sel, nth]) => {
  const el = document.querySelectorAll(sel)[nth]; if (!el) return 'MISSING';
  const cs = getComputedStyle(el); const r = el.getBoundingClientRect();
  return { cursor: cs.cursor, fs: cs.fontSize, fw: cs.fontWeight, ff: cs.fontFamily.slice(0, 20), color: cs.color, bg: cs.backgroundColor, w: r.width, h: r.height, pad: cs.padding, tr: cs.transition.slice(0, 40), ls: cs.letterSpacing };
}, [sel, nth]);
const out = {};
for (const [k, sel, n] of [
  ['dock tab', '.dv-tab', 0], ['dock tab close', '.dv-default-tab-action', 0], ['dock tabs strip', '.dv-tabs-and-actions-container', 0],
  ['sash', '.dv-sash', 0], ['menu top', '.kerf-menu-top', 0], ['ws tab', '[data-workspace]', 0], ['rail btn', '[role=tablist] button', 0],
  ['gear', 'button[aria-label=Settings]', 0], ['version chip', 'button[title^="Kerf"]', 0],
]) out[k] = await q(sel, n);
console.log(JSON.stringify(out, null, 1));
// html structure of a dock tab
console.log(await page.evaluate(() => document.querySelector('.dv-tab').outerHTML.slice(0, 600)));
console.log(await page.evaluate(() => { const s = [...document.querySelectorAll('.dv-sash')].slice(0,3).map(e => { const r = e.getBoundingClientRect(); const cs = getComputedStyle(e); return [r.x, r.y, r.width, r.height, cs.cursor, cs.backgroundColor, cs.zIndex].join(' '); }); return s.join('\n'); }));
await browser.close();
