// Fourth critic's driver: its own server on a strict port, its own headless Chrome (channel 'chrome',
// default Metal backend) on the Mac's GPU. Runs probe-site/ (attacks on the boundary checks), then
// tests the rendered check's allocation gate: whether Chrome refuses an unknown sampling parameter,
// and what the gate reads on the demo with CPU timing on (a known per-frame allocation) when the
// "objects already collected" flags are given and when they are not.
//   PLAYWRIGHT_CORE=.../playwright-core/index.mjs node run4.mjs <port> <out.json> [warm seconds]
import http from 'node:http';
import path from 'node:path';
import { readFile, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
const { chromium } = await import(process.env.PLAYWRIGHT_CORE ?? 'playwright-core');

const port = Number(process.argv[2]);
const outFile = path.resolve(process.argv[3]);
const warm = Number(process.argv[4] ?? 40);
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

function pageBytes(profile) {
  let bytes = 0;
  const walk = (n) => {
    if (n.selfSize > 0 && n.callFrame.url && new URL(n.callFrame.url).pathname.startsWith('/web3d/')) bytes += n.selfSize;
    n.children.forEach(walk);
  };
  walk(profile.head);
  return bytes;
}

const result = {};
let browser;
try {
  browser = await chromium.launch({ channel: 'chrome', headless: true, args: ['--enable-gpu', '--ignore-gpu-blocklist'] });
  result.browser = browser.version();
  {
    const page = await browser.newPage();
    const messages = [];
    page.on('console', (m) => messages.push(`${m.type()}: ${m.text()}`));
    page.on('pageerror', (e) => messages.push(`pageerror: ${e.message}`));
    await page.goto(`http://127.0.0.1:${port}/c/`);
    result.probes = await page.evaluate(() => window.__p4);
    result.probes.renderer = await page.evaluate(() => {
      const gl = document.createElement('canvas').getContext('webgl2');
      const d = gl.getExtension('WEBGL_debug_renderer_info');
      return gl.getParameter(d.UNMASKED_RENDERER_WEBGL);
    });
    result.probes.messages = messages;
    for (const t of result.probes.tests) console.log(`${t.pass ? 'pass' : 'FAIL'}  ${t.name}`);
    await page.close();
  }
  {
    const context = await browser.newContext({ viewport: { width: 1366, height: 768 }, deviceScaleFactor: 1 });
    const page = await context.newPage();
    await page.goto(`http://127.0.0.1:${port}/web3d/demo/?scale=0.75&cputime=1&hud=0`);
    await page.evaluate(() => window.__web3d.ready);
    const cdp = await context.newCDPSession(page);
    await cdp.send('HeapProfiler.enable');
    const tryStart = async (params) => {
      try {
        await cdp.send('HeapProfiler.startSampling', params);
        await cdp.send('HeapProfiler.stopSampling');
        return 'accepted';
      } catch (e) {
        return `refused: ${e.message}`;
      }
    };
    result.unknownParameter = await tryStart({ samplingInterval: 32, includeObjectsCollectedByAMadeUpGC: true });
    result.wrongTypeForARealFlag = await tryStart({ samplingInterval: 32, includeObjectsCollectedByMajorGC: 'yes' });
    console.log(`unknown parameter: ${result.unknownParameter}; wrong type for a real flag: ${result.wrongTypeForARealFlag}`);
    await page.evaluate((ms) => window.__web3d.frames(ms), warm * 1000);
    const sample = async (label, flags) => {
      await cdp.send('HeapProfiler.collectGarbage');
      await cdp.send('HeapProfiler.startSampling', flags ? { samplingInterval: 32, includeObjectsCollectedByMajorGC: true, includeObjectsCollectedByMinorGC: true } : { samplingInterval: 32 });
      const frames = await page.evaluate((ms) => window.__web3d.frames(ms), 10000);
      const { profile } = await cdp.send('HeapProfiler.stopSampling');
      const perFrame = Math.round((pageBytes(profile) / frames) * 100) / 100;
      console.log(`cputime=1, ${label}: ${frames} frames, page scripts ${perFrame} sampled bytes a frame (gate limit 1)`);
      return { label, flags, frames, pageScriptBytesPerFrame: perFrame };
    };
    result.cpuTimingOn = [await sample('with the flags', true), await sample('without the flags', false), await sample('with the flags again', true)];
    await context.close();
  }
} finally {
  await browser?.close();
  server.closeAllConnections?.();
  await new Promise((ok) => server.close(ok));
  console.log('browser and server stopped');
}
await writeFile(outFile, `${JSON.stringify(result, null, 2)}\n`);
