import { launch, open, shot } from './lib.mjs';
const b = await launch();
const { p } = await open(b, { q: '?music=1', dsf: 1 });
const TL = { x: 0, y: 665, width: 1920, height: 390 };
const clip = async (id) => (await p.locator(`.kclip[data-clip-id="${id}"]`).boundingBox());

// 1. select c2 (broll) click
let c2 = await clip('c2');
await p.mouse.click(c2.x + 150, c2.y + 25);
await p.waitForTimeout(400);
await shot(p, 'i1-select-c2', TL);
await shot(p, 'i1-select-c2-full');
// 2. hover over selected clip -> overlays
await p.mouse.move(c2.x + 120, c2.y + 30);
await p.waitForTimeout(300);
await shot(p, 'i1-hover-c2', { x: 600, y: 800, width: 500, height: 100 });
// 3. hover the right edge
await p.mouse.move(c2.x + c2.width - 3, c2.y + 30);
await p.waitForTimeout(300);
await shot(p, 'i1-hover-edge', { x: 800, y: 800, width: 300, height: 100 });
// 4. drag right edge of c2 left by 60px, mid drag
await p.mouse.down();
await p.mouse.move(c2.x + c2.width - 30, c2.y + 30, { steps: 5 });
await p.mouse.move(c2.x + c2.width - 63, c2.y + 30, { steps: 5 });
await p.waitForTimeout(200);
await shot(p, 'i1-edge-drag', TL);
await shot(p, 'i1-edge-drag-full');
await p.keyboard.press('Escape');
await p.mouse.up();
await p.waitForTimeout(300);
// 5. drag c2 clip body to the right by 120px
c2 = await clip('c2');
await p.mouse.move(c2.x + 100, c2.y + 25);
await p.mouse.down();
await p.mouse.move(c2.x + 160, c2.y + 25, { steps: 5 });
await p.mouse.move(c2.x + 260, c2.y + 25, { steps: 5 });
await p.waitForTimeout(200);
await shot(p, 'i1-clip-drag', TL);
await p.keyboard.press('Escape');
await p.mouse.up();
await p.waitForTimeout(300);
// 6. right click clip c1
const c1 = await clip('c1');
await p.mouse.click(c1.x + 200, c1.y + 25, { button: 'right' });
await p.waitForTimeout(400);
await shot(p, 'i1-ctx-clip-full');
const menu = await p.locator('[role=menu]').boundingBox();
console.log('menu', menu);
await p.keyboard.press('Escape');
console.log(p.logs);
await b.close();
