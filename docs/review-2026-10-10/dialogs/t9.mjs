import { open, DIR } from './lib.mjs';
const { browser, page, logs } = await open({ w: 1920, h: 1080, query: 'update=1' });
await page.waitForTimeout(800);
const dlg = async (n) => { await page.waitForTimeout(300); const el = await page.$('[role=dialog] > div'); await el.screenshot({ path: `${DIR}/${n}.png` }); };
await page.screenshot({ path: `${DIR}/60-update-auto-full.png` });
await dlg('60-update-available');
console.log('active:', await page.evaluate(() => document.activeElement?.outerHTML.slice(0, 100)));
// Tab order
const order = [];
for (let i = 0; i < 5; i++) { await page.keyboard.press('Tab'); order.push(await page.evaluate(() => document.activeElement?.textContent?.trim())); }
console.log(order.join(' | '));
// install
await page.getByRole('button', { name: 'Install update' }).click();
await page.waitForTimeout(700);
await dlg('61-update-downloading');
await page.waitForTimeout(4500);
await dlg('62-update-ready');
// unsaved warning present?  hasUnsavedWork true in harness
await page.evaluate(async () => { const m = await import('/src/lib/updater.svelte.ts'); m.updater.phase = 'error'; m.updater.update = null; m.updater.error = 'error sending request for url (https://github.com/OrellBuehler/kerf/releases/latest/download/latest.json): error trying to connect: dns error'; m.updater.errorKind = 'check'; });
await dlg('63-update-check-error');
await page.evaluate(async () => { const m = await import('/src/lib/updater.svelte.ts'); m.updater.phase = 'current'; m.updater.update = null; m.updater.error = null; m.updater.errorKind = null; });
await dlg('64-update-current');
await page.evaluate(async () => { const m = await import('/src/lib/updater.svelte.ts'); m.updater.phase = 'checking'; });
await dlg('65-update-checking');
await page.evaluate(async () => { const m = await import('/src/lib/updater.svelte.ts'); m.updater.phase = 'error'; m.updater.update = {version:'0.99.0', date:'2026-10-01T00:00:00Z', notes:'Synthetic'}; m.updater.error = 'failed to install: permission denied'; m.updater.errorKind = 'install'; });
await dlg('66-update-install-error');
console.log(logs.filter(l=>!l.includes('404')).join('\n'));
await browser.close();
