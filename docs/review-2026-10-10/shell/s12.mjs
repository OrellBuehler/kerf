import { open, S } from './lib.mjs';
const { browser, page } = await open({ w: 1920, h: 1080 });
const probes = [
  ['gear', 'button[aria-label=Settings]', 0],
  ['bell', '[data-notification-bell]', 0],
  ['version', 'button[title^="Kerf"]', 0],
  ['ws color', '[data-workspace="color"]', 0],
  ['dock tab inactive', '.dv-tab', 2],
  ['rail inactive', '[role=tablist] button', 2],
  ['menu top', '.kerf-menu-top', 1],
  ['status Logs', 'button[title^="Open the log"]', 0],
  ['toolbar IconBtn (timeline zoom out)', 'button[title="Zoom out"]', 0],
  ['timeline S toggle', 'button[title="Toggle solo"]', 0],
  ['snap', 'button[title^="Snapping"]', 0],
  ['+ Title', 'button:has-text("+ Title")', 0],
  ['track +V', 'button[title="Add a video track"]', 0],
];
for (const [name, sel, n] of probes) {
  const loc = page.locator(sel).nth(n);
  if (!(await loc.count())) { console.log(name, 'MISSING'); continue; }
  await page.mouse.move(960, 500); await page.waitForTimeout(250);
  const get = () => loc.evaluate((el) => { const cs = getComputedStyle(el); return cs.backgroundColor + ' | ' + cs.color + ' | ' + cs.borderColor + ' | ' + cs.opacity; });
  const before = await get();
  await loc.hover(); await page.waitForTimeout(350);
  const after = await get();
  console.log((before === after ? 'NO HOVER  ' : 'hover ok  ') + name.padEnd(34), before === after ? before : before + '  ->  ' + after);
}
await browser.close();
