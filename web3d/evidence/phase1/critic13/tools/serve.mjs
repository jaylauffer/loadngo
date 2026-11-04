// Critic 13's static server: the eleventh critic's (../../critic11/tools/serve.mjs) with two more
// paths. Serves web3d/ on 127.0.0.1:<port>, strict port, no COOP/COEP, Origin-Agent-Cluster: ?0, any
// Host header. /slow.js?ms=N&write=1 answers after N ms with a script that, when write=1, calls
// document.write() (a parser-inserted script, so the write does not reopen the document).
// BSD-3-Clause; see the repository's LICENSE.
import http from 'node:http';
import path from 'node:path';
import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../../..');
const TYPES = { '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8', '.mjs': 'text/javascript; charset=utf-8', '.wasm': 'application/wasm', '.json': 'application/json' };
export function serve(port) {
  const server = http.createServer(async (req, res) => {
    const u = new URL(req.url, 'http://x');
    let p = decodeURIComponent(u.pathname);
    if (p === '/slow.js') {
      const ms = Math.min(10000, Number(u.searchParams.get('ms') ?? 1000));
      const body = u.searchParams.get('write') === '1' ? "document.write('<p id=\"written\">written while parsing</p>'); window.__wroteWhileParsing = true;" : 'window.__slowRan = true;';
      setTimeout(() => res.writeHead(200, { 'Content-Type': 'text/javascript', 'Cache-Control': 'no-store', 'Origin-Agent-Cluster': '?0' }).end(body), ms);
      return;
    }
    if (p.endsWith('/')) p += 'index.html';
    const file = path.join(root, path.normalize(p));
    if (!file.startsWith(root + path.sep)) return res.writeHead(403).end();
    try {
      const body = await readFile(file);
      res.writeHead(200, { 'Content-Type': TYPES[path.extname(file)] ?? 'application/octet-stream', 'Cache-Control': 'no-store', 'Origin-Agent-Cluster': '?0' });
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
