import { open, DIR } from './lib.mjs';
const desc = () => page.evaluate(() => { const a = document.activeElement; if (!a) return 'none'; const lab = a.getAttribute('aria-label') || a.closest('label')?.innerText?.split('\n')[0] || a.textContent?.trim().slice(0, 30) || a.placeholder || ''; return a.tagName.toLowerCase() + (a.type ? '[' + a.type + ']' : '') + ' "' + lab.trim().slice(0, 40) + '"' ; });
let page, browser, logs;
async function trace(name, openKeys, n = 30) {
  ({ browser, page, logs } = await open({ w: 1920, h: 1080 }));
  await page.keyboard.press(openKeys);
  await page.waitForTimeout(600);
  const out = [];
  out.push('start: ' + await desc());
  for (let i = 0; i < n; i++) { await page.keyboard.press('Tab'); out.push(i + ': ' + await desc()); }
  console.log('== ' + name + '\n' + out.join('\n'));
  await page.keyboard.press('Escape');
  await page.waitForTimeout(500);
  console.log('after Esc: dialog open? ', await page.locator('[role=dialog]').count(), ' focus:', await desc());
  await browser.close();
}
await trace('export', 'Control+e', 24);
await trace('settings', 'Control+,', 14);
