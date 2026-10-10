import { open } from './lib.mjs';
import { execFileSync } from 'child_process';
// usage: node cap.mjs <name> <theme> <expand> <selection>
const [name, theme = 'dark', expand = '1', sel = 'v1'] = process.argv.slice(2);
const { browser, page } = await open({ theme });
const pts = { v1: [400, 858], a1: [400, 910], a2: [1000, 975], v1b: [800, 858] };
if (sel !== 'none') { await page.mouse.click(...pts[sel]); await page.waitForTimeout(500); }
await page.getByText('Inspector', { exact: true }).first().click();
await page.waitForTimeout(300);
if (expand === '1') await page.evaluate(() => document.querySelectorAll('details').forEach(d => d.open = true));
await page.setViewportSize({ width: 1920, height: 3300 });
await page.waitForTimeout(800);
const b = await page.evaluate(() => { const g = [...document.querySelectorAll('.dv-groupview')].find(g => g.innerText.startsWith('Inspector')); const r = g.getBoundingClientRect(); return { x: r.x, y: r.y, w: r.width, h: r.height }; });
await page.screenshot({ path: `${name}.png`, clip: { x: b.x, y: b.y, width: b.w, height: Math.min(b.h, 3200) } });
await browser.close();
