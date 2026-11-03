// Fifth critic's driver: its own server on a strict port, its own headless Chrome (channel 'chrome',
// default Metal backend). Serves web3d/ at /web3d/, probe-site/ at /c/, and /flaky/<name>.wasm,
// which fails (503) on its first request and then serves the module, and /flaky/count.
//   PLAYWRIGHT_CORE=.../playwright-core/index.mjs node run5.mjs <port> <out.json> [groups...]
import http from 'node:http';
import path from 'node:path';
import { readFile, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
const { chromium } = await import(process.env.PLAYWRIGHT_CORE ?? 'playwright-core');

const port = Number(process.argv[2]);
const outFile = path.resolve(process.argv[3]);
const groups = process.argv.slice(4).length ? process.argv.slice(4) : ['x8', 'main', 'heap', 'exhaust', 'cap'];
const here = path.dirname(fileURLToPath(import.meta.url));
const web3d = path.resolve(here, '../../..');
const ROOTS = { '/web3d/': web3d, '/c/': path.join(here, 'probe-site') };
const TYPES = { '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8', '.wasm': 'application/wasm' };
const failuresLeft = { once: 1, race: 1 };
const requests = { once: 0, race: 0 };
const server = http.createServer(async (req, res) => {
  let p = decodeURIComponent(new URL(req.url, 'http://x').pathname);
  if (p === '/flaky/count') return res.writeHead(200, { 'Content-Type': 'application/json' }).end(JSON.stringify(requests));
  const flaky = /^\/flaky\/(once|race)\.wasm$/.exec(p);
  if (flaky) {
    requests[flaky[1]]++;
    if (failuresLeft[flaky[1]]-- > 0) return res.writeHead(503).end();
    const body = await readFile(path.join(web3d, 'pkg/loadngo_web3d.wasm'));
    res.writeHead(200, { 'Content-Type': 'application/wasm', 'Cache-Control': 'no-store' });
    return res.end(body);
  }
  if (p.endsWith('/')) p += 'index.html';
  const prefix = Object.keys(ROOTS).find((k) => p.startsWith(k));
  if (!prefix) return res.writeHead(404).end();
  const file = path.join(ROOTS[prefix], path.normalize(p.slice(prefix.length)));
  if (!file.startsWith(ROOTS[prefix])) return res.writeHead(403).end();
  try {
    const body = await readFile(file);
    res.writeHead(200, { 'Content-Type': TYPES[path.extname(file)] ?? 'application/octet-stream', 'Cache-Control': 'no-store' });
    res.end(body);
  } catch {
    res.writeHead(404).end();
  }
});
await new Promise((ok, no) => {
  server.once('error', no);
  server.listen(port, '127.0.0.1', ok);
});

const result = { groups: {} };
let browser;
try {
  browser = await chromium.launch({ channel: 'chrome', headless: true, args: ['--enable-gpu', '--ignore-gpu-blocklist'] });
  result.browser = browser.version();
  for (const group of groups) {
    const page = await browser.newPage();
    const messages = [];
    page.on('console', (m) => messages.push(`${m.type()}: ${m.text()}`.slice(0, 400)));
    page.on('pageerror', (e) => messages.push(`pageerror: ${e.message}`));
    await page.goto(`http://127.0.0.1:${port}/c/?group=${group}`);
    const r = await page.evaluate(() => window.__p5);
    if (group === 'main') {
      r.renderer = await page.evaluate(() => {
        const gl = document.createElement('canvas').getContext('webgl2');
        const d = gl.getExtension('WEBGL_debug_renderer_info');
        return gl.getParameter(d.UNMASKED_RENDERER_WEBGL);
      });
    }
    r.messages = messages.slice(0, 60);
    result.groups[group] = r;
    for (const t of r.tests) console.log(`${t.pass ? 'pass' : 'FAIL'}  ${t.name}`);
    await page.close();
  }
} finally {
  await browser?.close();
  server.closeAllConnections?.();
  await new Promise((ok) => server.close(ok));
  console.log('browser and server stopped');
}
await writeFile(outFile, `${JSON.stringify(result, null, 2)}\n`);
