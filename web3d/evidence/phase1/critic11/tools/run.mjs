// Critic 11's driver: node run.mjs <port> <page.html> [--host h] [--args "chrome flags"] [--out name]
// Opens /evidence/phase1/critic11/site/<page> in its own headless Chrome, waits for
// window.__done (a promise of a JSON-able result), writes results/<out>.json, stops both.
import path from 'node:path';
import { writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { serve } from './serve.mjs';
const here = path.dirname(fileURLToPath(import.meta.url));
const argv = process.argv.slice(2);
const opt = (k, d) => { const i = argv.indexOf(`--${k}`); return i >= 0 ? argv[i + 1] : d; };
const port = Number(argv[0]);
const page = argv[1];
const host = opt('host', '127.0.0.1');
const extra = (opt('args', '') || '').split(' ').filter(Boolean);
const out = opt('out', page.replace(/\.html.*$/, ''));
const { chromium } = await import(process.env.PLAYWRIGHT_CORE);
const server = await serve(port);
let browser;
let code = 0;
try {
  browser = await chromium.launch({ channel: 'chrome', headless: true, args: ['--enable-gpu', '--ignore-gpu-blocklist', ...extra] });
  const context = await browser.newContext({ viewport: { width: 800, height: 600 } });
  const p = await context.newPage();
  const messages = [];
  context.on('page', (np) => {
    np.on('console', (m) => messages.push(`popup ${m.type()}: ${m.text()}`));
    np.on('pageerror', (e) => messages.push(`popup pageerror: ${e.message}`));
  });
  p.on('console', (m) => messages.push(`${m.type()}: ${m.text()}`));
  p.on('pageerror', (e) => messages.push(`pageerror: ${e.message}`));
  await p.goto(`http://${host}:${port}/evidence/phase1/critic11/site/${page}`);
  await p.waitForFunction(() => window.__done !== undefined, null, { timeout: 170000 });
  const result = await p.evaluate(() => window.__done);
  const report = { page, host, chrome: browser.version(), args: extra, result, messages };
  await writeFile(path.join(here, '../results', `${out}.json`), JSON.stringify(report, null, 2) + '\n');
  console.log(JSON.stringify(result, null, 1).slice(0, 6000));
  console.log(`messages: ${messages.length}`);
  for (const m of messages.slice(0, 30)) console.log('  ' + m.slice(0, 300));
} catch (e) {
  console.error('driver error:', e);
  code = 1;
} finally {
  await browser?.close();
  server.closeAllConnections?.();
  await new Promise((r) => server.close(r));
  console.log('browser and server stopped');
}
process.exit(code);
