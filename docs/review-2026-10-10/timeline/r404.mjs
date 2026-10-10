import { launch } from './lib.mjs';
const b = await launch();
const ctx = await b.newContext({ viewport: { width: 1920, height: 1080 } });
const p = await ctx.newPage();
p.on('response', r => { if (r.status() >= 400) console.log(r.status(), r.url()); });
await p.goto('http://localhost:1420/', { waitUntil: 'networkidle' });
await b.close();
