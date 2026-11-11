// The browser benchmark: loads pkg/ and a database (db.lmm, served beside this page by
// serve.py), drives characters along scripted paths at 60 steps a second as fast as it can,
// times each step, and posts the numbers to /result. Cross-origin isolated (serve.py sends
// COOP/COEP), performance.now() resolves to microseconds.
import { loadMotion } from './pkg/motion.js';

const out = document.getElementById('out');
const log = (s) => { out.textContent += `\n${s}`; };

const pct = (xs, p) => {
  const s = Float64Array.from(xs).sort();
  return s.length ? s[Math.min(s.length - 1, Math.floor(s.length * p))] : 0;
};

const scenarios = [
  { name: 'straight', path: [0, 0, 0, 12], speed: 0.83, seconds: 20 },
  { name: 'turn-left-90', path: [0, 0, 0, 6, 6, 6], speed: 0.83, seconds: 19 },
  { name: 'curve', path: (() => { const p = [0, 0, 0, 2]; for (let i = 1; i <= 24; i++) { const a = Math.PI - (Math.PI / 2) * (i / 24); p.push(5 + 5 * Math.cos(a), 2 + 5 * Math.sin(a)); } return p; })(), speed: 0.83, seconds: 18 },
  { name: 'alongside-slow', path: [0, 0, 0, 12], speed: 0.6, seconds: 25 },
];

async function main() {
  const t0 = performance.now();
  const motion = await loadMotion(new URL('./pkg/loadngo_animation.wasm', import.meta.url));
  const tCompiled = performance.now();
  const bytes = await (await fetch('db.lmm')).arrayBuffer();
  const tFetched = performance.now();
  const db = motion.loadDatabase(bytes);
  const tLoaded = performance.now();
  log(`isolated ${crossOriginIsolated}; compile ${(tCompiled - t0).toFixed(1)} ms; fetch ${(tFetched - tCompiled).toFixed(1)} ms; database load (hash check, ${db.frameCount} frames) ${(tLoaded - tFetched).toFixed(1)} ms`);
  const result = {
    userAgent: navigator.userAgent, crossOriginIsolated, hardwareConcurrency: navigator.hardwareConcurrency,
    databaseBytes: bytes.byteLength, frames: db.frameCount, loadMs: tLoaded - tFetched, compileMs: tCompiled - t0, scenarios: [],
  };
  const all = [];
  for (const s of scenarios) {
    const who = db.createCharacter(0, 0, 0);
    who.setPath(s.path, s.speed);
    const n = Math.round(s.seconds * 60);
    const us = new Float64Array(n);
    for (let i = 0; i < n; i++) {
      const a = performance.now();
      who.step(1 / 60);
      us[i] = (performance.now() - a) * 1000;
    }
    const pose = who.view();
    const end = Math.hypot(pose[0] - s.path[s.path.length - 2], pose[1] - s.path[s.path.length - 1]);
    const r = { name: s.name, steps: n, transitions: who.transitions, endOffM: end, us: { p50: pct(us, 0.5), p99: pct(us, 0.99), max: pct(us, 1), mean: us.reduce((x, y) => x + y, 0) / n } };
    result.scenarios.push(r);
    all.push(...us);
    log(`${s.name.padEnd(15)} step us p50 ${r.us.p50.toFixed(1)} p99 ${r.us.p99.toFixed(1)} max ${r.us.max.toFixed(1)}; ${r.transitions} transitions; ends ${end.toFixed(2)} m off`);
    who.dispose();
  }
  // A crowd: 16 characters stepping together, per 60 Hz frame.
  const crowd = Array.from({ length: 16 }, (_, k) => { const c = db.createCharacter(k * 2, 0, 0); c.setPath([k * 2, 0, k * 2, 15], 0.7 + 0.02 * k); return c; });
  const frames = new Float64Array(600);
  for (let f = 0; f < frames.length; f++) {
    const a = performance.now();
    for (const c of crowd) c.step(1 / 60);
    frames[f] = (performance.now() - a) * 1000;
  }
  crowd.forEach((c) => c.dispose());
  result.crowd16 = { p50: pct(frames, 0.5), p99: pct(frames, 0.99), max: pct(frames, 1) };
  result.all = { p50: pct(all, 0.5), p99: pct(all, 0.99), max: pct(all, 1) };
  log(`all steps us p50 ${result.all.p50.toFixed(1)} p99 ${result.all.p99.toFixed(1)}; 16 characters a frame us p50 ${result.crowd16.p50.toFixed(1)} p99 ${result.crowd16.p99.toFixed(1)}`);
  await fetch('/result', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(result) });
  document.title = 'done';
}

main().catch(async (e) => {
  log(`error: ${e?.stack ?? e}`);
  await fetch('/result', { method: 'POST', body: JSON.stringify({ error: String(e?.stack ?? e) }) });
});
