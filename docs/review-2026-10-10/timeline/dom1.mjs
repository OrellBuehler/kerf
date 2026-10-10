import { launch, open, shot } from './lib.mjs';
const b = await launch();
const { p } = await open(b, { q: '?music=1' });
const info = await p.evaluate(() => {
  const r = (el) => { if (!el) return null; const b = el.getBoundingClientRect(); return { x: Math.round(b.x), y: Math.round(b.y), w: Math.round(b.width), h: Math.round(b.height) }; };
  const clips = [...document.querySelectorAll('.kclip')].map(e => ({ id: e.dataset.clipId, ...r(e), txt: e.innerText.slice(0,30) }));
  const lanes = [...document.querySelectorAll('[data-lane]')].map(e => ({ id: e.dataset.trackId, kind: e.dataset.kind, ...r(e) }));
  const tl = [...document.querySelectorAll('div')].find(d => d.textContent.startsWith('Timeline') && d.className.includes('dv'))
  return { clips, lanes, ruler: r(document.querySelector('[data-lane]')?.parentElement), scrollW: document.documentElement.scrollWidth, win: [innerWidth, innerHeight] };
});
console.log(JSON.stringify(info, null, 1));
console.log(p.logs);
await b.close();
