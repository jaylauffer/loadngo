// Probe XR's page under a top window of another origin (see probe-xorigin.html). Reports
// { web3dXorigin: details } to the probe page. BSD-3-Clause; see the repository's LICENSE.
const IDENT = new Float32Array([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1]);
const ORTHO = new Float32Array([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, -2 / 9.9, 0, 0, 0, -10.1 / 9.9, 1]);
const quad = { positions: new Float32Array([-1, -1, -5, 1, -1, -5, 1, 1, -5, -1, 1, -5]), normals: new Float32Array([0, 0, 1, 0, 0, 1, 0, 0, 1, 0, 0, 1]), indices: new Uint32Array([0, 1, 2, 0, 2, 3]) };
const opts = { width: 16, height: 16, antialias: false };
const timeout = (ms) => new Promise((res) => setTimeout(() => res('timeout'), ms));
const settle = (p) => p.then((r) => ({ ok: true, r }), (e) => ({ ok: false, message: String(e?.message ?? e) }));
const eventOf = (t, type) => new Promise((res) => t.addEventListener(type, () => setTimeout(() => res(type), 0), { once: true }));
const greenOn = (r) => {
  r.setCamera(IDENT, ORTHO);
  r.setKeyLight(0, 0, 1, 0, 0, 0, 0);
  r.setAmbient(1, 1, 1, 1, 1, 1, Math.PI);
  r.setClearColor(0, 0, 0);
  r.createObject(r.createMesh(quad), r.createMaterial({ baseColor: [0, 1, 0, 1] }));
  r.frame();
  const px = new Uint8Array(4);
  r.gl.readPixels(8, 8, 1, 1, r.gl.RGBA, r.gl.UNSIGNED_BYTE, px);
  return [px[0], px[1], px[2]];
};

async function run() {
  const d = { topReachable: (() => { try { void top[Symbol.for('loadngo-web3d')]; return true; } catch { return false; } })() };
  const c = document.getElementById('c');
  // 1. The child, of this origin, loads web3d before this window does.
  const iframe = document.createElement('iframe');
  iframe.src = './probe-xorigin-child.html';
  document.body.append(iframe);
  await new Promise((res) => iframe.addEventListener('load', res, { once: true }));
  for (let i = 0; i < 50 && !iframe.contentWindow.__child; i++) await timeout(20);
  const child = iframe.contentWindow.__child;
  d.childOnOurs = (await settle(child.createRenderer(c, opts))).message ?? 'created';
  const theirs = await child.createRenderer(child.canvas, opts);
  d.childSerial = theirs._serial;
  d.childDraw = greenOn(theirs);
  // The child's copy keeps its state on this window, its home: the top is out of its reach.
  d.childHomeIsThisWindow = child.state() !== undefined && child.state() === window[Symbol.for('loadngo-web3d')];
  // 2. This window loads web3d now, after its child's copy made the shared state here.
  const { createRenderer } = await import('../pkg/web3d.js');
  const calls = { lost: 0, restored: 0 };
  const ours = await createRenderer(c, { ...opts, onContextLost: () => calls.lost++, onContextRestored: () => calls.restored++ });
  d.ourSerial = ours._serial;
  d.ourDraw = greenOn(ours);
  // 3. The child's copy, again, on this window's canvas, which now has a live renderer.
  d.childAgain = (await settle(child.createRenderer(c, opts))).message ?? 'created';
  // 4. This window's copy on the child's canvas.
  d.oursOnChild = (await settle(createRenderer(child.canvas, opts))).message ?? 'created';
  theirs.dispose();
  // 5. The child goes; this window's renderer loses its context and recovers.
  iframe.remove();
  await timeout(50);
  const lc = ours.gl.getExtension('WEBGL_lose_context');
  let prevented = null;
  c.addEventListener('webglcontextlost', (e) => (prevented = e.defaultPrevented), { once: true });
  const lost = eventOf(c, 'webglcontextlost');
  lc.loseContext();
  await lost;
  d.prevented = prevented;
  ours.dispose();
  const restored = eventOf(c, 'webglcontextrestored');
  lc.restoreContext();
  d.restored = await Promise.race([restored, timeout(2000)]);
  await timeout(20);
  d.calls = calls;
  const next = await settle(createRenderer(c, opts));
  d.next = next.ok ? greenOn(next.r) : next.message;
  if (next.ok) next.r.dispose();
  return d;
}
run().then(
  (d) => parent.postMessage({ web3dXorigin: d }, '*'),
  (e) => parent.postMessage({ web3dXorigin: { error: String(e?.stack ?? e) } }, '*'),
);
