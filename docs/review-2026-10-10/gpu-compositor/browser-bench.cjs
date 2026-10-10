const { chromium } = require('/home/orell/github/bissbilanz/node_modules/playwright-core');
const fs = require('fs');
const dir = __dirname + '/tmp/';
(async () => {
  const browser = await chromium.launch({ headless: true, args: ['--no-sandbox', '--enable-unsafe-swiftshader'] });
  const page = await browser.newPage();
  await page.setContent('<canvas id=c></canvas><img id=i>');
  const data = {
    j960q6: fs.readFileSync(dir + 'f_960_q6.jpg').toString('base64'),
    j960q2: fs.readFileSync(dir + 'f_960_q2.jpg').toString('base64'),
    j1920q6: fs.readFileSync(dir + 'f_1920_q6.jpg').toString('base64'),
    r960: fs.readFileSync(dir + 'f_960.rgba').toString('base64'),
    r1920: fs.readFileSync(dir + 'f_1920.rgba').toString('base64'),
  };
  const res = await page.evaluate(async (data) => {
    const med = (a) => { const s = [...a].sort((x, y) => x - y); return { min: s[0], med: s[s.length >> 1], p95: s[Math.floor(s.length * 0.95)] }; };
    const out = {};
    const img = document.getElementById('i');
    // 1. <img src=data:image/jpeg;base64> -> decode -> paint to a canvas (forces the real decode), as the Preview <img> does
    const probe = document.createElement('canvas');
    for (const [k, w, h] of [['j960q6', 960, 540], ['j960q2', 960, 540], ['j1920q6', 1920, 1080]]) {
      probe.width = w; probe.height = h;
      const pctx = probe.getContext('2d', { willReadFrequently: true });
      const t = [];
      for (let n = 0; n < 150; n++) {
        const i2 = new Image();
        // a comment-neutral change of the URL so no cache can answer: append a distinct query-free suffix via a different mime param
        const url = 'data:image/jpeg;x=' + n + ';base64,' + data[k];
        const t0 = performance.now();
        i2.src = url;
        await i2.decode();
        pctx.drawImage(i2, 0, 0);
        pctx.getImageData(0, 0, 1, 1);
        t.push(performance.now() - t0);
      }
      out['img data-URL decode+paint ' + k] = med(t);
    }

    // 1b. the same JPEG as raw bytes -> Blob -> createImageBitmap -> drawImage (no base64, no data URL)
    for (const [k, w, h] of [['j960q6', 960, 540], ['j1920q6', 1920, 1080]]) {
      probe.width = w; probe.height = h;
      const pctx = probe.getContext('2d', { willReadFrequently: true });
      const bytes = Uint8Array.from(atob(data[k]), (c) => c.charCodeAt(0));
      const t = [];
      for (let n = 0; n < 150; n++) {
        const t0 = performance.now();
        const bmp = await createImageBitmap(new Blob([bytes], { type: 'image/jpeg' }));
        pctx.drawImage(bmp, 0, 0); bmp.close();
        pctx.getImageData(0, 0, 1, 1);
        t.push(performance.now() - t0);
      }
      out['raw JPEG bytes -> createImageBitmap+paint ' + k] = med(t);
    }
    // 2. raw RGBA -> canvas putImageData (CPU canvas)
    const canvas = document.getElementById('c');
    for (const [k, w, h] of [['r960', 960, 540], ['r1920', 1920, 1080]]) {
      const bin = Uint8Array.from(atob(data[k]), (c) => c.charCodeAt(0));
      canvas.width = w; canvas.height = h;
      const ctx = canvas.getContext('2d');
      const t = [];
      for (let n = 0; n < 150; n++) {
        const t0 = performance.now();
        const copy = new Uint8ClampedArray(bin.buffer.slice(0)); // an IPC buffer arriving
        ctx.putImageData(new ImageData(copy, w, h), 0, 0);
        t.push(performance.now() - t0);
      }
      out['putImageData ' + k] = med(t);
      const t2 = [];
      for (let n = 0; n < 150; n++) {
        const t0 = performance.now();
        const copy = new Uint8ClampedArray(bin.buffer.slice(0));
        const bmp = await createImageBitmap(new ImageData(copy, w, h));
        ctx.drawImage(bmp, 0, 0);
        bmp.close();
        t2.push(performance.now() - t0);
      }
      out['createImageBitmap+drawImage ' + k] = med(t2);
    }
    // 3. WebGL texImage2D from a Uint8Array + draw
    for (const [k, w, h] of [['r960', 960, 540], ['r1920', 1920, 1080]]) {
      const bin = Uint8Array.from(atob(data[k]), (c) => c.charCodeAt(0));
      const cv = document.createElement('canvas'); cv.width = w; cv.height = h;
      const gl = cv.getContext('webgl2', { preserveDrawingBuffer: false });
      if (!gl) { out['webgl ' + k] = 'no webgl2'; continue; }
      const tex = gl.createTexture(); gl.bindTexture(gl.TEXTURE_2D, tex);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
      const t = [];
      for (let n = 0; n < 100; n++) {
        const t0 = performance.now();
        gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, w, h, 0, gl.RGBA, gl.UNSIGNED_BYTE, bin);
        gl.clear(gl.COLOR_BUFFER_BIT); gl.finish();
        t.push(performance.now() - t0);
      }
      out['webgl2 texImage2D+finish ' + k + ' (' + gl.getParameter(gl.RENDERER) + ')'] = med(t);
    }
    // 4. base64 + JSON string parse of a frame, as the Channel hands it to JS
    for (const k of ['j960q6', 'j1920q6']) {
      const t = [];
      for (let n = 0; n < 150; n++) {
        const msg = JSON.stringify({ time: 1.5, jpeg: 'data:image/jpeg;base64,' + data[k] });
        const t0 = performance.now();
        const o = JSON.parse(msg);
        t.push(performance.now() - t0 + 0 * o.time);
      }
      out['JSON.parse frame message ' + k] = med(t);
    }
    return out;
  }, data);
  console.log(JSON.stringify(res, null, 1));
  await browser.close();
})();
