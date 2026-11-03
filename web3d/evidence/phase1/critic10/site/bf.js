// Tenth critic: a renderer across the back/forward cache. The driver loads this page, navigates
// away and back; if the page came back from the cache (pageshow persisted), the renderer, its
// listeners and its hooks should still work.
import { createRenderer } from '/web3d/pkg/web3d.js';
const N = 0.1, F = 10;
const ORTHO = new Float32Array([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, -2 / (F - N), 0, 0, 0, -(F + N) / (F - N), 1]);
const IDENT = new Float32Array([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1]);
const timeout = (ms) => new Promise((res) => setTimeout(() => res('timeout'), ms));
const eventOf = (t, type) => new Promise((res) => t.addEventListener(type, () => setTimeout(() => res(type), 0), { once: true }));
const bf = { shows: [], calls: { lost: 0, restored: 0 } };
window.__bf = bf;
addEventListener('pageshow', (e) => bf.shows.push(e.persisted));
const read = (gl) => {
  const px = new Uint8Array(4);
  gl.readPixels(8, 8, 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, px);
  return [px[0], px[1], px[2]];
};
const canvas = document.getElementById('bf');
bf.ready = (async () => {
  const r = await createRenderer(canvas, { width: 16, height: 16, antialias: false, onContextLost: () => bf.calls.lost++, onContextRestored: () => bf.calls.restored++ });
  r.setCamera(IDENT, ORTHO);
  r.setKeyLight(0, 0, 1, 0, 0, 0, 0);
  r.setAmbient(1, 1, 1, 1, 1, 1, Math.PI);
  r.setClearColor(0, 0, 0);
  r.createObject(r.createMesh({ positions: new Float32Array([-1, -1, -5, 1, -1, -5, 1, 1, -5, -1, 1, -5]), normals: new Float32Array([0, 0, 1, 0, 0, 1, 0, 0, 1, 0, 0, 1]), indices: new Uint32Array([0, 1, 2, 0, 2, 3]) }), r.createMaterial({ baseColor: [0, 1, 0, 1] }));
  r.frame();
  bf.r = r;
  bf.before = read(r.gl);
  return true;
})();
bf.after = async () => {
  const r = bf.r;
  const res = { shows: bf.shows.slice(), lostFlag: r.lost, contextLost: r.gl.isContextLost(), calls: { ...bf.calls } };
  res.frame = r.frame();
  res.pixel = read(r.gl);
  const lc = r.gl.getExtension('WEBGL_lose_context');
  let prevented = null;
  canvas.addEventListener('webglcontextlost', (e) => (prevented = e.defaultPrevented), { once: true });
  if (!r.gl.isContextLost()) {
    const lost = eventOf(canvas, 'webglcontextlost');
    lc.loseContext();
    await lost;
    res.prevented = prevented;
    r.dispose();
    const restored = eventOf(canvas, 'webglcontextrestored');
    lc.restoreContext();
    res.restored = await Promise.race([restored, timeout(1500)]);
    await timeout(20);
  }
  res.callsAfter = { ...bf.calls };
  const nav = performance.getEntriesByType('navigation')[0];
  res.notRestoredReasons = nav?.notRestoredReasons ? JSON.parse(JSON.stringify(nav.notRestoredReasons)) : null;
  return res;
};
