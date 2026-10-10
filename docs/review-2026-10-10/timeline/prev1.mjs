import { launch, open, shot } from './lib.mjs';
const b = await launch();
{
  const { p, ctx } = await open(b, { q: '?music=1', dsf: 1 });
  const tcSel = async () => (await p.locator('span[title^="Timeline timecode"]').innerText());
  await p.mouse.move(700, 645);
  await p.mouse.down();
  await p.mouse.move(800, 645, { steps: 5 });
  console.log('mid-drag tc', await tcSel());
  await p.mouse.move(1000, 645, { steps: 5 });
  console.log('mid-drag2 tc', await tcSel());
  await p.mouse.up();
  console.log('after up tc', await tcSel());
  const sb = await p.evaluate(() => { const d=[...document.querySelectorAll('div')].find(x=>x.style.cursor==='pointer' && x.style.height==='4px'); const r=d.getBoundingClientRect(); return {x:r.x,y:r.y,w:r.width,h:r.height}; });
  console.log('scrub bar rect', sb);
  await ctx.close();
}
{
  const { p, ctx } = await open(b, { w: 2560, h: 1440, q: '?music=1', dsf: 1 });
  await shot(p, 'p-2560');
  const fr = await p.evaluate(() => { const i=document.querySelector('img[alt="preview frame"]'); const f=(i?i.parentElement:[...document.querySelectorAll('div')].find(d=>d.style.aspectRatio)); const r=f.getBoundingClientRect(); return {w:r.width,h:r.height}; });
  console.log('frame @2560', fr);
  await ctx.close();
}
await b.close();
