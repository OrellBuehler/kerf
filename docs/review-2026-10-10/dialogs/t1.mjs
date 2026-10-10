import { open, DIR } from './lib.mjs';
for (const [w,h] of [[1920,1080],[1366,768]]) {
  const { browser, page, logs } = await open({ w, h });
  await page.keyboard.press('Control+e');
  await page.waitForTimeout(600);
  await page.screenshot({ path: `${DIR}/10-export-${w}.png` });
  // scroll the dialog body to the bottom
  const info = await page.evaluate(() => {
    const dlg = document.querySelector('[role=dialog]');
    const body = [...dlg.querySelectorAll('div')].find(d => getComputedStyle(d).overflowY === 'auto');
    const r = dlg.firstElementChild.getBoundingClientRect();
    return { box: [r.x, r.y, r.width, r.height], scrollH: body?.scrollHeight, clientH: body?.clientHeight, active: document.activeElement?.tagName + ':' + (document.activeElement?.getAttribute('aria-label')||document.activeElement?.textContent?.slice(0,30)) };
  });
  console.log(w, JSON.stringify(info));
  // open advanced
  await page.getByText('Advanced encoding').click();
  await page.waitForTimeout(300);
  await page.screenshot({ path: `${DIR}/11-export-adv-${w}.png` });
  await page.evaluate(() => { const dlg = document.querySelector('[role=dialog]'); const body = [...dlg.querySelectorAll('div')].find(d => getComputedStyle(d).overflowY === 'auto'); body.scrollTop = body.scrollHeight; });
  await page.waitForTimeout(200);
  await page.screenshot({ path: `${DIR}/12-export-adv-bottom-${w}.png` });
  console.log(logs.join('\n'));
  await browser.close();
}
