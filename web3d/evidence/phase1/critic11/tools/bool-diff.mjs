// node bool-diff.mjs a.json b.json: every boolean leaf (by path) that differs between two results.
import { readFileSync } from 'node:fs';
const leaves = (f) => { const out = new Map(); const walk = (v, p) => { if (typeof v === 'boolean') out.set(p, v); else if (v && typeof v === 'object') for (const [k, x] of Object.entries(v)) walk(x, `${p}/${/^\d+$/.test(k) && Array.isArray(v) && typeof x === 'object' && x && (x.name || x.id) ? (x.name || x.id).slice(0, 50) : k}`); }; walk(JSON.parse(readFileSync(f, 'utf8')), ''); return out; };
const [a, b] = [leaves(process.argv[2]), leaves(process.argv[3])];
const diffs = [];
for (const [p, v] of a) if (b.has(p) && b.get(p) !== v) diffs.push(`${p}: ${v} -> ${b.get(p)}`);
for (const p of a.keys()) if (!b.has(p)) diffs.push(`${p}: only in the first`);
for (const p of b.keys()) if (!a.has(p)) diffs.push(`${p}: only in the second`);
console.log(`${a.size} booleans; ${diffs.length} differ${diffs.length ? '\n  ' + diffs.slice(0, 12).join('\n  ') : ''}`);
