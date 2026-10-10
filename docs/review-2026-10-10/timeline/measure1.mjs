import { launch, open } from './lib.mjs';
const b = await launch();
const { p } = await open(b, { q: '?music=1', dsf: 1 });
const out = await p.evaluate(() => {
  const tb = [...document.querySelectorAll('[role=toolbar]')].find(e => e.getAttribute('aria-label') === 'Edit tools').parentElement;
  const rows = [...tb.querySelectorAll('button, input[type=range], span[title]')].map(e => { const r = e.getBoundingClientRect(); const cs = getComputedStyle(e); return `${(e.getAttribute('aria-label') || e.title || e.textContent).slice(0, 24).padEnd(26)} ${Math.round(r.width)}x${Math.round(r.height)}  fs=${cs.fontSize} rad=${cs.borderRadius}`; });
  const hdr = [...document.querySelectorAll('button[aria-label^="Toggle"], button[aria-label="Remove track"]')].slice(0, 6).map(e => { const r = e.getBoundingClientRect(); return `${e.getAttribute('aria-label')} ${Math.round(r.width)}x${Math.round(r.height)}`; });
  const sc = document.querySelector('[data-lane]').parentElement.parentElement;
  return { rows, hdr, scroller: { clientH: sc.clientHeight, scrollH: sc.scrollHeight }, hdrW: document.querySelector('[data-lane]').parentElement.previousElementSibling.getBoundingClientRect().width };
});
console.log(out.rows.join('\n')); console.log(out.hdr, out.scroller, out.hdrW);
await b.close();
