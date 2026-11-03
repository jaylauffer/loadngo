// Sixth critic's driver: its own servers on two strict ports (the page's origin, and a second origin
// for a cross-origin image), its own headless Chrome (channel 'chrome', default Metal backend).
// Serves web3d/ at /web3d/, probe-site/ at /c/, and /count/*.wasm (the module, counted per path;
// garbage.wasm is not a module); /count/n returns the counts. Stops everything it starts.
//   PLAYWRIGHT_CORE=.../playwright-core/index.mjs node run6.mjs <port> <out.json>
import http from 'node:http';
import path from 'node:path';
import zlib from 'node:zlib';
import { readFile, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
const { chromium } = await import(process.env.PLAYWRIGHT_CORE ?? 'playwright-core');
const port = Number(process.argv[2]);
const outFile = path.resolve(process.argv[3]);
const page = process.argv[4] ?? 'index.html';
const here = path.dirname(fileURLToPath(import.meta.url));
const web3dDir = path.resolve(here, '../../..');
const ROOTS = { '/web3d/': web3dDir, '/c/': path.join(here, 'probe-site') };
const TYPES = { '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8', '.wasm': 'application/wasm', '.json': 'application/json', '.png': 'image/png' };
const counts = {};

/** A 4x4 PNG of four quadrants: red, green / blue, white. */
function png() {
  const crc = (buf) => {
    let c = ~0;
    for (const b of buf) {
      c ^= b;
      for (let k = 0; k < 8; k++) c = c & 1 ? (c >>> 1) ^ 0xedb88320 : c >>> 1;
    }
    return ~c >>> 0;
  };
  const chunk = (type, data) => {
    const len = Buffer.alloc(4);
    len.writeUInt32BE(data.length);
    const td = Buffer.concat([Buffer.from(type), data]);
    const c = Buffer.alloc(4);
    c.writeUInt32BE(crc(td));
    return Buffer.concat([len, td, c]);
  };
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(4, 0);
  ihdr.writeUInt32BE(4, 4);
  ihdr[8] = 8;
  ihdr[9] = 6;
  const px = (y, x) => (y < 2 ? (x < 2 ? [255, 0, 0, 255] : [0, 255, 0, 255]) : x < 2 ? [0, 0, 255, 255] : [255, 255, 255, 255]);
  const raw = [];
  for (let y = 0; y < 4; y++) {
    raw.push(0);
    for (let x = 0; x < 4; x++) raw.push(...px(y, x));
  }
  return Buffer.concat([Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]), chunk('IHDR', ihdr), chunk('IDAT', zlib.deflateSync(Buffer.from(raw))), chunk('IEND', Buffer.alloc(0))]);
}

const server = http.createServer(async (req, res) => {
  const p = decodeURIComponent(new URL(req.url, 'http://x').pathname);
  if (p === '/count/n') {
    res.writeHead(200, { 'Content-Type': 'application/json', 'Cache-Control': 'no-store' }).end(JSON.stringify(counts));
    return;
  }
  if (p.startsWith('/count/')) {
    counts[p] = (counts[p] ?? 0) + 1;
    const body = p === '/count/garbage.wasm' ? Buffer.from('not a wasm module at all') : await readFile(path.join(web3dDir, 'pkg/loadngo_web3d.wasm'));
    res.writeHead(200, { 'Content-Type': 'application/wasm', 'Cache-Control': 'no-store' }).end(body);
    return;
  }
  const root = Object.keys(ROOTS).find((r) => p.startsWith(r));
  if (!root) return res.writeHead(404).end();
  const file = path.join(ROOTS[root], path.normalize(p.slice(root.length)));
  if (!file.startsWith(ROOTS[root])) return res.writeHead(403).end();
  try {
    const body = await readFile(file);
    res.writeHead(200, { 'Content-Type': TYPES[path.extname(file)] ?? 'application/octet-stream', 'Cache-Control': 'no-store' }).end(body);
  } catch {
    res.writeHead(404).end();
  }
});
const foreign = http.createServer((req, res) => {
  if (req.url === '/quadrants.png') res.writeHead(200, { 'Content-Type': 'image/png' }).end(png());
  else res.writeHead(404).end();
});
const listen = (s, p) => new Promise((ok, no) => { s.once('error', no); s.listen(p, '127.0.0.1', ok); });
await listen(server, port);
await listen(foreign, port + 1);
let browser;
try {
  browser = await chromium.launch({ channel: 'chrome', headless: true, args: ['--enable-gpu', '--ignore-gpu-blocklist'] });
  const context = await browser.newContext({ viewport: { width: 900, height: 600 }, deviceScaleFactor: 1 });
  const tab = await context.newPage();
  const messages = [];
  tab.on('console', (m) => messages.push(`${m.type()}: ${m.text()}`));
  tab.on('pageerror', (e) => messages.push(`pageerror: ${e.message}`));
  await tab.addInitScript((o) => { window.__foreignOrigin = o; }, `http://localhost:${port + 1}`);
  await tab.goto(`http://127.0.0.1:${port}/c/${page}`);
  const result = await tab.evaluate(() => (window.__probe6 ?? window.__limit).done);
  const renderer = await tab.evaluate(() => {
    const gl = document.createElement('canvas').getContext('webgl2');
    const ext = gl.getExtension('WEBGL_debug_renderer_info');
    return gl.getParameter(ext ? ext.UNMASKED_RENDERER_WEBGL : gl.RENDERER);
  });
  if (/SwiftShader|llvmpipe|software/i.test(renderer)) throw new Error(`software renderer: ${renderer}`);
  const report = { browser: browser.version(), renderer, served: counts, result, messages };
  await writeFile(outFile, `${JSON.stringify(report, null, 2)}\n`);
  for (const t of result.probes ?? []) console.log(`${t.pass ? 'pass' : 'FAIL'}  ${t.id} ${t.claim}`);
  console.log(`${result.passed} passed; failed: ${JSON.stringify(result.failed)}; renderer ${renderer}; console messages ${messages.length}`);
} finally {
  await browser?.close();
  server.closeAllConnections?.();
  foreign.closeAllConnections?.();
  await new Promise((r) => server.close(r));
  await new Promise((r) => foreign.close(r));
  console.log('browser and servers stopped');
}
