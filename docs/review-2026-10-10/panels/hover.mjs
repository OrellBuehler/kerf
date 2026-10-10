import { open } from './lib.mjs';
const { browser, page } = await open({});
await page.mouse.click(400, 858);
await page.waitForTimeout(400);
const targets = {
  'bin row (interview)': 'div[role=button][draggable=true] >> nth=1',
  'rail tab (Titles)': '[data-library-tab="titles"]',
  'bin IconBtn plus': 'button[title="Import"]',
};
async function probe(label, loc) {
  const el = page.locator(loc).first();
  await el.scrollIntoViewIfNeeded().catch(()=>{});
  const sty = () => el.evaluate(e => { const c = getComputedStyle(e); return [c.backgroundColor, c.color, c.borderColor, c.boxShadow !== 'none' ? 'shadow' : '', c.opacity].join(' | '); });
  await page.mouse.move(1000, 300);
  const a = await sty();
  await el.hover();
  await page.waitForTimeout(250);
  const b = await sty();
  console.log(label.padEnd(26), a === b ? 'NO HOVER CHANGE' : 'changes', '\n   ', a, '\n   ', b);
}
for (const [k, v] of Object.entries(targets)) await probe(k, v);
await page.locator('[data-library-tab="effects"]').click();
await page.waitForTimeout(300);
await probe('effects chip (Warm)', 'button:has-text("Warm")');
await probe('effects tile (Blur)', 'button:has-text("Blur")');
await page.locator('[data-library-tab="titles"]').click();
await page.waitForTimeout(300);
await probe('Btn ghost (+ Text)', 'button:has-text("+ Text")');
await probe('Btn ghost (Captions)', 'button:has-text("Captions")');
await page.getByText('Agent', { exact: true }).first().click();
await page.waitForTimeout(300);
await probe('agent Apply', 'button:has-text("Apply") >> nth=0');
await probe('agent Preview', 'button:has-text("Preview")');
await probe('agent quick chip', 'button:has-text("Remove silences")');
await probe('agent Connect', 'button:has-text("Connect an agent")');
await page.getByText('Inspector', { exact: true }).first().click();
await page.waitForTimeout(300);
await probe('inspector summary', 'summary >> nth=1');
await probe('inspector Analyze Btn', 'button:has-text("Analyze")');
await browser.close();
