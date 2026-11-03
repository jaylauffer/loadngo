// Serves web3d/ and site/ on a strict port; one headless Chrome (V8 natives syntax on); waits <secs>, then samples the JS heap
// for 10 s and prints sampled bytes per frame by function, and V8's optimisation status of the per-frame methods. Stops both.
//   node run.mjs <port> <secs> '?bare=1[&cpu=1]'
import http from 'node:http'; import path from 'node:path'; import { readFile } from 'node:fs/promises';
// playwright-core from PLAYWRIGHT_CORE (a path to its index.mjs), else from node_modules.
const { chromium } = await import(process.env.PLAYWRIGHT_CORE ?? 'playwright-core');
const port = Number(process.argv[2]); const secs = Number(process.argv[3] ?? 30);
const ROOTS = { '/web3d/': '/Users/jay/pudding/loadngo/web3d', '/': path.join(path.dirname(new URL(import.meta.url).pathname), 'site') };
const T = { '.html': 'text/html', '.js': 'text/javascript', '.wasm': 'application/wasm' };
const server = http.createServer(async (req, res) => { const p = decodeURIComponent(new URL(req.url, 'http://x').pathname); const k = Object.keys(ROOTS).find((x) => p.startsWith(x)); let rel = p.slice(k.length); if (!rel || rel.endsWith('/')) rel += 'index.html'; try { res.writeHead(200, { 'Content-Type': T[path.extname(rel)] ?? 'application/octet-stream' }); res.end(await readFile(path.join(ROOTS[k], rel))); } catch { res.writeHead(404).end(); } });
await new Promise((ok, no) => { server.once('error', no); server.listen(port, '127.0.0.1', ok); });
let browser;
try {
  browser = await chromium.launch({ channel: 'chrome', headless: true, args: ['--enable-gpu', '--ignore-gpu-blocklist', '--js-flags=--allow-natives-syntax'] });
  const page = await browser.newPage(); const msgs = []; page.on('console', (m) => msgs.push(m.text())); page.on('pageerror', (e) => msgs.push(`pageerror: ${e.message}`));
  await page.goto(`http://127.0.0.1:${port}/${process.argv[4] ?? ''}`);
  await page.waitForTimeout(secs * 1000);
  const cdp = await page.context().newCDPSession(page); await cdp.send('HeapProfiler.enable'); await cdp.send('HeapProfiler.collectGarbage');
  await cdp.send('HeapProfiler.startSampling', { samplingInterval: 32, includeObjectsCollectedByMajorGC: true, includeObjectsCollectedByMinorGC: true });
  const f0 = await page.evaluate(() => window.__status().frames); await page.waitForTimeout(10000); const f1 = await page.evaluate(() => window.__status().frames);
  const { profile } = await cdp.send('HeapProfiler.stopSampling'); const by = {};
  const walk = (n) => { const { url, functionName, lineNumber } = n.callFrame; if (n.selfSize > 0 && url) { const k = `${new URL(url).pathname}:${lineNumber + 1} ${functionName}`; by[k] = (by[k] ?? 0) + n.selfSize; } n.children.forEach(walk); }; walk(profile.head);
  console.log(process.argv[4] || 'new', 'bytes/frame by function:', JSON.stringify(Object.fromEntries(Object.entries(by).map(([k, v]) => [k, Math.round(v / (f1 - f0) * 10) / 10]))), 'status', JSON.stringify(await page.evaluate(() => window.__status())));
  if (msgs.length) console.log(msgs);
} finally { await browser?.close(); server.closeAllConnections?.(); await new Promise((r) => server.close(r)); console.log('browser and server stopped'); }
