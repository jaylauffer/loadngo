// N10d: a same-origin frame's copy of web3d.js loads before any copy in the top window, so the shared
// state on the top window (its WeakMaps, the serial counter) is made in the frame's realm. The frame
// is removed; then the top window imports web3d.js and uses that state.
const out = { probes: [] };
window.__probe9 = { done: null, out };
const log = (id, claim, pass, detail) => out.probes.push({ id, claim, pass: Boolean(pass), detail });
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
const green = (p) => p[1] > 200 && p[0] < 60 && p[2] < 60;
async function main() {
  const d = {};
  const iframe = document.createElement('iframe');
  iframe.src = '/c/frame.html';
  document.body.append(iframe);
  await new Promise((res) => iframe.addEventListener('load', res, { once: true }));
  for (let i = 0; i < 100 && !iframe.contentWindow.__frame; i++) await timeout(20);
  const state = window[Symbol.for('loadngo-web3d')];
  d.stateMadeInFrame = Object.getPrototypeOf(state) !== Object.prototype;
  // The frame draws on its own canvas once, then goes away.
  const fc = iframe.contentDocument.createElement('canvas');
  iframe.contentDocument.body.append(fc);
  const fr = await iframe.contentWindow.__frame.createRenderer(fc, { width: 16, height: 16, antialias: false });
  d.frameDraw = draw(fr);
  fr.dispose();
  iframe.remove();
  await timeout(50);
  const { createRenderer } = await import('/web3d/pkg/web3d.js');
  const c = document.createElement('canvas');
  document.body.append(c);
  const calls = { lost: 0, restored: 0 };
  try {
    const r = await createRenderer(c, { width: 16, height: 16, antialias: false, onContextLost: () => calls.lost++, onContextRestored: () => calls.restored++ });
    d.draw = draw(r);
    const lc = r.gl.getExtension('WEBGL_lose_context');
    const lost = event(c, 'webglcontextlost');
    lc.loseContext();
    await lost;
    r.dispose();
    const restored = event(c, 'webglcontextrestored');
    lc.restoreContext();
    d.restored = await Promise.race([restored, timeout(2000)]);
    await timeout(20);
    d.calls = { ...calls };
    const next = await createRenderer(c, { width: 16, height: 16, antialias: false });
    d.next = draw(next);
    d.shared = (await createRenderer(c, { width: 16, height: 16, antialias: false }).then(() => 'created', (e) => e.message));
    next.dispose();
  } catch (e) {
    d.error = String(e?.stack ?? e);
  }
  log(
    'N10d',
    "a frame's copy made the shared state on the top window, then the frame was removed: the top window's copy still creates, draws, recovers from a loss and refuses a second renderer",
    !d.error && green(d.draw) && d.restored === 'webglcontextrestored' && d.calls.lost === 1 && d.calls.restored === 1 && green(d.next) && /already has a live renderer/.test(d.shared),
    d,
  );
  out.passed = out.probes.filter((p) => p.pass).length;
}
window.__probe9.done = main().then(() => out, (e) => ((out.error = String(e?.stack ?? e)), out));
