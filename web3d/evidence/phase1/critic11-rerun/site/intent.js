// The ninth critic's N10, N10c and N10e and the tenth's notop, retested by their intent rather than
// their letter. Their intent: a frame's copy of web3d must not be able to drive a canvas of the top
// document, so that the frame's removal cannot leave the top's canvas with a loss nobody cancels
// (N10), the control with the frame kept recovers (N10c), a canvas once used stays restorable
// (N10e), and all of it holds when the top loads web3d only later, after the frame's copy has run
// (notop). Each arrangement in which Chrome gives a top-document canvas the frame's prototype is
// tried: made by the frame and adopted here, made here and touched first by the frame's observer,
// and the plain case. The top imports web3d only after every frame attempt.
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const err = (e) => String(e?.message ?? e).slice(0, 120);
const IDENT = [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1];
const opts = { width: 16, height: 16, antialias: false };
const out = { topHasCopyAtStart: Symbol.for('loadngo-web3d') in window };
const settle = (p) => p.then((r) => ({ ok: true, r }), (e) => ({ ok: false, message: err(e) }));
const green = (r) => {
  r.setCamera(IDENT, [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, -0.2, 0, 0, 0, -1.02, 1]);
  r.setKeyLight(0, 0, 1, 0, 0, 0, 0);
  r.setAmbient(1, 1, 1, 1, 1, 1, Math.PI);
  r.setClearColor(0, 0, 0);
  const mesh = r.createMesh({ positions: new Float32Array([-1, -1, -5, 1, -1, -5, 1, 1, -5, -1, 1, -5]), normals: new Float32Array([0, 0, 1, 0, 0, 1, 0, 0, 1, 0, 0, 1]), indices: new Uint16Array([0, 1, 2, 0, 2, 3]) });
  r.createObject(mesh, r.createMaterial({ baseColor: [0, 1, 0, 1] }));
  const drew = r.frame();
  const px = new Uint8Array(4);
  r.gl.readPixels(8, 8, 1, 1, r.gl.RGBA, r.gl.UNSIGNED_BYTE, px);
  return drew && px[1] > 200 && px[0] < 70 ? 'green' : [drew, ...px];
};
const eventOf = (t, type) => new Promise((res) => t.addEventListener(type, () => setTimeout(() => res(type), 0), { once: true }));
async function cycles(web3d, c, n) {
  // n loss-and-restore cycles, each with a new renderer: cancelled, each hook once, drawn after.
  const seen = [];
  for (let i = 0; i < n; i++) {
    const calls = { lost: 0, restored: 0 };
    const made = await settle(web3d.createRenderer(c, { ...opts, onContextLost: () => calls.lost++, onContextRestored: () => calls.restored++ }));
    if (!made.ok) {
      seen.push({ create: made.message });
      return { cycles: seen, next: 'not reached' };
    }
    const r = made.r;
    const drew = green(r);
    const lc = r.gl.getExtension('WEBGL_lose_context');
    let prevented = null;
    c.addEventListener('webglcontextlost', (e) => (prevented = e.defaultPrevented), { once: true });
    const lost = eventOf(c, 'webglcontextlost');
    lc.loseContext();
    await Promise.race([lost, sleep(3000)]);
    r.dispose();
    const restored = eventOf(c, 'webglcontextrestored');
    try { lc.restoreContext(); } catch (e) { seen.push({ restoreThrew: err(e) }); }
    const ev = await Promise.race([restored, sleep(3000).then(() => 'timeout')]);
    await sleep(20);
    seen.push({ drew, prevented, restored: ev, calls });
  }
  const next = await settle(web3d.createRenderer(c, opts));
  return { cycles: seen, next: next.ok ? green(next.r) : next.message, done: next.ok && next.r.dispose() };
}
const f = document.createElement('iframe');
f.src = 'frame.html';
document.body.append(f);
await new Promise((r) => (f.onload = r));
while (!f.contentWindow.ready) await sleep(10);
const fw = f.contentWindow;
// The frame's copy, before the top has one, on canvases of the top document.
const plain = document.createElement('canvas');
document.body.append(plain);
const adopted = fw.document.createElement('canvas');
document.body.append(adopted);
fw.eval("window.seen = null; window.obs = new MutationObserver((rs) => { for (const r of rs) for (const n of r.addedNodes) if (n.nodeName === 'CANVAS') window.seen = n; }); window.obs.observe(parent.document.body, { childList: true });");
document.body.insertAdjacentHTML('beforeend', '<canvas id="first"></canvas>');
while (!fw.seen) await sleep(5);
fw.obs.disconnect();
const first = document.getElementById('first');
const proto = (c) => (Object.getPrototypeOf(c) === fw.HTMLCanvasElement.prototype ? 'frame' : 'top');
out.prototypes = { plain: proto(plain), adopted: proto(adopted), first: proto(first) };
out.frameCopy = {};
for (const [name, c] of [['plain', plain], ['adopted', adopted], ['first', first]]) {
  const t = await settle(fw.web3d.createRenderer(c, opts));
  out.frameCopy[name] = t.ok ? 'created' : t.message;
  if (t.ok) t.r.dispose();
}
// The frame's copy on its own canvas: allowed, and its loss recovers while the frame is kept (N10c).
out.frameOwnKept = await cycles(fw.web3d, fw.document.body.appendChild(fw.document.createElement('canvas')), 1);
// Now the top loads web3d (notop), drives each canvas, and the frame goes (N10); two cycles each (N10e).
const web3d = await import('/pkg/web3d.js');
out.topCopyLoadedAfterFrame = true;
const before = {};
for (const [name, c] of [['plain', plain], ['adopted', adopted], ['first', first]]) {
  const r = await settle(web3d.createRenderer(c, opts));
  before[name] = r.ok ? green(r.r) : r.message;
  if (r.ok) r.r.dispose();
}
out.topCopyWhileFrameKept = before;
// N10c's control, with the frame kept: each of the top's canvases recovers from a loss.
out.topCanvasesRecoverWhileKept = {};
for (const [name, c] of [['plain', plain], ['adopted', adopted], ['first', first]]) out.topCanvasesRecoverWhileKept[name] = await cycles(web3d, c, 1);
f.remove();
await sleep(100);
out.afterFrameRemoved = {};
for (const [name, c] of [['plain', plain], ['adopted', adopted], ['first', first]]) out.afterFrameRemoved[name] = await cycles(web3d, c, 2);
const ok = (x) => x.cycles.every((k) => k.drew === 'green' && k.prevented === true && k.restored === 'webglcontextrestored' && k.calls.lost === 1 && k.calls.restored === 1) && x.next === 'green';
out.verdict = {
  frameCopyRefusedEveryArrangement: Object.values(out.frameCopy).every((m) => /canvas element is in another document/.test(m)),
  frameOwnCanvasRecoversWhileKept_N10c: ok(out.frameOwnKept),
  topCanvasesRecoverWhileKept_N10c: Object.values(out.topCanvasesRecoverWhileKept).every(ok),
  topCopyDrawsEach: Object.values(before).every((v) => v === 'green'),
  afterFrameRemovedEachRecoversTwice_N10_N10e: Object.values(out.afterFrameRemoved).every(ok),
  topLoadedLazily_notop: out.topHasCopyAtStart === false,
};
out.pass = Object.values(out.verdict).every(Boolean);
window.__done = Promise.resolve(out);
