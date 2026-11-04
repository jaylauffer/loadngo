// Critic 14: why a speculation-rules prerender did or did not happen in headless Chrome. Serves
// web3d/ on <port>, opens critic14/site/c2-prerender.html, logs the DevTools Preload domain's
// events and the page's result, then stops both. node prerender-status.mjs <port> [chrome flags...]
import { serve } from '../../critic13/tools/serve.mjs';
const port = Number(process.argv[2]);
const extra = process.argv.slice(3);
const { chromium } = await import(process.env.PLAYWRIGHT_CORE);
const server = await serve(port);
const browser = await chromium.launch({ channel: 'chrome', headless: true, args: ['--enable-gpu', '--ignore-gpu-blocklist', ...extra] });
try {
  const context = await browser.newContext();
  const p = await context.newPage();
  const cdp = await context.newCDPSession(p);
  const events = [];
  for (const e of ['Preload.ruleSetUpdated', 'Preload.prerenderStatusUpdated', 'Preload.preloadingAttemptSourcesUpdated', 'Preload.preloadEnabledStateUpdated', 'Preload.prefetchStatusUpdated'])
    cdp.on(e, (d) => events.push({ e, d: JSON.stringify(d).slice(0, 400) }));
  await cdp.send('Preload.enable');
  await p.goto(`http://127.0.0.1:${port}/evidence/phase1/critic14/site/c2-prerender.html`);
  await p.waitForFunction(() => window.__done !== undefined, null, { timeout: 30000 });
  const result = await p.evaluate(() => window.__done);
  console.log(JSON.stringify({ chrome: browser.version(), extra, result, events }, null, 1));
} finally {
  await browser.close();
  server.closeAllConnections?.();
  await new Promise((r) => server.close(r));
  console.log('browser and server stopped');
}
