import { launch, open, shot } from './lib.mjs';
const b = await launch();
const { p } = await open(b, { q: '?music=1', dsf: 1 });
await p.evaluate(() => document.querySelector('.kclip').focus({ focusVisible: true }));
await p.keyboard.press('Tab'); await p.keyboard.press('Shift+Tab');
const o = await p.evaluate(() => { const e = document.activeElement; const cs = getComputedStyle(e); return { cls: e.className, tag: e.tagName, outline: cs.outline, fv: e.matches(':focus-visible') }; });
console.log(o);
await shot(p, 'f-clip', { x: 200, y: 810, width: 500, height: 100 });
// ripple on
await p.keyboard.press('r');
await p.waitForTimeout(300);
await shot(p, 'f-ripple', { x: 0, y: 665, width: 1920, height: 390 });
await b.close();
