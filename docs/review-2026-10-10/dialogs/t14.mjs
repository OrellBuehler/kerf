import { open, DIR } from './lib.mjs';
for (const theme of ['dark', 'light']) {
const { browser, page, logs } = await open({ w: 1920, h: 1080 });
if (theme === 'light') {
  await page.evaluate(async () => { const s = await import('/src/lib/settings.svelte.ts'); await s.settings.applyPreset('kerf-light'); });
  await page.waitForTimeout(500);
}
await page.evaluate(async () => {
  const n = await import('/src/lib/notifications.svelte.ts');
  const t = n.toast;
  t.success('Exported → /home/orell/Videos/very/long/path/to/my-project-final-cut-v3-FINAL.mp4', { action: { label: 'Show in folder', onClick: () => {} } });
  t.error("Couldn't download the base.en speech model — error sending request for url (https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin): dns error");
  t.warning('Skipped 2 cues that could not be read', {});
  t.info('Export renders with FFmpeg in the desktop app.');
  t('Clip removed', { action: { label: 'Undo', onClick: () => {} } });
  t.error('Could not save the theme', { description: 'permission denied (os error 13)' });
});
await page.waitForTimeout(900);
await page.screenshot({ path: `${DIR}/100-toasts-${theme}.png`, clip: { x: 1300, y: 300, width: 620, height: 780 } });
// bell
await page.evaluate(async () => { const n = await import('/src/lib/notifications.svelte.ts'); n.notifications.open = true; });
await page.waitForTimeout(500);
await page.screenshot({ path: `${DIR}/101-notifications-${theme}.png`, clip: { x: 1300, y: 0, width: 620, height: 700 } });
await browser.close();
}
