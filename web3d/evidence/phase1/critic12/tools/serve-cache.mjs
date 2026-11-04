// Critic 12: critic 11's static server without Cache-Control: no-store (which keeps a page out of the back-forward cache). Was: critic 11's static server: serves web3d/ (so /pkg/ and /evidence/phase1/critic11/site/ are
// reachable) on 127.0.0.1:<port>, strict port. No COOP/COEP, so popups keep their opener; sends
// Origin-Agent-Cluster: ?0 so a page may set document.domain. Any Host header is served, so
// localhost, a.localhost and b.a.localhost are further origins on the same port.
import http from 'node:http';
import path from 'node:path';
import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../../..');
const TYPES = { '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8', '.mjs': 'text/javascript; charset=utf-8', '.wasm': 'application/wasm', '.json': 'application/json' };
export function serve(port) {
  const server = http.createServer(async (req, res) => {
    let p = decodeURIComponent(new URL(req.url, 'http://x').pathname);
    if (p.endsWith('/')) p += 'index.html';
    const file = path.join(root, path.normalize(p));
    if (!file.startsWith(root + path.sep)) return res.writeHead(403).end();
    try {
      const body = await readFile(file);
      res.writeHead(200, { 'Content-Type': TYPES[path.extname(file)] ?? 'application/octet-stream', 'Origin-Agent-Cluster': '?0' });
      res.end(body);
    } catch {
      res.writeHead(404).end();
    }
  });
  return new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(port, '127.0.0.1', () => resolve(server));
  });
}
