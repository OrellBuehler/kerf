import { open, DIR } from './lib.mjs';
const { browser, page, logs } = await open();
const r = await page.evaluate(async () => {
  const m = await import('/src/lib/state.svelte.ts');
  const u = await import('/src/lib/editor-ui.svelte.ts');
  return Object.keys(m).join(',') + ' | ' + Object.keys(u).join(',');
});
console.log(r);
console.log(logs.join('\n'));
await browser.close();
