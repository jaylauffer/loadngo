// Tenth critic's driver: serves web3d/ at /web3d/ and site/ at /c/ on two strict ports of
// 127.0.0.1 (two origins), opens each probe page in its own headless Google Chrome
// (playwright-core, channel 'chrome', the GPU), gives pages window.__listeners(id) (DevTools
// protocol), records console messages, writes results beside this file, and stops everything.
//   PLAYWRIGHT_CORE=.../playwright-core/index.mjs node run10.mjs <port> <port2> <page...>
// Pages: state, pm, notop, xtop, bf (bf runs a Chrome with the back/forward cache allowed).
import http from 'node:http';
import path from 'node:path';
import { readFile, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';

const { chromium } = await import(process.env.PLAYWRIGHT_CORE ?? 'playwright-core');
const [port, port2] = [Number(process.argv[2]), Number(process.argv[3])];
const pages = process.argv.slice(4);
const here = path.dirname(fileURLToPath(import.meta.url));
const web3dDir = path.resolve(here, '../../..');
const ROOTS = { '/web3d/': web3dDir, '/c/': path.join(here, '../critic10/site') }; // the sixteenth critic's copy of the sixteenth round's copy: the tenth critic's pages, results written to rerun/ here
const TYPES = { '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8', '.wasm': 'application/wasm' };
const serve = (p) =>
  new Promise((ok, no) => {
    const s = http.createServer(async (req, res) => {
      const u = decodeURIComponent(new URL(req.url, 'http://x').pathname);
      const root = Object.keys(ROOTS).find((r) => u.startsWith(r));
      if (!root) return res.writeHead(404).end();
      const file = path.join(ROOTS[root], path.normalize(u.slice(root.length)));
      if (!file.startsWith(ROOTS[root])) return res.writeHead(403).end();
      try {
        const body = await readFile(file);
        res.writeHead(200, { 'Content-Type': TYPES[path.extname(file)] ?? 'application/octet-stream', 'Cache-Control': 'no-cache' }).end(body);
      } catch {
        res.writeHead(404).end();
      }
    });
    s.once('error', no);
    s.listen(p, '127.0.0.1', () => ok(s));
  });
const servers = [await serve(port), await serve(port2)];
const main = `http://127.0.0.1:${port}`;
const foreign = `http://127.0.0.1:${port2}`;

const scrub = (o) => JSON.parse(JSON.stringify(o).replaceAll(process.env.HOME, '~'));
async function open(name) {
  const bfcache = name === 'bf';
  const browser = await chromium.launch({
    channel: 'chrome',
    headless: true,
    args: ['--enable-gpu', '--ignore-gpu-blocklist'],
    ...(bfcache ? { ignoreDefaultArgs: ['--disable-back-forward-cache'] } : {}),
  });
  const result = { page: name, browser: `Chrome ${browser.version()} (headless, own instance)`, console: [], pageErrors: [] };
  try {
    const context = await browser.newContext({ viewport: { width: 800, height: 600 } });
    const page = await context.newPage();
    const cdp = await context.newCDPSession(page);
    await page.exposeFunction('__listeners', async (id) => {
      const { result: el } = await cdp.send('Runtime.evaluate', { expression: `document.getElementById(${JSON.stringify(id)})` });
      if (!el.objectId) return { error: `no element ${id}` };
      const { listeners } = await cdp.send('DOMDebugger.getEventListeners', { objectId: el.objectId });
      const counts = {};
      for (const l of listeners) counts[l.type] = (counts[l.type] ?? 0) + 1;
      await cdp.send('Runtime.releaseObject', { objectId: el.objectId });
      return counts;
    });
    await page.addInitScript((f) => {
      window.__foreign = f;
    }, foreign);
    page.on('console', (m) => result.console.push(`${m.type()}: ${m.text()}`.slice(0, 300)));
    page.on('pageerror', (e) => result.pageErrors.push(String(e.message).slice(0, 300)));
    const wait = (target) => target.evaluate(() => Promise.race([window.__probe10.done, new Promise((r) => setTimeout(() => r({ timeout: true }), 120000))]));
    if (name === 'xtop') {
      await page.goto(`${foreign}/c/xtop.html?host=${encodeURIComponent(main)}`);
      let f;
      for (let i = 0; i < 200 && !(f = page.frames().find((x) => x.url().endsWith('/c/nest-host.html'))); i++) await new Promise((r) => setTimeout(r, 25));
      await f.waitForFunction(() => window.__probe10);
      result.out = await wait(f);
    } else if (bfcache) {
      await page.goto(`${main}/c/bf.html`);
      await page.waitForFunction(() => window.__bf?.ready);
      await page.evaluate(() => window.__bf.ready);
      result.before = await page.evaluate(() => window.__bf.before);
      await page.goto(`${main}/c/blank.html`);
      await page.goBack({ waitUntil: 'commit' });
      await page.waitForFunction(() => window.__bf?.r);
      result.out = await page.evaluate(() => window.__bf.after());
    } else {
      await page.goto(`${main}/c/${name}.html`);
      await page.waitForFunction(() => window.__probe10);
      result.out = await wait(page);
      if (name === 'state2') {
        // What the screen shows of the two drawingBufferColorSpace canvases: each screenshot's
        // centre pixel, decoded by the page (the screenshot is sRGB, as Chrome composites it).
        result.screen = {};
        for (const id of ['space-control', 'space-p3']) {
          const b64 = (await page.locator(`#${id}`).screenshot()).toString('base64');
          result.screen[id] = await page.evaluate(async (b) => {
            const bmp = await createImageBitmap(await (await fetch(`data:image/png;base64,${b}`)).blob());
            const c = new OffscreenCanvas(bmp.width, bmp.height).getContext('2d');
            c.drawImage(bmp, 0, 0);
            return Array.from(c.getImageData(bmp.width >> 1, bmp.height >> 1, 1, 1).data.slice(0, 3));
          }, b64);
        }
      }
    }
    result.gpu = await page.evaluate(() => {
      const gl = document.createElement('canvas').getContext('webgl2');
      const d = gl?.getExtension('WEBGL_debug_renderer_info');
      const v = d ? gl.getParameter(d.UNMASKED_RENDERER_WEBGL) : null;
      gl?.getExtension('WEBGL_lose_context')?.loseContext();
      return v;
    });
    await context.close();
  } catch (e) {
    result.driverError = String(e?.stack ?? e).slice(0, 1000);
  } finally {
    await browser.close();
  }
  await writeFile(path.join(here, 'rerun', `run10-${name}.json`), `${JSON.stringify(scrub(result), null, 2)}\n`);
  return result;
}
try {
  for (const name of pages) {
    const r = await open(name);
    console.log(`== ${name}: ${r.browser}; gpu ${r.gpu}; page errors ${r.pageErrors.length}${r.driverError ? `; DRIVER ERROR ${r.driverError.split('\n')[0]}` : ''}`);
  }
} finally {
  for (const s of servers) {
    s.closeAllConnections?.();
    await new Promise((r) => s.close(r));
  }
  console.log('browsers and servers stopped');
}
