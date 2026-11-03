// Third critic's driver: its own server on a strict port, its own headless Chrome (channel 'chrome',
// default Metal backend) on the Mac's GPU; runs probe-site/ and stops both.
//   node run3.mjs <port> <out.json>
import http from 'node:http';
import path from 'node:path';
import { readFile, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
// playwright-core from PLAYWRIGHT_CORE (a path to its index.mjs), else from node_modules.
const { chromium } = await import(process.env.PLAYWRIGHT_CORE ?? 'playwright-core');

const port = Number(process.argv[2]);
const outFile = path.resolve(process.argv[3]);
const here = path.dirname(fileURLToPath(import.meta.url));
const ROOTS = { '/web3d/': path.resolve(here, '../../..'), '/c/': path.join(here, 'probe-site') };
const TYPES = { '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8', '.wasm': 'application/wasm' };
const server = http.createServer(async (req, res) => {
  let p = decodeURIComponent(new URL(req.url, 'http://x').pathname);
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
let browser;
try {
  browser = await chromium.launch({ channel: 'chrome', headless: true, args: ['--enable-gpu', '--ignore-gpu-blocklist'] });
  const page = await browser.newPage();
  const messages = [];
  page.on('console', (m) => messages.push(`${m.type()}: ${m.text()}`));
  page.on('pageerror', (e) => messages.push(`pageerror: ${e.message}`));
  await page.goto(`http://127.0.0.1:${port}/c/`);
  const res = await page.evaluate(() => window.__p3);
  res.browser = browser.version();
  res.messages = messages;
  await writeFile(outFile, `${JSON.stringify(res, null, 2)}\n`);
  for (const t of res.tests) console.log(`${t.pass ? 'pass' : 'FAIL'}  ${t.name}`);
  console.log(`renderer ${res.info?.renderer}; error ${res.error ?? 'none'}; messages ${messages.length}`);
} finally {
  await browser?.close();
  server.closeAllConnections?.();
  await new Promise((ok) => server.close(ok));
  console.log('browser and server stopped');
}
