import { launch, open, shot } from './lib.mjs';
const b = await launch();
// staged
{
  const { p, ctx } = await open(b, { q: '?staged=1', dsf: 1 });
  await shot(p, 'm-staged-full');
  await ctx.close();
}
// gpusurface + vertical delivery + safe areas
{
  const { p, ctx } = await open(b, { q: '?gpusurface=1', dsf: 1, pre: async (p) => {
    await p.evaluate(() => { localStorage.setItem('kerf.settings.safeAreas', '1'); });
    await p.reload({ waitUntil: 'networkidle' });
  }});
  await p.getByRole('button', { name: /Source/ }).click();
  await p.waitForTimeout(300);
  await shot(p, 'm-delivery-menu');
  await p.getByRole('menuitem', { name: /9:16/ }).click();
  await p.waitForTimeout(800);
  await shot(p, 'm-916-full');
  await shot(p, 'm-916-preview', { x: 400, y: 70, width: 1066, height: 600 });
  await ctx.close();
}
await b.close();
