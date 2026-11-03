import http from 'node:http'; import path from 'node:path'; import { readFile } from 'node:fs/promises';
// playwright-core from PLAYWRIGHT_CORE (a path to its index.mjs), else from node_modules.
const { chromium } = await import(process.env.PLAYWRIGHT_CORE ?? 'playwright-core');
const port = Number(process.argv[2]);
const ROOTS = { '/web3d/': path.resolve(path.dirname(new URL(import.meta.url).pathname), '../../..'), '/probe/': path.join(path.dirname(new URL(import.meta.url).pathname), 'probe-site') };
const TYPES = { '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8', '.wasm': 'application/wasm' };
const server = http.createServer(async (req, res) => {
  const p = decodeURIComponent(new URL(req.url, 'http://x').pathname); const k = Object.keys(ROOTS).find((x) => p.startsWith(x));
  if (!k) return res.writeHead(404).end(); const root = ROOTS[k]; const file = path.join(root, path.normalize(p.slice(k.length) || 'index.html'));
  if (!file.startsWith(root + path.sep)) return res.writeHead(403).end();
  try { const b = await readFile(file); res.writeHead(200, { 'Content-Type': TYPES[path.extname(file)] ?? 'application/octet-stream' }); res.end(b); } catch { res.writeHead(404).end(); }
});
await new Promise((ok, no) => { server.once('error', no); server.listen(port, '127.0.0.1', ok); });
let browser;
try {
  browser = await chromium.launch({ channel: 'chrome', headless: true, args: ['--enable-gpu', '--ignore-gpu-blocklist'] });
  const page = await browser.newPage(); const msgs = [];
  page.on('console', (m) => msgs.push(`${m.type()}: ${m.text()}`)); page.on('pageerror', (e) => msgs.push(`pageerror: ${e.message}`));
  await page.goto(`http://127.0.0.1:${port}/probe/loss.html`);
  await page.waitForFunction(() => window.__loss, null, { timeout: 15000 });
  const renderer = await page.evaluate(() => { const g = document.createElement('canvas').getContext('webgl2'); const d = g.getExtension('WEBGL_debug_renderer_info'); return g.getParameter(d.UNMASKED_RENDERER_WEBGL); });
  console.log(JSON.stringify({ renderer, result: await page.evaluate(() => window.__loss), warnings: msgs.filter((m) => m.startsWith('warning')).length, sample: msgs.slice(0, 4) }, null, 1));
} finally { await browser?.close(); server.closeAllConnections?.(); await new Promise((r) => server.close(r)); console.log('browser and server stopped'); }
