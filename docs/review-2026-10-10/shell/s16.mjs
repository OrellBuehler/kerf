import { open, S } from './lib.mjs';
const { browser, page } = await open({ w: 1920, h: 1080, q: '?staged=1' });
await page.waitForTimeout(1000);
await page.screenshot({ path: `${S}/staged.png` });
await browser.close();
