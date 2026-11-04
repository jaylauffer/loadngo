// Critic 12 helpers for the attack pages in this folder. Generic: no game material.
export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
export const err = (e) => String(e?.message ?? e).slice(0, 140);
export const settle = (p) => p.then((r) => ({ ok: true, r }), (e) => ({ ok: false, message: err(e) }));
export const attempt = (f) => { try { return { ok: true, v: f() }; } catch (e) { return { ok: false, message: err(e) }; } };
export const IDENT = [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1];
export const OPTS = { width: 16, height: 16, antialias: false };
export async function until(f, ms = 5000) {
  const end = performance.now() + ms;
  while (performance.now() < end) {
    let v;
    try { v = f(); } catch { v = undefined; }
    if (v) return v;
    await sleep(10);
  }
  return undefined;
}
export async function frameOf(src) {
  const f = document.createElement('iframe');
  f.src = src;
  document.body.append(f);
  await new Promise((r) => f.addEventListener('load', r, { once: true }));
  await until(() => f.contentWindow.ready);
  return f;
}
/** Draws a green quad over a clear colour; returns 'green' or what it read at the centre. */
export function green(r, clear = [0, 0, 0]) {
  r.setCamera(IDENT, [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, -0.2, 0, 0, 0, -1.02, 1]);
  r.setKeyLight(0, 0, 1, 0, 0, 0, 0);
  r.setAmbient(1, 1, 1, 1, 1, 1, Math.PI);
  r.setClearColor(...clear);
  const mesh = r.createMesh({ positions: new Float32Array([-0.5, -0.5, -5, 0.5, -0.5, -5, 0.5, 0.5, -5, -0.5, 0.5, -5]), normals: new Float32Array([0, 0, 1, 0, 0, 1, 0, 0, 1, 0, 0, 1]), indices: new Uint16Array([0, 1, 2, 0, 2, 3]) });
  r.createObject(mesh, r.createMaterial({ baseColor: [0, 1, 0, 1] }));
  return redraw(r);
}
/** Draws a frame and reads the centre and a corner. */
export function redraw(r) {
  const drew = r.frame();
  const gl = r.gl;
  const px = (x, y) => { const p = new Uint8Array(4); gl.readPixels(x, y, 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, p); return [...p]; };
  const centre = px(gl.drawingBufferWidth >> 1, gl.drawingBufferHeight >> 1);
  const corner = px(0, 0);
  return { drew, centre, corner, isGreen: drew && centre[1] > 200 && centre[0] < 70 };
}
export const eventOf = (t, type, ms = 3000) => Promise.race([new Promise((res) => t.addEventListener(type, () => setTimeout(() => res(type), 0), { once: true })), sleep(ms).then(() => 'timeout')]);
/** One loss and restore on a renderer: whether the loss was cancelled, the restore came, and the hooks. */
export async function cycle(web3d, c, opts = OPTS) {
  const calls = { lost: 0, restored: 0 };
  const made = await settle(web3d.createRenderer(c, { ...opts, onContextLost: () => calls.lost++, onContextRestored: () => calls.restored++ }));
  if (!made.ok) return { create: made.message };
  const r = made.r;
  const drew = green(r).isGreen;
  const lc = r.gl.getExtension('WEBGL_lose_context');
  let prevented = null;
  c.addEventListener('webglcontextlost', (e) => (prevented = e.defaultPrevented), { once: true });
  const lost = eventOf(c, 'webglcontextlost');
  lc.loseContext();
  const lostEvent = await lost;
  r.dispose();
  const restored = eventOf(c, 'webglcontextrestored');
  let restoreThrew;
  try { lc.restoreContext(); } catch (e) { restoreThrew = err(e); }
  const ev = await restored;
  await sleep(20);
  const next = await settle(web3d.createRenderer(c, opts));
  const after = next.ok ? green(next.r).isGreen : next.message;
  if (next.ok) next.r.dispose();
  const ok = drew && lostEvent === 'webglcontextlost' && prevented === true && ev === 'webglcontextrestored' && calls.lost === 1 && calls.restored === 1 && after === true;
  return { drew, lostEvent, prevented, restoreThrew, restored: ev, calls, after, ok };
}
/** Runs a page's body; whatever it returns, or the error it throws, becomes window.__done. */
export function page(body) {
  const out = {};
  window.__out = out;
  window.__done = undefined;
  body(out).then(
    () => { window.__done = Promise.resolve(out); },
    (e) => { out.pageError = `${err(e)} | ${String(e?.stack ?? '').slice(0, 600)}`; window.__done = Promise.resolve(out); },
  );
}
/** Listener counts over DevTools (the driver's window.__listeners), or 'no driver'. */
let n = 0;
export async function listenersOf(object) {
  if (typeof window.__listeners !== 'function') return 'no driver';
  window.__targets ??= {};
  const name = `c12t${++n}`;
  window.__targets[name] = object;
  try { return await window.__listeners(`@${name}`); } finally { delete window.__targets[name]; }
}
