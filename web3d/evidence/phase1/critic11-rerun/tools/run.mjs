// The twelfth round's page driver, after the eleventh critic's (../../critic11/tools/run.mjs), with
// its pages and its results here: node run.mjs <port> <page.html> [--site <dir under web3d/>]
// [--host h] [--args "chrome flags"] [--out name]. Serves web3d/ with ../../critic11/tools/serve.mjs
// (strict port, Origin-Agent-Cluster: ?0, any Host), opens <site>/<page> (default this round's
// site/) in its own headless Chrome on the GPU, gives the page window.__listeners as check.mjs gives
// the probe page, waits for window.__done, writes results/<out>.json, and stops both. BSD-3-Clause; see the repository's LICENSE.
import path from 'node:path';
import { writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { serve } from '../../critic11/tools/serve.mjs';
const here = path.dirname(fileURLToPath(import.meta.url));
const argv = process.argv.slice(2);
const opt = (k, d) => { const i = argv.indexOf(`--${k}`); return i >= 0 ? argv[i + 1] : d; };
const port = Number(argv[0]);
const page = argv[1];
const site = opt('site', 'evidence/phase1/critic11-rerun/site');
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
    if (np === p) return;
    np.on('console', (m) => messages.push(`popup ${m.type()}: ${m.text()}`));
    np.on('pageerror', (e) => messages.push(`popup pageerror: ${e.message}`));
  });
  p.on('console', (m) => messages.push(`${m.type()}: ${m.text()}`));
  p.on('pageerror', (e) => messages.push(`pageerror: ${e.message}`));
  // window.__listeners(id), as check.mjs gives the probe page: an object's event listeners counted
  // by type over the DevTools protocol; '@name' is the top window's __targets[name], else an element
  // id. DevTools lists an object's listeners only when the object is resolved in its own frame's
  // context (a node of a frame's document, or an OffscreenCanvas a frame made, reads as having none
  // from the top's), so the object is resolved in every frame's main context, and the longest list
  // is the object's.
  const cdp = await context.newCDPSession(p);
  const frames = new Map();
  cdp.on('Runtime.executionContextCreated', ({ context: c }) => { if (c.auxData?.isDefault) frames.set(c.id, c); });
  cdp.on('Runtime.executionContextDestroyed', ({ executionContextId }) => frames.delete(executionContextId));
  cdp.on('Runtime.executionContextsCleared', () => frames.clear());
  await cdp.send('Runtime.enable');
  await p.exposeFunction('__listeners', async (id) => {
    const key = String(id);
    const expression = key.startsWith('@') ? `globalThis.top.__targets[${JSON.stringify(key.slice(1))}]` : `globalThis.top.document.getElementById(${JSON.stringify(key)})`;
    let best = null;
    for (const contextId of frames.keys()) {
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
  await p.goto(`http://${host}:${port}/${site}/${page}`);
  await p.waitForFunction(() => window.__done !== undefined, null, { timeout: 170000 });
  const result = await p.evaluate(() => window.__done);
  const report = { page: `${site}/${page}`, host, chrome: browser.version(), args: extra, result, messages };
  await writeFile(path.join(here, '../results', `${out}.json`), JSON.stringify(report, null, 2) + '\n');
  console.log(JSON.stringify(result, null, 1).slice(0, 8000));
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
