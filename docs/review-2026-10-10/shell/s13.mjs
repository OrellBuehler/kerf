import { open, S } from './lib.mjs';
const { browser, page } = await open({ w: 1920, h: 1080 });
const btns = await page.evaluate(() => {
  return [...document.querySelectorAll('button, [role=tab], [role=button], summary')].map((el, i) => {
    const r = el.getBoundingClientRect(); const cs = getComputedStyle(el);
    if (r.width < 4 || r.height < 4 || cs.visibility === 'hidden' || cs.display === 'none' || el.disabled) return null;
    el.setAttribute('data-probe', i);
    return { i, label: (el.getAttribute('aria-label') || el.getAttribute('title') || el.innerText || el.className || '').toString().replace(/\s+/g, ' ').slice(0, 40), x: r.x + r.width / 2, y: r.y + r.height / 2 };
  }).filter(Boolean);
});
const snap = (i) => page.evaluate((i) => { const el = document.querySelector(`[data-probe="${i}"]`); const cs = getComputedStyle(el); return [cs.backgroundColor, cs.color, cs.borderTopColor, cs.opacity, cs.boxShadow, cs.transform, cs.filter, cs.textDecorationLine, cs.outlineStyle].join('|'); }, i);
let none = [], ok = 0;
for (const b of btns) {
  await page.mouse.move(5, 540); await page.waitForTimeout(40);
  const before = await snap(b.i);
  await page.mouse.move(b.x, b.y); await page.waitForTimeout(220);
  const after = await snap(b.i);
  if (before === after) none.push(b); else { ok++; console.log("HOVER OK", b.label); }
}
console.log('total', btns.length, 'with hover', ok, 'without', none.length);
console.log(none.map((b) => `${Math.round(b.x)},${Math.round(b.y)}  ${b.label}`).join('\n'));
await browser.close();
