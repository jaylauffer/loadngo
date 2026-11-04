// The sixteenth critic's copy of the sixteenth round's no-DevTools driver (../../critic15-rerun/tools/no-devtools.mjs):
// runs a page in headless Chrome with no DevTools client attached (Playwright's attachment disables
// prerendering: "PrerenderingDisabledByDevTools"). Serves web3d/ on <port>, starts Chrome on a page
// under ../../critic14/site with a new profile, waits for the page to POST its result, writes it to
// ../results/<out>.json, and stops Chrome and the server. node no-devtools.mjs <port> <page> <out>
import http from 'node:http';
import path from 'node:path';
import os from 'node:os';
import { spawn } from 'node:child_process';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { serve } from '../../critic13/tools/serve.mjs';
const here = path.dirname(fileURLToPath(import.meta.url));
const [port, page, out] = [Number(process.argv[2]), process.argv[3], process.argv[4]];
const server = await serve(port);
let resolveReport;
const report = new Promise((r) => (resolveReport = r));
const handler = server.listeners('request')[0];
server.removeAllListeners('request');
server.on('request', (req, res) => {
  if (req.method === 'POST' && req.url === '/report') {
    let body = '';
    req.on('data', (d) => (body += d));
    req.on('end', () => { res.writeHead(204).end(); resolveReport(body); });
    return;
  }
  handler(req, res);
});
const profile = await mkdtemp(path.join(os.tmpdir(), 'c16-chrome-'));
const chrome = spawn('/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', [
  '--headless=new', `--user-data-dir=${profile}`, '--no-first-run', '--no-default-browser-check', '--enable-gpu', '--ignore-gpu-blocklist',
  '--disable-background-networking', '--disable-component-update', '--password-store=basic', '--use-mock-keychain',
  `http://127.0.0.1:${port}/evidence/phase1/critic14/site/${page}`,
], { stdio: 'ignore' });
const timer = new Promise((r) => setTimeout(() => r(null), 40000));
const body = await Promise.race([report, timer]);
chrome.kill('SIGTERM');
await new Promise((r) => (chrome.exitCode !== null ? r() : chrome.once('exit', r)));
await rm(profile, { recursive: true, force: true });
server.closeAllConnections?.();
await new Promise((r) => server.close(r));
const result = body === null ? { timeout: 'no report in 40 s' } : JSON.parse(body);
await writeFile(path.join(here, '../results', `${out}.json`), JSON.stringify({ page, devtools: 'none attached', result }, null, 2) + '\n');
console.log(JSON.stringify(result, null, 1));
console.log('chrome and server stopped');
