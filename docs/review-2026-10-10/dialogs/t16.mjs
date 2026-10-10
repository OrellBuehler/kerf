import { open, DIR } from './lib.mjs';
const { browser, page, logs } = await open({ w: 1920, h: 1080 });
await page.getByRole('button', { name: /Import captions/ }).first().click();
await page.waitForTimeout(500);
await page.screenshot({ path: `${DIR}/110-import-captions-opts.png`, clip: { x: 1467, y: 200, width: 453, height: 460 } });
const [fc] = await Promise.all([
  page.waitForEvent('filechooser'),
  page.getByRole('button', { name: /Choose file/ }).click()
]);
await fc.setFiles(DIR + '/sample.srt');
await page.waitForTimeout(900);
await page.screenshot({ path: `${DIR}/111-import-captions-done.png` });
// again for replace confirm
await page.getByRole('button', { name: /Import captions/ }).first().click();
await page.waitForTimeout(300);
const [fc2] = await Promise.all([ page.waitForEvent('filechooser'), page.getByRole('button', { name: /Choose file/ }).click() ]);
await fc2.setFiles(DIR + '/sample.srt');
await page.waitForTimeout(900);
console.log(logs.filter(l=>!l.includes('404')).join('\n'));
await browser.close();
