import { open, DIR } from './lib.mjs';
const { browser, page, logs } = await open({ w: 1920, h: 1080 });
await page.getByRole('tab', { name: 'Agent' }).click().catch(async () => { await page.getByText('Agent', { exact: true }).first().click(); });
await page.waitForTimeout(600);
await page.screenshot({ path: `${DIR}/180-agent.png`, clip: { x: 1467, y: 36, width: 453, height: 630 } });
await browser.close();
