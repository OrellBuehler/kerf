import { launch, open, shot } from './lib.mjs';
const b = await launch();
const { p } = await open(b, { q: '?music=1', dsf: 1 });
const TL = { x: 0, y: 665, width: 1920, height: 390 };
await p.evaluate(() => { const s = document.querySelector('[data-lane]').parentElement.parentElement; s.scrollLeft = 900; });
await p.waitForTimeout(600);
await shot(p, 'sc2-scrolled', TL);
await b.close();
