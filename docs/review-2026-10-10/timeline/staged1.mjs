import { launch, open, shot } from './lib.mjs';
const b = await launch();
const { p } = await open(b, { q: '?staged=1&music=1', dsf: 1 });
await p.getByRole('button', { name: 'Preview' , exact: true}).last().click().catch(e=>console.log('noprev', e.message));
await p.waitForTimeout(800);
await shot(p, 'm-staged-preview-full');
await shot(p, 'm-staged-preview-tl', { x: 0, y: 665, width: 1920, height: 390 });
console.log(p.logs);
await b.close();
