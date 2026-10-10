import { launch, open, shot } from './lib.mjs';
const b = await launch();
const { p } = await open(b, { q: '?music=1', dsf: 2 });
p.on('response', r => { if (r.status() === 404) console.log('404', r.url()); });
await p.reload({ waitUntil: 'networkidle' });
await p.waitForTimeout(1000);
// toolbar
await shot(p, 'c2-toolbar-left', { x: 0, y: 698, width: 560, height: 36 });
await shot(p, 'c2-toolbar-right', { x: 1280, y: 698, width: 640, height: 36 });
await shot(p, 'c2-overview-ruler', { x: 0, y: 732, width: 1100, height: 100 });
await shot(p, 'c2-headers', { x: 0, y: 790, width: 420, height: 270 });
await shot(p, 'c2-clips-left', { x: 220, y: 820, width: 760, height: 200 });
await shot(p, 'c2-statusbar', { x: 0, y: 1050, width: 1920, height: 30 });
await shot(p, 'c2-transport', { x: 400, y: 620, width: 1066, height: 48 });
await shot(p, 'c2-preview-frame', { x: 560, y: 130, width: 750, height: 440 });
await b.close();
