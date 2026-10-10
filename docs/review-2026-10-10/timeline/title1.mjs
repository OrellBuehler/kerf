import { launch, open, shot } from './lib.mjs';
const b = await launch();
const { p } = await open(b, { q: '?music=1', dsf: 2 });
await p.locator('button[aria-label="Add a title"]').click();
await p.waitForTimeout(500);
// seek into title: click ruler at 1.5s
await p.mouse.click(224 + 36 * 1.5, 782);
await p.waitForTimeout(500);
await shot(p, 'pt-title-selected', { x: 780, y: 380, width: 320, height: 200 });
// deselect by clicking empty lane
await p.mouse.click(1300, 1030);
await p.waitForTimeout(300);
await shot(p, 'pt-title-unselected', { x: 780, y: 380, width: 320, height: 200 });
await shot(p, 'pt-tl-title', { x: 224, y: 795, width: 500, height: 36 });
await b.close();
