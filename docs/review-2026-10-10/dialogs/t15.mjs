import { open, DIR } from './lib.mjs';
const cases = {
  success: (t) => t.success('Exported → /home/orell/Videos/very/long/path/to/my-project-final-cut-v3-FINAL.mp4', { action: { label: 'Show in folder', onClick: () => {} } }),
  error: (t) => t.error("Couldn't download the base.en speech model — error sending request for url (https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin): dns error"),
  errdesc: (t) => t.error('Could not save the theme', { description: 'permission denied (os error 13)' }),
  warning: (t) => t.warning('Skipped 2 cues that could not be read'),
  info: (t) => t.info('Export renders with FFmpeg in the desktop app.'),
  note: (t) => t('Clip removed', { action: { label: 'Undo', onClick: () => {} } }),
};
for (const theme of ['dark', 'light']) {
  const { browser, page } = await open({ w: 1920, h: 1080 });
  if (theme === 'light') { await page.evaluate(async () => { const s = await import('/src/lib/settings.svelte.ts'); await s.settings.applyPreset('kerf-light'); }); await page.waitForTimeout(400); }
  for (const [k, fn] of Object.entries(cases)) {
    await page.evaluate(async (src) => { const n = await import('/src/lib/notifications.svelte.ts'); n.toast.dismiss(); eval('(' + src + ')')(n.toast); }, fn.toString());
    await page.waitForTimeout(700);
    await page.screenshot({ path: `${DIR}/102-toast-${k}-${theme}.png`, clip: { x: 1380, y: 900, width: 540, height: 180 } });
  }
  await browser.close();
}
