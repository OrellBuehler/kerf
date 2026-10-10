import { open } from './lib.mjs';
const { browser, page } = await open({ q: '' , dsf: 2});
const r = await page.evaluate(async () => {
  try {
    const m = await import('/src/lib/state.svelte.ts');
    m.editor.assets = [];
    return Object.keys(m).join(',');
  } catch (e) { return 'ERR ' + e.message; }
});
console.log(r);
await page.waitForTimeout(500);
await page.screenshot({ path: 'e-bin.png', clip: { x: 0, y: 36, width: 400, height: 330 } });
await page.locator('[data-library-tab="transcript"]').click();
await page.waitForTimeout(300);
await page.screenshot({ path: 'e-transcript.png', clip: { x: 0, y: 36, width: 400, height: 330 } });
await browser.close();
