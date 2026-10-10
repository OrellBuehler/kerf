import { open } from './lib.mjs';
const { browser, page } = await open({});
const count = () => page.evaluate(() => {
  const out = {};
  const w = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
  let n; while ((n = w.nextNode())) { if (!n.nodeValue.trim()) continue; const e = n.parentElement; const r = e.getBoundingClientRect(); if (!r.width) continue; const g = e.closest('.dv-groupview'); if (!g) continue; const key = g.innerText.slice(0,7).trim().split('\n')[0]; const fs = getComputedStyle(e).fontSize; (out[key] ||= {})[fs] = ((out[key] ||= {})[fs] || 0) + 1; }
  return out;
});
await page.mouse.click(400, 858); await page.waitForTimeout(300);
console.log(JSON.stringify(await count()));
await page.getByRole('button', { name: 'Audio', exact: true }).click(); await page.waitForTimeout(900);
console.log(JSON.stringify(await count()));
const m = await page.evaluate(() => { const l = document.querySelector('[aria-label$="level meter"]'); const r = l.getBoundingClientRect(); return [r.width, r.height]; });
console.log('meter', m);
await browser.close();
