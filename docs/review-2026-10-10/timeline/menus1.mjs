import { launch, open, shot } from './lib.mjs';
const b = await launch();
const { p } = await open(b, { q: '?music=1', dsf: 2 });
// canvas (below tracks) context menu
await p.mouse.click(1400, 1030, { button: 'right' });
await p.waitForTimeout(300);
let m = await p.locator('[role=menu]').boundingBox();
console.log('canvas menu', m);
await shot(p, 'cm-canvas', { x: m.x - 10, y: m.y - 10, width: m.width + 20, height: m.height + 20 });
await p.keyboard.press('Escape');
// track header menu
await p.mouse.click(60, 910, { button: 'right' });
await p.waitForTimeout(300);
m = await p.locator('[role=menu]').boundingBox();
console.log('track menu', m);
await shot(p, 'cm-track', { x: m.x - 10, y: m.y - 10, width: m.width + 20, height: m.height + 20 });
await p.keyboard.press('Escape');
// name chevron menu
await p.locator('button[aria-label="Track A1 height"]').click();
await p.waitForTimeout(300);
m = await p.locator('[role=menu]').boundingBox();
await shot(p, 'cm-height', { x: m.x - 10, y: m.y - 10, width: m.width + 20, height: m.height + 20 });
await p.keyboard.press('Escape');
// keyboard nav test: open clip menu, press ArrowDown, see focus
const c1 = await p.locator('.kclip[data-clip-id="c4"]').boundingBox();
await p.mouse.click(c1.x + 300, c1.y + 20, { button: 'right' });
await p.waitForTimeout(300);
await p.keyboard.press('ArrowDown');
const af = await p.evaluate(() => document.activeElement?.tagName + ' ' + (document.activeElement?.getAttribute('role') ?? ''));
console.log('active after ArrowDown:', af);
m = await p.locator('[role=menu]').boundingBox();
console.log('audio clip menu', m);
await shot(p, 'cm-audio-clip', { x: m.x - 10, y: Math.max(0,m.y - 10), width: m.width + 20, height: Math.min(m.height + 20, 1080) });
await b.close();
