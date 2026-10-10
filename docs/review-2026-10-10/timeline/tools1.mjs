import { launch, open, shot } from './lib.mjs';
const b = await launch();
const { p } = await open(b, { q: '?music=1', dsf: 1 });
const TL = { x: 0, y: 665, width: 1920, height: 390 };
const clip = async (id) => (await p.locator(`.kclip[data-clip-id="${id}"]`).boundingBox());
// razor hover
await p.keyboard.press('c');
const c1 = await clip('c1');
await p.mouse.move(c1.x + 250, c1.y + 25, { steps: 4 });
await p.waitForTimeout(300);
await shot(p, 'tl-razor-hover', TL);
// roll tool hover on the cut c1|c2 at x=674
await p.keyboard.press('n');
await p.mouse.move(c1.x + c1.width - 2, c1.y + 25, { steps: 4 });
await p.waitForTimeout(300);
await shot(p, 'tl-roll-hover', TL);
// roll drag
await p.mouse.down();
await p.mouse.move(c1.x + c1.width - 40, c1.y + 25, { steps: 6 });
await p.waitForTimeout(300);
await shot(p, 'tl-roll-drag-full');
await p.keyboard.press('Escape'); await p.mouse.up();
// slip drag on c2
await p.keyboard.press('y');
const c2 = await clip('c2');
await p.mouse.move(c2.x + 120, c2.y + 25);
await p.mouse.down();
await p.mouse.move(c2.x + 80, c2.y + 25, { steps: 6 });
await p.waitForTimeout(300);
await shot(p, 'tl-slip-drag-full');
await p.keyboard.press('Escape'); await p.mouse.up();
console.log(p.logs);
await b.close();
