// Tenth critic: the loss controller's listeners against real frames, on a top window that has no
// copy of web3d.js at first (no simulation: nothing is deleted from the shared state). The top's
// copy is imported partway through, after a frame's copy has already made the listeners: the case
// "a frame copy loaded before the top's". Then nested same-origin frames. window.__listeners(id) is
// given by the driver (DevTools protocol). Results in window.__probe10.done.
const out = {};
const timeout = (ms) => new Promise((res) => setTimeout(() => res('timeout'), ms));
const settle = (p) => p.then((r) => ({ ok: true, r }), (e) => ({ ok: false, message: String(e?.message ?? e) }));
const eventOf = (t, type) => new Promise((res) => t.addEventListener(type, () => setTimeout(() => res(type), 0), { once: true }));
const opts = { width: 16, height: 16, antialias: false };
const N = 0.1;
const F = 10;
const ORTHO = new Float32Array([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, -2 / (F - N), 0, 0, 0, -(F + N) / (F - N), 1]);
const IDENT = new Float32Array([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1]);
const quad = {
  positions: new Float32Array([-1, -1, -5, 1, -1, -5, 1, 1, -5, -1, 1, -5]),
  normals: new Float32Array([0, 0, 1, 0, 0, 1, 0, 0, 1, 0, 0, 1]),
  indices: new Uint32Array([0, 1, 2, 0, 2, 3]),
};
const green = (r, color = [0, 1, 0, 1]) => {
  r.setCamera(IDENT, ORTHO);
  r.setKeyLight(0, 0, 1, 0, 0, 0, 0);
  r.setAmbient(1, 1, 1, 1, 1, 1, Math.PI);
  r.setClearColor(0, 0, 0);
  r.createObject(r.createMesh(quad), r.createMaterial({ baseColor: color }));
  r.frame();
  const px = new Uint8Array(4);
  r.gl.readPixels(8, 8, 1, 1, r.gl.RGBA, r.gl.UNSIGNED_BYTE, px);
  return [px[0], px[1], px[2]];
};
let n = 0;
const canvasFor = () => {
  const c = document.createElement('canvas');
  c.id = `f10-${++n}`;
  c.width = 16;
  c.height = 16;
  document.body.append(c);
  return c;
};
const frame = async (src = './frame.html') => {
  const f = document.createElement('iframe');
  f.src = src;
  document.body.append(f);
  await new Promise((res) => f.addEventListener('load', res, { once: true }));
  return f;
};
const copyIn = async (win) => {
  for (let i = 0; i < 100 && !win.__frame; i++) await timeout(20);
  return win.__frame;
};
const listeners = async (c) => (typeof window.__listeners === 'function' ? window.__listeners(c.id) : 'no driver');
/** Loses the canvas's context; returns whether the lost event was cancelled (seen by a listener added last). */
const lose = async (c) => {
  let prevented = null;
  c.addEventListener('webglcontextlost', (e) => (prevented = e.defaultPrevented), { once: true });
  const lost = eventOf(c, 'webglcontextlost');
  // Kept for the restore: getExtension returns null once the context is lost.
  c.__lc = c.getContext('webgl2').getExtension('WEBGL_lose_context');
  c.__lc.loseContext();
  await lost;
  return prevented;
};
const restore = async (c) => {
  let fired = false;
  const once = () => (fired = true);
  c.addEventListener('webglcontextrestored', once, { once: true });
  c.__lc.restoreContext(); // Chrome raises a GL error, not an exception, for a loss nobody cancelled
  for (let i = 0; i < 75 && !fired; i++) await timeout(20);
  c.removeEventListener('webglcontextrestored', once);
  await timeout(0);
  return fired ? 'webglcontextrestored' : 'no restore within 1.5 s';
};

async function run() {
  out.sharedStateBefore = Boolean(window[Symbol.for('loadngo-web3d')]);

  // R1. The documented limit, for real: no copy in the top window; the frame's copy made the
  // listeners; the frame is removed; a loss before any create is not cancelled.
  {
    const c = canvasFor();
    const f = await frame();
    const fc = await copyIn(f.contentWindow);
    const theirs = await fc.createRenderer(c, opts);
    out.r1Drawn = green(theirs);
    theirs.dispose();
    f.remove();
    await timeout(50);
    out.r1Prevented = await lose(c);
    out.r1Listeners = await listeners(c);
    const f2 = await frame();
    const fc2 = await copyIn(f2.contentWindow);
    out.r1Create = (await settle(fc2.createRenderer(c, opts))).message ?? 'created';
    f2.remove();
  }

  // R2. A frame navigated to another origin, no copy in the top: a later create from another frame
  // puts the listeners back (listening() must treat the cross-origin document as gone).
  {
    const c = canvasFor();
    const f = await frame();
    const fc = await copyIn(f.contentWindow);
    (await fc.createRenderer(c, opts)).dispose();
    const nav = new Promise((res) => f.addEventListener('load', res, { once: true }));
    f.src = `${window.__foreign}/c/blank.html`;
    await nav;
    await timeout(50);
    const f2 = await frame();
    const fc2 = await copyIn(f2.contentWindow);
    const calls = { lost: 0, restored: 0 };
    const r = await fc2.createRenderer(c, { ...opts, onContextLost: () => calls.lost++, onContextRestored: () => calls.restored++ });
    out.r2Drawn = green(r);
    out.r2Listeners = await listeners(c);
    out.r2Prevented = await lose(c);
    r.dispose();
    out.r2Restored = await restore(c);
    await timeout(20);
    out.r2Calls = calls;
    f.remove();
    f2.remove();
  }

  // R3. A frame copy loaded before the top's: the frame's copy creates (and keeps) a renderer on a
  // canvas here; then the top window's copy is loaded; then the frame is removed, and the context
  // lost before any create. docs/WEB3D.md: "the listeners are made by the top window's copy of the
  // module once one has loaded (it outlives every frame)", and the one case left needs "no copy
  // in the top window".
  let top;
  {
    const c = canvasFor();
    const f = await frame();
    const fc = await copyIn(f.contentWindow);
    const calls = { lost: 0, restored: 0 };
    const theirs = await fc.createRenderer(c, { ...opts, onContextLost: () => calls.lost++, onContextRestored: () => calls.restored++ });
    out.r3Drawn = green(theirs);
    // A second canvas the frame's copy used and disposed before the top's copy loads.
    const c2 = canvasFor();
    (await fc.createRenderer(c2, opts)).dispose();
    top = await import('/web3d/pkg/web3d.js'); // the top window's copy loads now
    out.r3TopInstall = typeof window[Symbol.for('loadngo-web3d')].install;
    f.remove();
    await timeout(50);
    out.r3Prevented = await lose(c);
    out.r3Listeners = await listeners(c);
    out.r3Restored = await restore(c);
    await timeout(20);
    out.r3Calls = calls;
    out.r3Lost = theirs.lost;
    theirs.dispose();
    const next = await settle(top.createRenderer(c, opts));
    out.r3Next = next.ok ? 'created' : next.message;
    if (next.ok) next.r.dispose();
    out.r3DisposedPrevented = await lose(c2);
    out.r3DisposedRestored = await restore(c2);
  }
  // R3b. The same with the frame's renderer disposed before the top's copy loads (a copy loaded
  // later in the top, as a page that imports web3d lazily would).
  {
    const c = canvasFor();
    const f = await frame();
    const fc = await copyIn(f.contentWindow);
    // Simulate "before the top's copy" for this canvas: the frame's copy installs its own listeners
    // when the shared installer is not there yet. Here it is already there (R3 loaded it), so this
    // case instead checks a frame's create after the top's copy loaded: listeners are the top's.
    (await fc.createRenderer(c, opts)).dispose();
    f.remove();
    await timeout(50);
    out.r3bPrevented = await lose(c);
    out.r3bRestored = await restore(c);
    const next = await settle(top.createRenderer(c, opts));
    out.r3bNext = next.ok ? green(next.r) : next.message;
    if (next.ok) next.r.dispose();
  }

  // R4. Nested same-origin frames, top copy loaded: a frame (no copy) holds a frame with a copy,
  // which creates on a canvas here; the outer frame is removed (the inner goes with it).
  {
    const c = canvasFor();
    const a = await frame('./nest-a.html');
    const inner = a.contentDocument.getElementById('inner');
    if (inner.contentDocument?.readyState !== 'complete') await new Promise((res) => inner.addEventListener('load', res, { once: true }));
    const fc = await copyIn(inner.contentWindow);
    const calls = { lost: 0, restored: 0 };
    const theirs = await fc.createRenderer(c, { ...opts, onContextLost: () => calls.lost++, onContextRestored: () => calls.restored++ });
    out.r4Drawn = green(theirs);
    a.remove();
    await timeout(50);
    out.r4Prevented = await lose(c);
    theirs.dispose();
    out.r4Restored = await restore(c);
    await timeout(20);
    out.r4Calls = calls;
    out.r4Listeners = await listeners(c);
  }
  // R3 again, with the frame's renderer disposed while the frame is there (the RF probe's own (b)
  // order), on a fresh canvas the top's copy has not seen: is it the frame's copy's listeners that
  // stay, even though the top's copy is loaded? (R3b is the reverse: the frame creates after the
  // top's copy loaded, so the top's installer runs.) Here the top's copy is already loaded, so the
  // frame's create uses the top's installer: the listeners are the top's.
  return out;
}
window.__probe10 = { done: run().catch((e) => ({ error: String(e?.stack ?? e), partial: out })) };
