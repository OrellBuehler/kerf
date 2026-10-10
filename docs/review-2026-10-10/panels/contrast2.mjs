import { open } from './lib.mjs';
const theme = process.argv[2] || 'dark';
const { browser, page } = await open({ theme });
await page.getByText('Agent', { exact: true }).first().click(); await page.waitForTimeout(400);
const fn = `(sel, nth) => {
  const els = document.querySelectorAll(sel); const el = els[nth||0]; if (!el) return null;
  const cv = document.createElement('canvas'); cv.width = cv.height = 1; const cx = cv.getContext('2d', { willReadFrequently: true });
  const rgba = (s) => { cx.clearRect(0,0,1,1); cx.fillStyle = '#000'; cx.fillStyle = s; cx.fillRect(0,0,1,1); const d = cx.getImageData(0,0,1,1).data; return [d[0], d[1], d[2], d[3]/255]; };
  const over = (fg, bg) => [0,1,2].map(i => fg[i]*fg[3] + bg[i]*(1-fg[3]));
  const chain = []; for (let e = el; e; e = e.parentElement) chain.push(e);
  let bg = [255,255,255,1];
  for (const e of chain.reverse()) { const c = rgba(getComputedStyle(e).backgroundColor); if (c[3] > 0) { const o = over(c, bg); bg = [o[0], o[1], o[2], 1]; } }
  let op = 1; for (let e = el; e; e = e.parentElement) op *= parseFloat(getComputedStyle(e).opacity);
  const cs = getComputedStyle(el); let fg = rgba(cs.color); const fo = over(fg, bg); const f2 = [0,1,2].map(i => fo[i]*op + bg[i]*(1-op));
  const lum = (c) => { const [r,g,b] = c.map(v => { v/=255; return v<=0.03928 ? v/12.92 : Math.pow((v+0.055)/1.055,2.4); }); return 0.2126*r+0.7152*g+0.0722*b; };
  const L1 = lum(f2), L2 = lum(bg); const ratio = (Math.max(L1,L2)+0.05)/(Math.min(L1,L2)+0.05);
  return { text: (el.innerText||'').trim().slice(0,22), fs: cs.fontSize, ratio: Math.round(ratio*100)/100 };
}`;
const L = [
  ['review badge', 'span:has-text("review")'],
  ['Apply button', 'button:has-text("Apply") >> nth=0'],
];
const sels = [
  ['badge review', 'span[style*="display:inline-flex"][style*="height:19px"]', 1],
  ['connection line text', 'div[style*="flex:none;font-size:12px;color:var(--text-secondary)"]', 0],
  ['review section group label (text-disabled 10px)', 'div[style*="text-disabled"][style*="margin-bottom:4px"]', 0],
  ['Preview button', 'button[title="Show the proposed cut in the editor"]', 0],
  ['task remove x (text-disabled)', 'button[title="Remove from queue"]', 0],
  ['quick chip', 'button[title^="Run now"]', 0],
  ['history Revert pill', 'button[title^="Revert"]', 0],
  ['composer hint', 'span[style*="font-size:12px;color:var(--text-muted);line-height:1.4"]', 0],
  ['task meta mono', 'div[data-selectable][style*="font-mono"]', 0],
];
for (const [k, s, n] of sels) console.log(theme, k.padEnd(45), JSON.stringify(await page.evaluate(`(${fn})(${JSON.stringify(s)}, ${n})`)));
await browser.close();
