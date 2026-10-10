import { launch, open, shot } from './lib.mjs';
const b = await launch();
const { p } = await open(b, { q: '', dsf: 1 });
await p.mouse.click(1500, 1030);
await p.keyboard.press('Control+a');
await p.keyboard.press('Delete');
await p.waitForTimeout(600);
await shot(p, 'e-empty-full');
await b.close();
