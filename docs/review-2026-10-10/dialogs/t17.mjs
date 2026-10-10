import { open, DIR } from './lib.mjs';
for (const [w,h,theme] of [[1920,1080,'dark'],[1366,768,'dark'],[1920,1080,'light']]) {
const { browser, page, logs } = await open({ w, h });
if (theme === 'light') { await page.evaluate(async () => { const s = await import('/src/lib/settings.svelte.ts'); await s.settings.applyPreset('kerf-light'); }); }
await page.evaluate(async () => { const m = await import('/src/lib/state.svelte.ts'); m.editor.assets = []; m.editor.timeline = { tracks: [] }; });
await page.waitForTimeout(700);
await page.screenshot({ path: `${DIR}/120-empty-${w}-${theme}.png` });
if (w === 1920 && theme === 'dark') {
  // other workspaces empty
  for (const ws of ['Deliver','Audio']) { await page.getByRole('button', { name: ws, exact: true }).click(); await page.waitForTimeout(700); await page.screenshot({ path: `${DIR}/121-empty-${ws}.png` }); }
  await page.getByRole('button', { name: 'Edit', exact: true }).click();
  await page.keyboard.press('Control+e'); await page.waitForTimeout(500);
  await (await page.$('[role=dialog] > div')).screenshot({ path: `${DIR}/122-empty-export.png` });
}
await browser.close();
}
