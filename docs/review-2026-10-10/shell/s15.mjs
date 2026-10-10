import { open, S } from './lib.mjs';
{
  const { browser, page } = await open({ w: 1920, h: 1080, dsf: 2, theme: 'kerf-light' });
  await page.getByRole('menuitem', { name: 'Edit', exact: true }).first().click();
  await page.waitForTimeout(250);
  await page.screenshot({ path: `${S}/light-menu-edit.png`, clip: { x: 0, y: 0, width: 420, height: 480 } });
  await page.keyboard.press('Escape');
  await page.mouse.click(1000, 400, { button: 'right' });
  // right-click on a timeline clip for ctx menu
  await page.mouse.click(450, 860, { button: 'right' });
  await page.waitForTimeout(300);
  await page.screenshot({ path: `${S}/light-ctx.png`, clip: { x: 300, y: 620, width: 520, height: 460 } });
  await page.keyboard.press('Escape');
  await page.locator('button[title="Remove track"]').nth(1).click();
  await page.waitForTimeout(500);
  await page.locator('[data-notification-bell]').click();
  await page.waitForTimeout(300);
  await page.screenshot({ path: `${S}/light-notif.png`, clip: { x: 1300, y: 0, width: 620, height: 220 } });
  await browser.close();
}
{
  const { browser, page } = await open({ w: 1920, h: 1080, dsf: 2, theme: 'kerf-dark' });
  await page.mouse.click(450, 860, { button: 'right' });
  await page.waitForTimeout(300);
  await page.screenshot({ path: `${S}/dark-ctx.png`, clip: { x: 300, y: 560, width: 560, height: 520 } });
  // measure status bar divider vs text vertical center
  const m = await page.evaluate(() => {
    const bar = [...document.querySelectorAll('div')].find(d => getComputedStyle(d).height === '26px' && d.textContent.includes('assets'));
    const kids = [...bar.children].map(k => { const r = k.getBoundingClientRect(); return [k.tagName, k.textContent.trim().slice(0, 14), +(r.y + r.height / 2).toFixed(1), +r.height.toFixed(1)]; });
    return kids;
  });
  console.log(JSON.stringify(m));
  await browser.close();
}
