// Added by the builder after round 2: runs the second critic's critic.js (GPU probes against web3d and a
// real three.js r180 WebGLRenderer) in its own headless Chrome, with its own server on a strict port,
// and stops both.
//   PLAYWRIGHT_CORE=.../playwright-core/index.mjs THREE_BUILD=.../three/build node run-critic.mjs <port> <out dir>
import http from 'node:http';
import path from 'node:path';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
// playwright-core from PLAYWRIGHT_CORE (a path to its index.mjs), else from node_modules.
const { chromium } = await import(process.env.PLAYWRIGHT_CORE ?? 'playwright-core');

const port = Number(process.argv[2]);
if (!process.env.THREE_BUILD) throw new Error('set THREE_BUILD to a three.js r180 build directory');
const outDir = path.resolve(process.argv[3]);
const here = path.dirname(fileURLToPath(import.meta.url));
const ROOTS = {
  '/web3d/': path.resolve(here, '../../..'),
  // three.js r180's build directory (node_modules/three/build), for the real-three.js comparison.
  '/three/': process.env.THREE_BUILD,
  '/c/': here,
};
const TYPES = { '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8', '.wasm': 'application/wasm', '.txt': 'text/plain' };
const server = http.createServer(async (req, res) => {
  const p = decodeURIComponent(new URL(req.url, 'http://x').pathname);
  const prefix = Object.keys(ROOTS).find((k) => p.startsWith(k));
  if (!prefix) return res.writeHead(404).end();
  let rel = p.slice(prefix.length);
  if (rel === '' || rel.endsWith('/')) rel += 'index.html';
  const root = ROOTS[prefix];
  const file = path.join(root, path.normalize(rel));
  if (!file.startsWith(root + path.sep)) return res.writeHead(403).end();
  try {
    const body = await readFile(file);
    res.writeHead(200, { 'Content-Type': TYPES[path.extname(file)] ?? 'application/octet-stream', 'Cache-Control': 'no-store' });
    res.end(body);
  } catch {
    res.writeHead(404).end();
  }
});
await new Promise((resolve, reject) => {
  server.once('error', reject);
  server.listen(port, '127.0.0.1', resolve);
});
let browser;
let failed = true;
try {
  browser = await chromium.launch({ channel: 'chrome', headless: true, args: ['--enable-gpu', '--ignore-gpu-blocklist'] });
  const page = await browser.newPage();
  const messages = [];
  page.on('console', (m) => messages.push(`${m.type()}: ${m.text()}`));
  page.on('pageerror', (e) => messages.push(`pageerror: ${e.message}`));
  await page.goto(`http://127.0.0.1:${port}/c/`);
  const out = await page.evaluate(() => window.__critic.done);
  if (/SwiftShader|llvmpipe|softpipe|software|Basic Render/i.test(out.renderer ?? '')) throw new Error(`software renderer: ${out.renderer}`);
  const invalid = messages.filter((m) => /INVALID_OPERATION|does not belong/.test(m));
  await mkdir(outDir, { recursive: true });
  await writeFile(path.join(outDir, 'report-probe.json'), `${JSON.stringify({ browser: browser.version(), probe: out, messages, invalidOperation: invalid }, null, 2)}\n`);
  for (const t of out.tests) console.log(`${t.pass ? 'pass' : 'FAIL'}  ${t.name}`);
  console.log(`${out.passed} passed, ${out.failed.length} failed; INVALID_OPERATION warnings ${invalid.length}; renderer ${out.renderer}`);
  failed = out.failed.length > 0 || invalid.length > 0 || Boolean(out.error);
  if (out.error) console.log(`error: ${out.error}`);
} finally {
  await browser?.close();
  server.closeAllConnections?.();
  await new Promise((r) => server.close(r));
  console.log('browser and server stopped');
}
process.exit(failed ? 1 : 0);
