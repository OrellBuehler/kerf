import { open } from './lib.mjs';
const theme = process.argv[2] || 'dark';
const { browser, page } = await open({ theme });
await page.mouse.click(400, 858);
await page.waitForTimeout(400);
const fn = `(sel, nth) => {
  const els = document.querySelectorAll(sel); const el = els[nth||0]; if (!el) return null;
  const cv = document.createElement('canvas'); cv.width = cv.height = 1; const cx = cv.getContext('2d', { willReadFrequently: true });
  const rgba = (s) => { cx.clearRect(0,0,1,1); cx.fillStyle = '#000'; cx.fillStyle = s; cx.fillRect(0,0,1,1); const d = cx.getImageData(0,0,1,1).data; 
     // alpha: draw on transparent -> data has alpha premult? getImageData returns non-premult
     return [d[0], d[1], d[2], d[3]/255]; };
  const over = (fg, bg) => [0,1,2].map(i => fg[i]*fg[3] + bg[i]*(1-fg[3]));
  const chain = []; for (let e = el; e; e = e.parentElement) chain.push(e);
  let bg = rgba(getComputedStyle(document.documentElement).backgroundColor); if (bg[3] < 1) bg = [255,255,255,1];
  for (const e of chain.reverse()) { const c = rgba(getComputedStyle(e).backgroundColor); if (c[3] > 0) { const o = over(c, bg); bg = [o[0], o[1], o[2], 1]; } }
  let op = 1; for (let e = el; e; e = e.parentElement) op *= parseFloat(getComputedStyle(e).opacity);
  const cs = getComputedStyle(el); let fg = rgba(cs.color); const fo = over(fg, bg); const f2 = [0,1,2].map(i => fo[i]*op + bg[i]*(1-op));
  const lum = (c) => { const [r,g,b] = c.map(v => { v/=255; return v<=0.03928 ? v/12.92 : Math.pow((v+0.055)/1.055,2.4); }); return 0.2126*r+0.7152*g+0.0722*b; };
  const L1 = lum(f2), L2 = lum(bg); const ratio = (Math.max(L1,L2)+0.05)/(Math.min(L1,L2)+0.05);
  return { text: (el.innerText||'').trim().slice(0,22), fs: cs.fontSize, fw: cs.fontWeight, ratio: Math.round(ratio*100)/100, opacity: op };
}`;
const list = [
  ['analysis chip done', '[data-testid=analysis-chip][data-state=done]'],
  ['analysis chip not_run', '[data-testid=analysis-chip][data-state=not_run]'],
  ['analysis chip off', '[data-testid=analysis-chip][data-state=off]'],
  ['proxy badge building', '[data-testid=proxy-badge] span'],
  ['bin spec line', 'div[role=button][draggable=true] div[title]'],
  ['bin duration', 'div[role=button][draggable=true] span[style*="font-mono"]'],
  ['rail icon inactive', '[data-library-tab="titles"]'],
  ['library header label', '#library-content span[title]'],
  ['inspector read label (Start)', 'summary ~ div span'],
  ['inspector summary value', 'summary .value'],
];
for (const [k, s] of list) console.log(theme, k.padEnd(30), JSON.stringify(await page.evaluate(`(${fn})(${JSON.stringify(s)})`)));
await page.getByText('Inspector', { exact: true }).first().click();
await page.waitForTimeout(300);
for (const [k, s] of [['inspector summary value', 'summary .value'], ['inspector readRow label','details .content span'], ['numRow label','label span']]) console.log(theme, k.padEnd(30), JSON.stringify(await page.evaluate(`(${fn})(${JSON.stringify(s)})`)));
await page.locator('[data-library-tab="transcript"]').click().catch(()=>{});
await page.waitForTimeout(300);
for (const [k, s] of [['transcript timestamp','#library-content span[style*="font-mono"]'],['transcript x','#library-content button[aria-label="Cut line"]'], ['transcript struck line','#library-content button span >> nth=3']]) { try { console.log(theme, k.padEnd(30), JSON.stringify(await page.evaluate(`(${fn})(${JSON.stringify(s.replace(' >> nth=3',''))}, ${s.includes('nth=3')?5:0})`))); } catch(e){ console.log(k, 'err', e.message.slice(0,60)); } }
await page.getByRole('button', { name: 'Audio', exact: true }).click();
await page.waitForTimeout(1000);
for (const [k, s] of [['mixer M toggle', '.tog.mute'], ['mixer pan label', '.pan-label'], ['mixer db readout','.db'], ['mixer peak readout','.peak'], ['mixer overline Ceiling','.cap'], ['mixer hint','.hint']]) console.log(theme, k.padEnd(30), JSON.stringify(await page.evaluate(`(${fn})(${JSON.stringify(s)})`)));
await browser.close();
