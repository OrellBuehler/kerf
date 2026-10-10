import { launch, open, shot } from './lib.mjs';
const b = await launch();
for (const [w,h] of [[1366,768],[1100,700]]) {
const { p, ctx } = await open(b, { w, h, q: '?music=1', dsf: 1 });
const clip = async (id) => (await p.locator(`.kclip[data-clip-id="${id}"]`).boundingBox());
const c1 = await clip('c1');
await p.keyboard.down('Alt');
await p.mouse.click(c1.x + 100, c1.y + 20, { modifiers: ['Alt'] });
await p.waitForTimeout(300);
await p.mouse.click(c1.x + 100, c1.y + 20);
await p.keyboard.down('Alt');
await p.waitForTimeout(400);
const tb = await p.evaluate(() => {
  const el = [...document.querySelectorAll('[role=toolbar]')][0]?.parentElement; const r = el.getBoundingClientRect(); return { y: r.y, h: r.height, w: r.width };
});
console.log(w, 'toolbar', tb);
await shot(p, `n-${w}`, { x: 0, y: Math.max(0, tb.y - 40), width: w, height: 330 });
await shot(p, `n-${w}-full`);
await ctx.close();
}
await b.close();
