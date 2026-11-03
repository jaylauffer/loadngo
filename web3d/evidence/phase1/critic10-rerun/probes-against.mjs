// Runs web3d/demo/probe.html, the current probes, against any pkg/ directory: the before-the-fix
// control. Serves web3d/demo/ and web3d/tools/ from this checkout and /pkg/ from the directory
// given, cross-origin isolated and with CORP, so that the second origin probe XR embeds (localhost,
// against the page's 127.0.0.1, from the same server) loads; on one strict port; opens the page in
// its own headless Google Chrome
// (playwright-core, channel 'chrome', the GPU); gives it window.__listeners(id) as check.mjs does;
// writes the results with the home directory replaced by "~"; stops its browser and server.
//   PLAYWRIGHT_CORE=.../playwright-core/index.mjs node probes-against.mjs <port> <pkg dir> <out.json>
import http from 'node:http';
import path from 'node:path';
import { readFile, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';

const { chromium } = await import(process.env.PLAYWRIGHT_CORE ?? 'playwright-core');
const port = Number(process.argv[2]);
const pkgDir = path.resolve(process.argv[3]);
const outFile = path.resolve(process.argv[4]);
const web3d = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../..');
const ROOTS = { '/pkg/': pkgDir, '/demo/': path.join(web3d, 'demo'), '/tools/': path.join(web3d, 'tools') };
const TYPES = { '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8', '.wasm': 'application/wasm', '.txt': 'text/plain; charset=utf-8' };
const server = http.createServer(async (req, res) => {
  const p = decodeURIComponent(new URL(req.url, 'http://x').pathname);
  const root = Object.keys(ROOTS).find((r) => p.startsWith(r));
  if (!root) return res.writeHead(404).end();
  const file = path.join(ROOTS[root], path.normalize(p.slice(root.length)));
  if (!file.startsWith(ROOTS[root])) return res.writeHead(403).end();
  try {
    const body = await readFile(file);
    res.writeHead(200, { 'Content-Type': TYPES[path.extname(file)] ?? 'application/octet-stream', 'Cache-Control': 'no-store', 'Cross-Origin-Opener-Policy': 'same-origin', 'Cross-Origin-Embedder-Policy': 'require-corp', 'Cross-Origin-Resource-Policy': 'cross-origin' }).end(body);
  } catch {
    res.writeHead(404).end();
  }
});
await new Promise((ok, no) => {
  server.once('error', no);
  server.listen(port, '127.0.0.1', ok);
});
let browser;
// Named by its last two components only: the directory may be anywhere on this machine.
const pkgLabel = pkgDir.startsWith(web3d + path.sep) ? path.relative(web3d, pkgDir) : `<outside the checkout>/${pkgDir.split(path.sep).slice(-2).join('/')}`;
const result = { started: new Date().toISOString(), pkg: pkgLabel, messages: [] };
try {
  browser = await chromium.launch({ channel: 'chrome', headless: true, args: ['--enable-gpu', '--ignore-gpu-blocklist'] });
  result.browser = `Chrome ${browser.version()} (headless, own instance)`;
  const context = await browser.newContext({ viewport: { width: 800, height: 300 }, deviceScaleFactor: 1 });
  const page = await context.newPage();
  const cdp = await context.newCDPSession(page);
  await page.exposeFunction('__listeners', async (id) => {
    const { result: el } = await cdp.send('Runtime.evaluate', { expression: `document.getElementById(${JSON.stringify(String(id))})` });
    if (!el.objectId) return { error: `no element ${id}` };
    const { listeners } = await cdp.send('DOMDebugger.getEventListeners', { objectId: el.objectId });
    await cdp.send('Runtime.releaseObject', { objectId: el.objectId });
    const counts = {};
    for (const l of listeners) counts[l.type] = (counts[l.type] ?? 0) + 1;
    return counts;
  });
  page.on('console', (m) => result.messages.push(`${m.type()}: ${m.text()}`.slice(0, 300)));
  page.on('pageerror', (e) => result.messages.push(`pageerror: ${String(e.message).slice(0, 300)}`));
  await page.goto(`http://127.0.0.1:${port}/demo/probe.html`);
  const done = await page.evaluate(() => Promise.race([window.__probe.done, new Promise((r) => setTimeout(() => r({ timeout: true }), 240000))]));
  Object.assign(result, done);
  await context.close();
} finally {
  await browser?.close();
  server.closeAllConnections?.();
  await new Promise((r) => server.close(r));
}
await writeFile(outFile, `${JSON.stringify(result, null, 2).replaceAll(process.env.HOME, '~')}\n`);
for (const t of result.tests ?? []) console.log(`${t.pass ? 'pass' : 'FAIL'}  ${t.name.slice(0, 150)}`);
const invalid = result.messages.filter((m) => /INVALID_OPERATION/.test(m)).length;
console.log(`${result.browser}\npassed ${result.passed} of ${(result.tests ?? []).length}; INVALID_OPERATION warnings ${invalid}${result.timeout ? '; TIMED OUT' : ''}${result.error ? `; error ${result.error.slice(0, 200)}` : ''}`);
console.log('browser and server stopped');
