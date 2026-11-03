// Eighth critic's driver: serves web3d/ at /web3d/ and probe-site/ at /c/ on one strict port,
// cross-origin isolated (for SharedArrayBuffer), opens probe-site/index.html in its own headless
// Google Chrome (playwright-core, channel 'chrome', the GPU), waits for window.__probe8.done, writes
// the results, and stops everything it started.
//   PLAYWRIGHT_CORE=.../playwright-core/index.mjs node run8.mjs <port> <out.json> [page, default index.html]
import http from 'node:http';
import path from 'node:path';
import { readFile, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';

const { chromium } = await import(process.env.PLAYWRIGHT_CORE ?? 'playwright-core');
const port = Number(process.argv[2]);
const outFile = path.resolve(process.argv[3]);
const pageName = process.argv[4] ?? 'index.html';
const here = path.dirname(fileURLToPath(import.meta.url));
const web3dDir = path.resolve(here, '../../..');
const ROOTS = { '/web3d/': web3dDir, '/c/': path.join(here, 'probe-site') };
const TYPES = { '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8', '.wasm': 'application/wasm', '.json': 'application/json' };

const server = http.createServer(async (req, res) => {
  const p = decodeURIComponent(new URL(req.url, 'http://x').pathname);
  const root = Object.keys(ROOTS).find((r) => p.startsWith(r));
  if (!root) return res.writeHead(404).end();
  const file = path.join(ROOTS[root], path.normalize(p.slice(root.length)));
  if (!file.startsWith(ROOTS[root])) return res.writeHead(403).end();
  try {
    const body = await readFile(file);
    res
      .writeHead(200, {
        'Content-Type': TYPES[path.extname(file)] ?? 'application/octet-stream',
        'Cache-Control': 'no-store',
        'Cross-Origin-Opener-Policy': 'same-origin',
        'Cross-Origin-Embedder-Policy': 'require-corp',
      })
      .end(body);
  } catch {
    res.writeHead(404).end();
  }
});
await new Promise((ok, no) => {
  server.once('error', no);
  server.listen(port, '127.0.0.1', ok);
});

let browser;
const result = { started: new Date().toISOString(), console: [], pageErrors: [] };
try {
  browser = await chromium.launch({ channel: 'chrome', headless: true, args: ['--enable-gpu', '--ignore-gpu-blocklist'] });
  result.browser = `Chrome ${browser.version()} (headless, own instance)`;
  const context = await browser.newContext({ viewport: { width: 800, height: 600 }, deviceScaleFactor: 1 });
  const page = await context.newPage();
  page.on('console', (m) => result.console.push(`${m.type()}: ${m.text()}`.slice(0, 400)));
  page.on('pageerror', (e) => result.pageErrors.push(String(e.message).slice(0, 400)));
  await page.goto(`http://127.0.0.1:${port}/c/${pageName}`);
  const done = await page.evaluate(() => Promise.race([window.__probe8.done, new Promise((r) => setTimeout(() => r({ timeout: true }), 120000))]));
  Object.assign(result, done);
  await context.close();
} finally {
  await browser?.close();
  server.closeAllConnections?.();
  await new Promise((r) => server.close(r));
}
const text = JSON.stringify(result, null, 2).replaceAll(process.env.HOME, '~');
await writeFile(outFile, `${text}\n`);
const lines = (result.probes ?? []).map((p) => `${p.pass ? 'pass' : 'FAIL'}  ${p.id}  ${p.claim}`);
console.log(`${result.browser}\n${lines.join('\n')}\npassed ${result.passed} of ${(result.probes ?? []).length}; page errors ${result.pageErrors.length}${result.timeout ? '; TIMED OUT' : ''}`);
console.log('browser and server stopped');
