import { launch, open, shot } from './lib.mjs';
const b = await launch();
const { p } = await open(b, { q: '?music=1', dsf: 1 });
const TL = { x: 0, y: 665, width: 1920, height: 390 };
await p.mouse.move(900, 600);
for (const [name, key, n] of [['z-in8', '=', 8], ['z-in14', '=', 6], ['z-in20', '=', 6]]) {
  for (let i = 0; i < n; i++) await p.keyboard.press(key);
  await p.waitForTimeout(500);
  await shot(p, name, TL);
}
// go back out
for (let i = 0; i < 20; i++) await p.keyboard.press('-');
for (let i = 0; i < 12; i++) await p.keyboard.press('-');
await p.waitForTimeout(500);
await shot(p, 'z-out12', TL);
for (let i = 0; i < 6; i++) await p.keyboard.press('-');
await p.waitForTimeout(500);
await shot(p, 'z-out18', TL);
await p.keyboard.press('Shift+Z');
await p.waitForTimeout(600);
await shot(p, 'z-fit', TL);
console.log(p.logs);
await b.close();
