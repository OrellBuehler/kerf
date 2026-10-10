import { open, DIR } from './lib.mjs';
const { browser, page, logs } = await open({ w: 1920, h: 1080 });
const shot = async (n, sel = '[role=dialog] > div') => { await page.waitForTimeout(250); const el = await page.$(sel); await el.screenshot({ path: `${DIR}/${n}.png` }); };
await page.evaluate(async () => {
  const u = await import('/src/lib/editor-ui.svelte.ts');
  u.ui.markIn = 2.5; u.ui.markOut = 12.25;
});
await page.keyboard.press('Control+e');
await page.waitForTimeout(500);
await shot('30-export-range');
// validation: choose target_size and clear bitrate
const sel = page.locator('[role=dialog] select').first();
await sel.selectOption('target_size_2pass');
await page.getByText('Advanced encoding').click();
await page.waitForTimeout(200);
const br = page.locator('[role=dialog] input[placeholder="e.g. 8M"]');
await br.fill('');
await br.press('Tab');
await shot('31-export-invalid');
await br.fill('abc');
await br.press('Tab');
await shot('31b-export-invalid2');
await br.fill('8M'); await br.press('Tab');
// progress states
await page.evaluate(async () => {
  const m = await import('/src/lib/state.svelte.ts');
  m.editor.exportRun = { progress: { fraction: 0.42, elapsed_secs: 31, eta_secs: 75, variant: 1, total: 3 }, cancelling: false };
});
await shot('32-export-rendering');
await page.evaluate(async () => { const m = await import('/src/lib/state.svelte.ts'); m.editor.exportRun = { progress: { fraction: 0, elapsed_secs: 0, waiting: 'Export waiting for the preview proxy' }, cancelling: false }; });
await shot('33-export-waiting');
await page.evaluate(async () => { const m = await import('/src/lib/state.svelte.ts'); m.editor.exportRun = { progress: { fraction: 0.9, elapsed_secs: 0, eta_secs: 3 }, cancelling: true }; });
await shot('34-export-stopping');
// status bar during run
await page.screenshot({ path: `${DIR}/35-statusbar-rendering.png`, clip: { x: 0, y: 1040, width: 1920, height: 40 } });
await page.evaluate(async () => { const m = await import('/src/lib/state.svelte.ts'); m.editor.exportRun = null; });
// click Export in harness
await page.getByRole('button', { name: 'Export' , exact: true}).last().click();
await page.waitForTimeout(500);
await page.screenshot({ path: `${DIR}/36-export-toast.png` });
console.log(logs.join('\n'));
await browser.close();
