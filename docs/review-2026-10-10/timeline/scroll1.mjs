import { launch, open, shot } from './lib.mjs';
const b = await launch();
const { p } = await open(b, { q: '?music=1', dsf: 1 });
const TL = { x: 0, y: 665, width: 1920, height: 390 };
// zoom in to ~100px/s and scroll right
await p.mouse.move(900, 900);
for (let i = 0; i < 4; i++) await p.keyboard.press('=');
await p.evaluate(() => { const s = [...document.querySelectorAll('div')].find(d => getComputedStyle(d).overflow === 'auto' && d.querySelector('[data-lane]')); s.scrollLeft = 2200; });
await p.waitForTimeout(600);
await shot(p, 'sc-scrolled', TL);
// disable clip c2
const c2 = await p.locator('.kclip[data-clip-id="c2"]').boundingBox();
await p.keyboard.press('Shift+Z');
await p.waitForTimeout(400);
const c2b = await p.locator('.kclip[data-clip-id="c2"]').boundingBox();
await p.mouse.click(c2b.x + 60, c2b.y + 20, { button: 'right' });
await p.getByRole('menuitem', { name: /Disable clip/ }).click();
await p.waitForTimeout(500);
await p.mouse.move(1200, 600);
await shot(p, 'sc-disabled', TL);
console.log(p.logs);
await b.close();
