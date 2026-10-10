import { open } from './lib.mjs';
const { browser, page } = await open({ dsf: 2 });
await page.mouse.click(800, 858); // broll clip
await page.waitForTimeout(400);
await page.getByText('Inspector', { exact: true }).first().click();
await page.waitForTimeout(300);
await page.evaluate(() => document.querySelectorAll('details').forEach(d => d.open = true));
await page.mouse.click(700, 782); // seek on ruler
await page.waitForTimeout(300);
await page.locator('button[aria-label*="brightness keyframe"]').click();
await page.waitForTimeout(400);
await page.mouse.click(900, 782);
await page.waitForTimeout(300);
await page.locator('button[aria-label*="brightness keyframe"]').click();
await page.waitForTimeout(400);
await page.getByRole('button', { name: '+ Keyframe @ playhead' }).click().catch(()=>{});
await page.waitForTimeout(300);
await page.mouse.click(760, 782);
await page.getByRole('button', { name: '+ Keyframe @ playhead' }).click().catch(()=>{});
await page.waitForTimeout(400);
await page.setViewportSize({ width: 1920, height: 3000 });
await page.waitForTimeout(800);
const pos = await page.evaluate(() => { const out = {}; for (const s of document.querySelectorAll('details > summary')) { const r = s.getBoundingClientRect(); out[s.innerText.split('\n')[0].trim()] = { y: r.y, summary: s.innerText.replace(/\n/g,' | ') }; } return out; });
console.log(JSON.stringify(pos));
const c = pos['Color'], a = pos['Animation'], t = pos['Transform'];
await page.screenshot({ path: 'k-color.png', clip: { x: 1467, y: c.y - 6, width: 453, height: 240 } });
await page.screenshot({ path: 'k-anim.png', clip: { x: 1467, y: a.y - 6, width: 453, height: 260 } });
await page.screenshot({ path: 'k-transform.png', clip: { x: 1467, y: t.y - 6, width: 453, height: 190 } });
await browser.close();
