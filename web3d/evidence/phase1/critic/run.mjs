// Critic's driver: own server on a strict port, own headless Chrome on the GPU; stops both.
import http from 'node:http';
import path from 'node:path';
import { readFile, writeFile } from 'node:fs/promises';
// playwright-core from PLAYWRIGHT_CORE (a path to its index.mjs), else from node_modules.
const { chromium } = await import(process.env.PLAYWRIGHT_CORE ?? 'playwright-core');

const port = Number(process.argv[2]);
const outDir = process.argv[3];
const ROOTS = { '/web3d/': '/Users/jay/pudding/loadngo/web3d', '/probe/': path.join(path.dirname(new URL(import.meta.url).pathname), 'probe-site') };
const TYPES = { '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8', '.wasm': 'application/wasm' };
const server = http.createServer(async (req, res) => {
  const p = decodeURIComponent(new URL(req.url, 'http://x').pathname);
  const prefix = Object.keys(ROOTS).find((k) => p.startsWith(k));
  if (!prefix) return res.writeHead(404).end();
  const root = ROOTS[prefix];
  let rel = p.slice(prefix.length);
  if (rel === '' || rel.endsWith('/')) rel += 'index.html';
  const file = path.join(root, path.normalize(rel));
  if (!file.startsWith(root + path.sep)) return res.writeHead(403).end();
  try {
    const body = await readFile(file);
    res.writeHead(200, { 'Content-Type': TYPES[path.extname(file)] ?? 'application/octet-stream', 'Cache-Control': 'no-store', 'Cross-Origin-Opener-Policy': 'same-origin', 'Cross-Origin-Embedder-Policy': 'require-corp' });
    res.end(body);
  } catch { res.writeHead(404).end(); }
});
await new Promise((resolve, reject) => { server.once('error', reject); server.listen(port, '127.0.0.1', resolve); });
const SOFTWARE = /SwiftShader|llvmpipe|softpipe|software|Basic Render/i;
const report = {};
let browser;
try {
  browser = await chromium.launch({ channel: 'chrome', headless: true, args: ['--enable-gpu', '--ignore-gpu-blocklist'] });
  report.browser = browser.version();
  // 1. Probe page.
  {
    const ctx = await browser.newContext({ viewport: { width: 800, height: 300 }, deviceScaleFactor: 1 });
    const page = await ctx.newPage();
    const msgs = [];
    page.on('console', (m) => msgs.push(`${m.type()}: ${m.text()}`));
    page.on('pageerror', (e) => msgs.push(`pageerror: ${e.message}`));
    await page.goto(`http://127.0.0.1:${port}/probe/`);
    const out = await page.evaluate(() => window.__probe.done);
    if (SOFTWARE.test(out.info?.renderer ?? '')) throw new Error(`software renderer: ${out.info.renderer}`);
    report.probe = out;
    report.probeMessages = msgs;
    await ctx.close();
  }
  // 2. Demo, still, for visual inspection; and a close-up of the textured sphere.
  for (const [name, q] of [['still-full', 'still=1&scale=1'], ['still-noaa', 'still=1&scale=1&aa=0']]) {
    const ctx = await browser.newContext({ viewport: { width: 1366, height: 768 }, deviceScaleFactor: 1 });
    const page = await ctx.newPage();
    const msgs = [];
    page.on('console', (m) => { if (m.type() === 'error' || m.type() === 'warning') msgs.push(m.text()); });
    page.on('pageerror', (e) => msgs.push(`pageerror: ${e.message}`));
    await page.goto(`http://127.0.0.1:${port}/web3d/demo/?${q}`);
    const info = await page.evaluate(() => window.__web3d.ready);
    if (SOFTWARE.test(info.renderer)) throw new Error(`software renderer: ${info.renderer}`);
    const m = await page.evaluate(() => window.__web3d.measure(2000));
    await page.screenshot({ path: path.join(outDir, `demo-${name}.png`) });
    report[name] = { renderer: info.renderer, draws: m.draws, triangles: m.triangles, culled: m.culled, gpu: m.gpuMs, glError: await page.evaluate(() => window.__web3d.glError()), msgs };
    await ctx.close();
  }
  // 3. An application's per-frame path (plain-Array matrices through setCamera/setTransform): JS heap sampling.
  {
    const ctx = await browser.newContext({ viewport: { width: 1100, height: 650 }, deviceScaleFactor: 1 });
    const page = await ctx.newPage();
    await page.goto(`http://127.0.0.1:${port}/probe/game.html`);
    await page.waitForFunction(() => window.__game && window.__game.frames() > 5);
    const cdp = await ctx.newCDPSession(page);
    await cdp.send('HeapProfiler.enable');
    const sample = async (ms) => {
      await cdp.send('HeapProfiler.collectGarbage');
      await cdp.send('HeapProfiler.startSampling', { samplingInterval: 32, includeObjectsCollectedByMajorGC: true, includeObjectsCollectedByMinorGC: true });
      const f0 = await page.evaluate(() => window.__game.frames());
      await page.waitForTimeout(ms);
      const f1 = await page.evaluate(() => window.__game.frames());
      const { profile } = await cdp.send('HeapProfiler.stopSampling');
      const byFile = {}, byFn = {};
      const walk = (n) => { const { url, functionName, lineNumber } = n.callFrame; if (n.selfSize > 0) { const f = url ? new URL(url).pathname : '(native)'; byFile[f] = (byFile[f] ?? 0) + n.selfSize; if (url) { const k = `${f}:${lineNumber + 1} ${functionName}`; byFn[k] = (byFn[k] ?? 0) + n.selfSize; } } n.children.forEach(walk); };
      walk(profile.head);
      const frames = f1 - f0;
      return { frames, bytesPerFrameByFile: Object.fromEntries(Object.entries(byFile).map(([k, v]) => [k, Math.round(v / frames * 10) / 10])), topFunctions: Object.fromEntries(Object.entries(byFn).sort((a, b) => b[1] - a[1]).slice(0, 8)) };
    };
    const cold = await sample(8000);
    await page.waitForTimeout(40000);
    const steady = await sample(10000);
    report.gamePath = { renderer: await page.evaluate(() => window.__game.renderer), cold, steady };
    await ctx.close();
  }
} finally {
  await browser?.close();
  server.closeAllConnections?.();
  await new Promise((r) => server.close(r));
  console.log('browser and server stopped');
}
await writeFile(path.join(outDir, 'probe-report.json'), JSON.stringify(report, null, 2));
console.log(JSON.stringify(report, null, 1));
