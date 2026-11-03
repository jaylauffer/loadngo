// Critic 11 helpers shared by the probe pages.
export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
export async function until(f, ms = 5000) {
  const end = performance.now() + ms;
  while (performance.now() < end) { const v = f(); if (v) return v; await sleep(10); }
  throw new Error('timed out');
}
export async function frame(src, attrs = {}) {
  const f = document.createElement('iframe');
  for (const [k, v] of Object.entries(attrs)) f.setAttribute(k, v);
  if (src) f.src = src;
  document.body.appendChild(f);
  if (src || attrs.srcdoc) await new Promise((r) => (f.onload = r));
  return f;
}
export async function loadInto(win) {
  // Loads web3d into a window that has no module of its own (about:blank).
  const s = win.document.createElement('script');
  s.type = 'module';
  s.textContent = `import * as m from '${new URL('/pkg/web3d.js', location.href).href}'; window.web3d = m; window.ready = true;`;
  win.document.body.appendChild(s);
  await until(() => win.ready);
}
export const err = (e) => String(e?.message ?? e).slice(0, 160);
export async function tryCreate(web3d, canvas, opts = {}) {
  try {
    const r = await web3d.createRenderer(canvas, { width: 64, height: 64, antialias: false, ...opts });
    return { ok: true, r };
  } catch (e) {
    return { ok: false, error: err(e) };
  }
}
export const IDENT = [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1];
/** A quad over the whole view, drawn once; returns the centre pixel and a pixel off the diagonal. */
export function drawQuad(r, colour = [0, 1, 0]) {
  const mesh = r.createMesh({
    positions: new Float32Array([-1, -1, 0, 1, -1, 0, 1, 1, 0, -1, 1, 0]),
    normals: new Float32Array([0, 0, 1, 0, 0, 1, 0, 0, 1, 0, 0, 1]),
    indices: new Uint16Array([0, 1, 2, 0, 2, 3]),
  });
  const mat = r.createMaterial({ baseColor: colour });
  const obj = r.createObject(mesh, mat, IDENT);
  r.setCamera(IDENT, [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, -0.5, 0, 0, 0, 0, 1]);
  r.setKeyLight(0, 0, 1, 1, 1, 1, 3);
  r.setClearColor(0, 0, 0);
  const drew = r.frame();
  const gl = r.gl;
  const w = gl.drawingBufferWidth, h = gl.drawingBufferHeight;
  const px = (x, y) => { const p = new Uint8Array(4); gl.readPixels(x, y, 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, p); return [...p]; };
  return { drew, size: [w, h], centre: px(w >> 1, h >> 1), offDiagonal: px(Math.floor(w * 0.8), Math.floor(h * 0.3)), error: gl.getError(), obj };
}
