// node leaf-diff.mjs a.json b.json: every string or null leaf (by path) that differs between two
// results, and paths only in one (booleans are bool-diff.mjs's; numbers are left out, as timings).
// The fifteenth critic's. BSD-3-Clause; see the repository's LICENSE.
import { readFileSync } from 'node:fs';
const leaves = (f) => { const out = new Map(); const walk = (v, p) => { if (typeof v === 'string' || v === null) out.set(p, v); else if (v && typeof v === 'object') for (const [k, x] of Object.entries(v)) walk(x, `${p}/${k}`); }; walk(JSON.parse(readFileSync(f, 'utf8')).result ?? {}, ''); return out; };
const [a, b] = [leaves(process.argv[2]), leaves(process.argv[3])];
const diffs = [];
for (const [p, v] of a) if (b.has(p) && b.get(p) !== v) diffs.push(`${p}: ${JSON.stringify(v).slice(0, 90)} -> ${JSON.stringify(b.get(p)).slice(0, 90)}`);
for (const p of a.keys()) if (!b.has(p)) diffs.push(`${p}: only in the first`);
for (const p of b.keys()) if (!a.has(p)) diffs.push(`${p}: only in the second`);
console.log(`${a.size} string leaves; ${diffs.length} differ${diffs.length ? '\n  ' + diffs.join('\n  ') : ''}`);
