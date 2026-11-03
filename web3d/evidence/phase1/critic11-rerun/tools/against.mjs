// The before-the-fix control: runs a page of this checkout against any pkg/ directory (such as the
// code the eleventh critic judged, built by ../../critic10-rerun/build-before-fix.sh). Serves web3d/
// from this checkout with /pkg/ from the directory given, on one strict port, for any Host (so
// a.localhost and b.a.localhost are further origins). --isolated sends COOP, COEP and CORP, as
// check.mjs does for the probe page; otherwise Origin-Agent-Cluster: ?0, as the eleventh critic's
// server does, so a page may set document.domain. Opens the page in its own headless Google Chrome
// (playwright-core, channel 'chrome', the GPU), gives it window.__listeners as check.mjs does, waits
// for window.__probe.done or window.__done, writes the result with the home directory as "~", and
// stops its browser and server.
// The result names the pkg/ by --label (default: its path within web3d/, or "a pkg/ outside the
// repository"), never by a path outside the repository.
//   PLAYWRIGHT_CORE=.../playwright-core/index.mjs node against.mjs <port> <pkg dir> <page under web3d/> <out.json> [--host h] [--isolated] [--label text]
// BSD-3-Clause; see the repository's LICENSE.
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import { readFile, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';

const { chromium } = await import(process.env.PLAYWRIGHT_CORE ?? 'playwright-core');
const argv = process.argv.slice(2);
const port = Number(argv[0]);
const pkgDir = path.resolve(argv[1]);
const page = argv[2];
const outFile = path.resolve(argv[3]);
const host = argv.includes('--host') ? argv[argv.indexOf('--host') + 1] : '127.0.0.1';
const isolated = argv.includes('--isolated');
const label = argv.includes('--label') ? argv[argv.indexOf('--label') + 1] : null;
const web3d = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../../..');
const TYPES = { '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8', '.mjs': 'text/javascript; charset=utf-8', '.wasm': 'application/wasm', '.txt': 'text/plain; charset=utf-8', '.json': 'application/json' };
const headers = isolated
  ? { 'Cross-Origin-Opener-Policy': 'same-origin', 'Cross-Origin-Embedder-Policy': 'require-corp', 'Cross-Origin-Resource-Policy': 'cross-origin' }
  : { 'Origin-Agent-Cluster': '?0' };
const server = http.createServer(async (req, res) => {
  let p = decodeURIComponent(new URL(req.url, 'http://x').pathname);
  if (p.endsWith('/')) p += 'index.html';
  const [root, rel] = p.startsWith('/pkg/') ? [pkgDir, p.slice('/pkg/'.length)] : [web3d, p.slice(1)];
  const file = path.join(root, path.normalize(rel));
  if (!file.startsWith(root + path.sep)) return res.writeHead(403).end();
  try {
    const body = await readFile(file);
    res.writeHead(200, { 'Content-Type': TYPES[path.extname(file)] ?? 'application/octet-stream', 'Cache-Control': 'no-store', ...headers }).end(body);
  } catch {
    res.writeHead(404).end();
  }
});
await new Promise((resolve, reject) => {
  server.once('error', reject);
  server.listen(port, '127.0.0.1', resolve);
});
let browser;
let code = 0;
try {
  browser = await chromium.launch({ channel: 'chrome', headless: true, args: ['--enable-gpu', '--ignore-gpu-blocklist'] });
  const context = await browser.newContext({ viewport: { width: 800, height: 300 }, deviceScaleFactor: 1 });
  const p = await context.newPage();
  const messages = [];
  p.on('console', (m) => messages.push(`${m.type()}: ${m.text()}`));
  p.on('pageerror', (e) => messages.push(`pageerror: ${e.message}`));
  const cdp = await context.newCDPSession(p);
  const frames = new Set();
  cdp.on('Runtime.executionContextCreated', ({ context: c }) => c.auxData?.isDefault && frames.add(c.id));
  cdp.on('Runtime.executionContextDestroyed', ({ executionContextId }) => frames.delete(executionContextId));
  cdp.on('Runtime.executionContextsCleared', () => frames.clear());
  await cdp.send('Runtime.enable');
  await p.exposeFunction('__listeners', async (id) => {
    const key = String(id);
    const expression = key.startsWith('@') ? `globalThis.top.__targets[${JSON.stringify(key.slice(1))}]` : `globalThis.top.document.getElementById(${JSON.stringify(key)})`;
    let best = null;
    for (const contextId of [...frames]) {
      const { result } = await cdp.send('Runtime.evaluate', { expression, contextId }).catch(() => ({ result: {} }));
      if (!result?.objectId) continue;
      const { listeners } = await cdp.send('DOMDebugger.getEventListeners', { objectId: result.objectId });
      await cdp.send('Runtime.releaseObject', { objectId: result.objectId });
      if (!best || listeners.length > best.length) best = listeners;
    }
    if (!best) return { error: `no object ${key}` };
    const counts = {};
    for (const l of best) counts[l.type] = (counts[l.type] ?? 0) + 1;
    return counts;
  });
  await p.goto(`http://${host}:${port}/${page}`);
  await p.waitForFunction(() => window.__probe?.done || window.__done, null, { timeout: 170000 });
  const result = await p.evaluate(() => (window.__probe ? window.__probe.done : window.__done));
  const pkg = label ?? (pkgDir.startsWith(web3d + path.sep) ? path.relative(web3d, pkgDir) : 'a pkg/ outside the repository');
  const report = { page, host, pkg, chrome: browser.version(), result, messages };
  await writeFile(outFile, `${JSON.stringify(report, null, 2).replaceAll(os.homedir(), '~')}\n`);
  if (result.tests) {
    console.log(`${result.passed} passed, ${result.failed?.length} failed: ${(result.failed ?? []).map((n) => n.slice(0, 2)).join(' ')}`);
  } else console.log(`pass: ${result.pass}`);
} catch (e) {
  console.error('driver error:', e);
  code = 1;
} finally {
  await browser?.close();
  server.closeAllConnections?.();
  await new Promise((resolve) => server.close(resolve));
  console.log('browser and server stopped');
}
process.exit(code);
