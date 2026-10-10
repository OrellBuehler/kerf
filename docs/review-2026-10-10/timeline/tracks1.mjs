import { launch, open, shot } from './lib.mjs';
const b = await launch();
const { p } = await open(b, { q: '?music=1', dsf: 1 });
const TL = { x: 0, y: 665, width: 1920, height: 390 };
// add tracks
await p.getByRole('button', { name: '+ V' }).click();
await p.getByRole('button', { name: '+ A' }).click();
await p.waitForTimeout(500);
await shot(p, 'tr-5tracks', TL);
// scroll the timeline vertically
await p.evaluate(() => { const s = [...document.querySelectorAll('div')].find(d => getComputedStyle(d).overflow === 'auto' && d.querySelector('[data-lane]')); s.scrollTop = 80; });
await p.waitForTimeout(300);
await shot(p, 'tr-5tracks-scrolled', TL);
// reset scroll; set compact heights
await p.evaluate(() => { const s = [...document.querySelectorAll('div')].find(d => getComputedStyle(d).overflow === 'auto' && d.querySelector('[data-lane]')); s.scrollTop = 0; });
await p.locator('button[aria-label="All tracks compact"]').click();
await p.waitForTimeout(400);
await shot(p, 'tr-compact', TL);
await shot(p, 'tr-compact-crop', { x: 0, y: 770, width: 1000, height: 285 });
await p.locator('button[aria-label="All tracks large"]').click();
await p.waitForTimeout(400);
await shot(p, 'tr-large', TL);
console.log(p.logs);
await b.close();
