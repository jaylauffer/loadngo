// A worker with its own copy of web3d.js: an OffscreenCanvas transferred from the page, lost and restored.
import { createRenderer } from '/web3d/pkg/web3d.js';
const IDENT = new Float32Array([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1]);
const ORTHO = new Float32Array([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, -2 / 9.9, 0, 0, 0, -10.1 / 9.9, 1]);
const quad = { positions: new Float32Array([-1, -1, -5, 1, -1, -5, 1, 1, -5, -1, 1, -5]), normals: new Float32Array([0, 0, 1, 0, 0, 1, 0, 0, 1, 0, 0, 1]), indices: new Uint32Array([0, 1, 2, 0, 2, 3]) };
const event = (t, type) => new Promise((res) => t.addEventListener(type, () => setTimeout(() => res(type), 0), { once: true }));
const timeout = (ms) => new Promise((res) => setTimeout(() => res('timeout'), ms));
const draw = (r) => {
  r.setCamera(IDENT, ORTHO);
  r.setKeyLight(0, 0, 1, 0, 0, 0, 0);
  r.setAmbient(1, 1, 1, 1, 1, 1, Math.PI);
  r.setClearColor(0, 0, 0);
  r.createObject(r.createMesh(quad), r.createMaterial({ baseColor: [0, 1, 0, 1] }));
  r.frame();
  const px = new Uint8Array(4);
  r.gl.readPixels(8, 8, 1, 1, r.gl.RGBA, r.gl.UNSIGNED_BYTE, px);
  return [...px.slice(0, 3)];
};
onmessage = async ({ data: { canvas } }) => {
  const d = {};
  try {
    const calls = { lost: 0, restored: 0 };
    const r = await createRenderer(canvas, { width: 16, height: 16, antialias: false, onContextLost: () => calls.lost++, onContextRestored: () => calls.restored++ });
    d.first = draw(r);
    const lc = r.gl.getExtension('WEBGL_lose_context');
    const lost = event(canvas, 'webglcontextlost');
    lc.loseContext();
    await lost;
    r.dispose();
    const restored = event(canvas, 'webglcontextrestored');
    lc.restoreContext();
    d.restored = await Promise.race([restored, timeout(2000)]);
    await timeout(20);
    d.calls = calls;
    const next = await createRenderer(canvas, { width: 16, height: 16, antialias: false });
    d.next = draw(next);
    next.dispose();
  } catch (e) {
    d.error = String(e?.stack ?? e);
  }
  postMessage(d);
};
