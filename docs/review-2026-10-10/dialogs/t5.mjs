import { open, DIR } from './lib.mjs';
for (const [w,h] of [[1920,1080],[1366,768]]) {
  const { browser, page, logs } = await open({ w, h });
  await page.keyboard.press('Control+,');
  await page.waitForTimeout(600);
  const secs = ['Performance','Analysis','Speech','Preview','Appearance','Keyboard'];
  for (const s of secs) {
    await page.locator('[role=dialog] button', { hasText: new RegExp('^\\s*' + s + '\\s*$') }).first().click();
    await page.waitForTimeout(400);
    const info = await page.evaluate(() => {
      const dlg = document.querySelector('[role=dialog]');
      const panel = [...dlg.querySelectorAll('div')].find(d => ['auto','hidden'].includes(getComputedStyle(d).overflowY) && d.scrollHeight > 0 && d.parentElement?.style.display === 'flex' && d.style.flex?.startsWith('1'));
      return { scrollH: panel?.scrollHeight, clientH: panel?.clientHeight };
    });
    console.log(w, s, JSON.stringify(info));
    await (await page.$('[role=dialog] > div')).screenshot({ path: `${DIR}/40-settings-${s.toLowerCase()}-${w}.png` });
  }
  console.log(logs.filter(l=>!l.includes('404')).join('\n'));
  await browser.close();
}
