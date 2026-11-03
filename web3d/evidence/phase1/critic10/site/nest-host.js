// Tenth critic: under a cross-origin top, does a same-origin child frame's copy share this frame's
// state (owners, contexts, controllers, serials)? docs/WEB3D.md: "One renderer per canvas: a second
// is refused while the first is live"; "Every copy of the module in a window and its same-origin
// frames shares one counter".
import { createRenderer } from '/web3d/pkg/web3d.js';
const out = {};
const timeout = (ms) => new Promise((res) => setTimeout(() => res('timeout'), ms));
const settle = (p) => p.then((r) => ({ ok: true, r }), (e) => ({ ok: false, message: String(e?.message ?? e) }));
const N = 0.1, F = 10;
const ORTHO = new Float32Array([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, -2 / (F - N), 0, 0, 0, -(F + N) / (F - N), 1]);
const IDENT = new Float32Array([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1]);
const quad = (z) => ({
  positions: new Float32Array([-1, -1, z, 1, -1, z, 1, 1, z, -1, 1, z]),
  normals: new Float32Array([0, 0, 1, 0, 0, 1, 0, 0, 1, 0, 0, 1]),
  uvs: new Float32Array([0, 1, 1, 1, 1, 0, 0, 0]),
  indices: new Uint32Array([0, 1, 2, 0, 2, 3]),
});
const setup = (r, color) => {
  r.setCamera(IDENT, ORTHO);
  r.setKeyLight(0, 0, 1, 0, 0, 0, 0);
  r.setAmbient(1, 1, 1, 1, 1, 1, Math.PI);
  r.setClearColor(0, 0, 0);
  r.createObject(r.createMesh(quad(-5)), r.createMaterial({ baseColor: color }));
};
const read = (gl) => {
  const px = new Uint8Array(4);
  gl.readPixels(8, 8, 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, px);
  return [px[0], px[1], px[2]];
};
async function run() {
  const child = document.getElementById('child');
  if (child.contentDocument?.readyState !== 'complete' || !child.contentWindow.__frame) {
    for (let i = 0; i < 100 && !child.contentWindow.__frame; i++) await timeout(20);
  }
  const fc = child.contentWindow.__frame;
  const canvas = document.getElementById('host');
  out.topCrossOrigin = (() => { try { void top.document; return false; } catch { return true; } })();
  const mine = globalThis[Symbol.for('loadngo-web3d')];
  out.sharedStateSame = mine === fc.state();
  const a = await createRenderer(canvas, { width: 16, height: 16, antialias: false });
  setup(a, [0, 1, 0, 1]);
  a.frame();
  out.aFirst = read(a.gl);
  const b = await settle(fc.createRenderer(canvas, { width: 16, height: 16, antialias: false }));
  out.secondCreate = b.ok ? 'created: two live renderers on one canvas' : b.message;
  out.sharedStateAfter = mine === fc.state();
  if (b.ok) {
    out.serials = { a: a._serial, b: b.r._serial };
    setup(b.r, [1, 0, 0, 1]);
    b.r.frame();
    out.bDrew = read(a.gl);
    a.frame();
    out.aAgain = read(a.gl);
    out.aAgainError = a.gl.getError();
    b.r.dispose();
  }
  a.dispose();
  return out;
}
window.__probe10 = { done: run().catch((e) => ({ error: String(e?.stack ?? e), partial: out })) };
