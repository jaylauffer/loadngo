// Serves a pkg/ directory at /pkg/ and site/ at /, on a strict port, opens a page in its own headless
// Google Chrome (channel 'chrome', the GPU), prints window.__done line by line, and stops both.
//   PLAYWRIGHT_CORE=.../playwright-core/index.mjs node run.mjs <port> <pkg dir> <site dir> <page>
import http from 'node:http'; import path from 'node:path'; import { readFile } from 'node:fs/promises';
const { chromium } = await import(process.env.PLAYWRIGHT_CORE);
const [port, pkg, site, pageName] = [Number(process.argv[2]), process.argv[3], process.argv[4], process.argv[5]];
const T = { '.html': 'text/html', '.js': 'text/javascript', '.wasm': 'application/wasm' };
const server = http.createServer(async (req, res) => { const p = decodeURIComponent(new URL(req.url, 'http://x').pathname); const [root, rel] = p.startsWith('/pkg/') ? [pkg, p.slice(5)] : [site, p.slice(1)]; let body; try { body = await readFile(path.join(root, rel)); } catch { return res.writeHead(404).end(); } res.writeHead(200, { "Content-Type": T[path.extname(rel)] ?? "application/octet-stream" }).end(body); });
await new Promise((ok) => server.listen(port, '127.0.0.1', ok));
const browser = await chromium.launch({ channel: 'chrome', headless: true, args: ['--enable-gpu', '--ignore-gpu-blocklist'] });
try { const page = await browser.newPage(); const msgs = []; page.on('console', (m) => msgs.push(m.text())); await page.goto(`http://127.0.0.1:${port}/${pageName}`); const out = await page.waitForFunction(() => window.__done, null, { timeout: 60000 }).then((h) => h.jsonValue()); for (const o of out) console.log(JSON.stringify(o)); console.log(msgs.filter((m) => !/Too many/.test(m)).slice(0, 8).join('\n')); }
finally { await browser.close(); server.close(); }
