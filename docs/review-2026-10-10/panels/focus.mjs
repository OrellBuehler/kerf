import { open } from './lib.mjs';
const { browser, page } = await open({ dsf: 2 });
// tab to first bin row via keyboard
await page.locator('[data-library-tab="media"]').focus();
for (let i=0;i<8;i++){ await page.keyboard.press('Tab'); const d = await page.evaluate(() => { const e=document.activeElement; const c=getComputedStyle(e); return (e.getAttribute('role')||e.tagName)+' '+(e.getAttribute('aria-label')||e.title||e.innerText||'').slice(0,30)+' | outline:'+c.outlineStyle+' '+c.outlineWidth+' '+c.outlineColor+' | shadow:'+(c.boxShadow!=='none'); }); console.log(i,d); }
await page.screenshot({ path: 'focus-bin.png', clip: { x: 0, y: 36, width: 400, height: 300 } });
await browser.close();
