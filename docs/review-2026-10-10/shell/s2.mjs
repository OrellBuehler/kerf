import { open, S } from './lib.mjs';
const { browser, page, logs } = await open({});
const r = await page.evaluate(async () => {
  await document.fonts.ready;
  const out = {};
  out.fontsSize = document.fonts.size;
  out.checkGeist = document.fonts.check('13px Geist');
  out.checkMono = document.fonts.check('13px "Geist Mono"');
  out.checkGrotesk = document.fonts.check('13px "Space Grotesk"');
  out.imports = [];
  for (const ss of document.styleSheets) {
    try { for (const r of ss.cssRules) if (r.type === 3) out.imports.push(r.href); } catch(e) { out.imports.push('ERR ' + ss.href); }
  }
  out.fontFaceCount = 0;
  for (const ss of document.styleSheets) { try { for (const r of ss.cssRules) if (r.type === 5) out.fontFaceCount++; } catch(e){} }
  out.links = [...document.querySelectorAll('link')].map(l => l.href);
  return out;
});
console.log(JSON.stringify(r, null, 1));
console.log(logs);
await browser.close();
