// Eighth critic's probes for web3d phase 1 (round eight: awaitRestore, the pending-error drain,
// lost renderers, turned and scaled VideoFrames, DataView bytes).
// window.__probe8.done resolves to { probes: [{ id, claim, pass, detail }], passed }.
// `pass` means the behaviour is right, or the documented claim holds; a FAIL is a finding.
import { createRenderer } from '/web3d/pkg/web3d.js';

const out = { probes: [] };
window.__probe8 = { done: null, out };
const log = (id, claim, pass, detail) => out.probes.push({ id, claim, pass: Boolean(pass), detail });

const IDENT = new Float32Array([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1]);
const ORTHO = new Float32Array([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, -2 / 9.9, 0, 0, 0, -10.1 / 9.9, 1]);
const quad = (z = -5) => ({
  positions: new Float32Array([-1, -1, z, 1, -1, z, 1, 1, z, -1, 1, z]),
  normals: new Float32Array([0, 0, 1, 0, 0, 1, 0, 0, 1, 0, 0, 1]),
  uvs: new Float32Array([0, 1, 1, 1, 1, 0, 0, 0]),
  indices: new Uint32Array([0, 1, 2, 0, 2, 3]),
});
const flat = (r) => {
  r.setCamera(IDENT, ORTHO);
  r.setKeyLight(0, 0, 1, 0, 0, 0, 0);
  r.setAmbient(1, 1, 1, 1, 1, 1, Math.PI);
  r.setClearColor(0, 0, 0);
};
const attempt = (fn) => {
  try {
    return { threw: false, value: fn() };
  } catch (e) {
    return { threw: true, message: String(e?.message ?? e) };
  }
};
const settle = (p) => p.then((r) => ({ ok: true, r }), (e) => ({ ok: false, message: String(e?.message ?? e) }));
const timeout = (ms) => new Promise((res) => setTimeout(() => res('timeout'), ms));
const made = [];
/** A fresh canvas; contexts of finished probes are released so Chrome's 16-context limit drops none of ours. */
const canvas = (w = 16, h = 16) => {
  while (made.length > 6) made.shift().getContext('webgl2')?.getExtension('WEBGL_lose_context')?.loseContext();
  const c = document.createElement('canvas');
  made.push(c);
  c.width = w;
  c.height = h;
  document.body.append(c);
  return c;
};
const pixel = (r, nx, ny) => {
  const px = new Uint8Array(4);
  const { width, height } = r.canvas;
  r.gl.readPixels(Math.min(width - 1, Math.floor(((nx + 1) / 2) * width)), Math.min(height - 1, Math.floor(((ny + 1) / 2) * height)), 1, 1, r.gl.RGBA, r.gl.UNSIGNED_BYTE, px);
  return [px[0], px[1], px[2]];
};
const green = (p) => p && p[1] > 200 && p[0] < 60 && p[2] < 60;
const event = (target, type) => new Promise((res) => target.addEventListener(type, () => setTimeout(() => res(type), 0), { once: true }));
const OPTS = { width: 16, height: 16, antialias: false };
/** Draws a green quad and returns the centre pixel: proof a renderer works. */
const drawsGreen = (r) => {
  flat(r);
  r.createObject(r.createMesh(quad()), r.createMaterial({ baseColor: [0, 1, 0, 1] }));
  r.frame();
  return pixel(r, 0, 0);
};
// Page errors from hooks that throw on purpose are expected; keep them out of the console noise.
const expectedErrors = [];
window.addEventListener('error', (e) => {
  if (String(e.message).includes('[expected]')) {
    expectedErrors.push(String(e.message));
    e.preventDefault();
  }
});

/** E1: a hook that throws stops the hooks queued after it on the same canvas. */
async function throwingHook() {
  const c = canvas();
  const calls = { a: 0, b: 0 };
  const a = await createRenderer(c, { ...OPTS, onContextRestored: () => { calls.a++; throw new Error('[expected] hook A throws'); } });
  const lc = a.gl.getExtension('WEBGL_lose_context');
  const lost = event(c, 'webglcontextlost');
  lc.loseContext();
  a.dispose(); // disposed while lost, as the LOST message says: A's hook is queued
  const b = await settle(createRenderer(c, { ...OPTS, onContextRestored: () => calls.b++ })); // fails as lost: B's hook queued
  await lost;
  const restored = event(c, 'webglcontextrestored');
  lc.restoreContext();
  await Promise.race([restored, timeout(3000)]);
  await timeout(100);
  const d = { calls, bCreate: b.ok ? 'created' : b.message, expectedErrors: expectedErrors.length };
  log('E1', "a failed create's onContextRestored is called on the restore even when another renderer's hook on that canvas throws", calls.a === 1 && calls.b === 1, d);
}

/** E2: is there a task where isContextLost() is false but webglcontextrestored has not been dispatched? Dispose there. */
async function restoreWindow() {
  const c = canvas();
  const calls = { a: 0 };
  const a = await createRenderer(c, { ...OPTS, onContextRestored: () => calls.a++ });
  const gl = a.gl;
  const lc = gl.getExtension('WEBGL_lose_context');
  const lost = event(c, 'webglcontextlost');
  lc.loseContext();
  await lost;
  let restoredSeen = false;
  c.addEventListener('webglcontextrestored', () => (restoredSeen = true), { once: true });
  lc.restoreContext();
  const d = { ticks: 0, windowSeen: false, disposedIn: null };
  const mc = new MessageChannel();
  await new Promise((res) => {
    mc.port1.onmessage = () => {
      d.ticks++;
      if (!gl.isContextLost() && !restoredSeen && !d.windowSeen) {
        d.windowSeen = true;
        d.disposedIn = d.ticks;
        a.dispose(); // lost renderer disposed: the hook should still come
      }
      if (restoredSeen || d.ticks > 20000) return res();
      mc.port2.postMessage(0);
    };
    mc.port2.postMessage(0);
  });
  await timeout(100);
  if (!d.windowSeen) a.dispose();
  d.calls = calls;
  d.restoredSeen = restoredSeen;
  log('E2', 'a lost renderer disposed after isContextLost() turns false but before webglcontextrestored is dispatched still gets its hook (or there is no such window)', !d.windowSeen || calls.a === 1, d);
}

/** E3: hooks that create a renderer inside the hook: the first creates and draws, the second is refused as shared. */
async function createInsideHook() {
  const c = canvas();
  let first = null;
  let second = null;
  const a = await createRenderer(c, { ...OPTS, onContextRestored: () => (first = settle(createRenderer(c, OPTS))) });
  const lc = a.gl.getExtension('WEBGL_lose_context');
  const lost = event(c, 'webglcontextlost');
  lc.loseContext();
  await lost; // dispose after the lost event this time
  a.dispose();
  const failed = await settle(createRenderer(c, { ...OPTS, onContextRestored: () => (second = settle(createRenderer(c, OPTS))) }));
  const restored = event(c, 'webglcontextrestored');
  lc.restoreContext();
  await Promise.race([restored, timeout(3000)]);
  await timeout(50);
  const r1 = first ? await first : { ok: false, message: 'hook never ran' };
  const r2 = second ? await second : { ok: false, message: 'hook never ran' };
  const d = { failedCreate: failed.ok ? 'created' : failed.message, first: r1.ok ? 'created' : r1.message, second: r2.ok ? 'created' : r2.message };
  if (r1.ok) {
    d.pixel = drawsGreen(r1.r);
    d.getError = r1.r.gl.getError();
    r1.r.dispose();
  }
  if (r2.ok) r2.r.dispose();
  log('E3', 'renderers created inside the restore hooks: one draws, the other is refused as shared; getError 0', r1.ok && green(d.pixel) && d.getError === 0 && !r2.ok && /live renderer/.test(r2.message), d);
}

/** E4: several renderers and failed creates on one canvas; one function used as two renderers' hook. */
async function manyOnOneCanvas() {
  const c = canvas();
  const n = { a: 0, b: 0, c: 0, shared: 0 };
  const shared = () => n.shared++;
  const a = await createRenderer(c, { ...OPTS, onContextRestored: () => n.a++ });
  const lc = a.gl.getExtension('WEBGL_lose_context');
  const lost = event(c, 'webglcontextlost');
  lc.loseContext();
  const fb = await settle(createRenderer(c, { ...OPTS, onContextRestored: () => n.b++ }));
  const fc = await settle(createRenderer(c, { ...OPTS, onContextRestored: () => n.c++ }));
  await lost;
  a.dispose();
  const fs1 = await settle(createRenderer(c, { ...OPTS, onContextRestored: shared }));
  const fs2 = await settle(createRenderer(c, { ...OPTS, onContextRestored: shared }));
  const restored = event(c, 'webglcontextrestored');
  lc.restoreContext();
  await Promise.race([restored, timeout(3000)]);
  await timeout(50);
  const map = globalThis[Symbol.for('loadngo-web3d')].restores;
  const d = { n, creates: [fb, fc, fs1, fs2].map((x) => (x.ok ? 'created' : x.message.slice(0, 40))), mapEntryLeft: map.has(c) };
  // Second shape: a live (undisposed) renderer and a failed create with the same hook.
  const c2 = canvas();
  let twice = 0;
  const hook = () => twice++;
  const live = await createRenderer(c2, { ...OPTS, onContextRestored: hook });
  const lc2 = live.gl.getExtension('WEBGL_lose_context');
  const lost2 = event(c2, 'webglcontextlost');
  lc2.loseContext();
  await lost2;
  await settle(createRenderer(c2, { ...OPTS, onContextRestored: hook }));
  const restored2 = event(c2, 'webglcontextrestored');
  lc2.restoreContext();
  await Promise.race([restored2, timeout(3000)]);
  await timeout(50);
  live.dispose();
  d.sameHookLiveAndFailed = twice;
  log('E4', 'each queued hook runs once on the restore (a, b, c once; one function queued twice runs once); the map entry is cleared', n.a === 1 && n.b === 1 && n.c === 1 && n.shared === 1 && !d.mapEntryLeft, d);
  log('E4b', 'note only: one function given to a live renderer and to a failed create on its canvas is called this many times by one restore', true, { calls: twice });
}

/** E5: dispose before the lost event and after it (not in a hook): the hook comes once each time. */
async function disposeTimings() {
  const d = {};
  for (const when of ['before-event', 'after-event']) {
    const c = canvas();
    let calls = 0;
    const a = await createRenderer(c, { ...OPTS, onContextRestored: () => calls++ });
    const lc = a.gl.getExtension('WEBGL_lose_context');
    const lost = event(c, 'webglcontextlost');
    lc.loseContext();
    if (when === 'before-event') a.dispose();
    await lost;
    if (when === 'after-event') a.dispose();
    const restored = event(c, 'webglcontextrestored');
    const before = c.getContext('webgl2').getError();
    lc.restoreContext();
    const got = await Promise.race([restored, timeout(3000)]);
    await timeout(50);
    const next = await settle(createRenderer(c, OPTS));
    d[when] = { calls, restored: got, restoreError: before, next: next.ok ? 'created' : next.message };
    if (next.ok) {
      d[when].pixel = drawsGreen(next.r);
      next.r.dispose();
    }
  }
  log('E5', 'disposed before or after the lost event: the hook comes once, and a new renderer draws', ['before-event', 'after-event'].every((k) => d[k].calls === 1 && green(d[k].pixel)), d);
}

/** E6: a lost renderer left undisposed (allowed: "dispose it, in onContextLost or later") hears its successor's loss and restore. */
async function staleHooks() {
  const c = canvas();
  const n = { aLost: 0, aRestored: 0, bLost: 0, bRestored: 0 };
  const a = await createRenderer(c, { ...OPTS, onContextLost: () => n.aLost++, onContextRestored: () => n.aRestored++ });
  let lc = a.gl.getExtension('WEBGL_lose_context');
  let lost = event(c, 'webglcontextlost');
  lc.loseContext();
  await lost;
  let restored = event(c, 'webglcontextrestored');
  lc.restoreContext();
  await Promise.race([restored, timeout(3000)]);
  const b = await createRenderer(c, { ...OPTS, onContextLost: () => n.bLost++, onContextRestored: () => n.bRestored++ });
  const afterFirst = { ...n };
  lc = b.gl.getExtension('WEBGL_lose_context');
  lost = event(c, 'webglcontextlost');
  lc.loseContext();
  await lost;
  restored = event(c, 'webglcontextrestored');
  lc.restoreContext();
  await Promise.race([restored, timeout(3000)]);
  await timeout(50);
  b.dispose();
  a.dispose();
  const d = { afterFirstCycle: afterFirst, afterSecondCycle: n };
  log('E6', "a finished (lost, undisposed) renderer's hooks are not called for the next renderer's loss and restore", n.aLost === 1 && n.aRestored === 1 && n.bLost === 1 && n.bRestored === 1, d);
}

/** E7: the drain: how many errors Chrome can hold, whether 16 covers them, and that the page's error is consumed. */
async function drain() {
  const c = canvas();
  const gl = c.getContext('webgl2', { antialias: false });
  const raise = () => {
    gl.enable(0x1234); // INVALID_ENUM
    gl.lineWidth(-1); // INVALID_VALUE
    gl.useProgram(null);
    gl.drawArrays(gl.TRIANGLES, 0, 3); // INVALID_OPERATION (no program)
    const fb = gl.createFramebuffer();
    gl.bindFramebuffer(gl.FRAMEBUFFER, fb);
    gl.clear(gl.COLOR_BUFFER_BIT); // INVALID_FRAMEBUFFER_OPERATION (no attachment)
    gl.bindFramebuffer(gl.FRAMEBUFFER, null);
    gl.deleteFramebuffer(fb);
  };
  raise();
  const held = [];
  for (let i = 0; i < 64; i++) {
    const e = gl.getError();
    if (e === 0) break;
    held.push(`0x${e.toString(16)}`);
  }
  const r = await createRenderer(c, OPTS);
  flat(r);
  const warnings = [];
  const warn = console.warn;
  console.warn = (...a) => (warnings.push(String(a[0])), warn.apply(console, a));
  raise();
  const data = new Uint8ClampedArray(4 * 4 * 4).fill(255);
  const bmp = await createImageBitmap(new ImageData(data, 4, 4));
  const made = attempt(() => r.createTexture(bmp));
  console.warn = warn;
  const pageSees = gl.getError();
  r.dispose();
  const d = { errorsChromeHeld: held, texture: made.threw ? made.message : 'created', pendingWarnings: warnings.filter((w) => /pending before a texture upload/.test(w)).length, pageGetErrorAfter: pageSees };
  log('E7', 'the drain bound of 16 covers every error Chrome holds at once, and a good texture is made despite them', held.length <= 16 && !made.threw && d.pendingWarnings === held.length, d);
  log('E7b', "note only: the page's own pending errors are consumed by createTexture (getError after it)", true, { pageGetErrorAfter: pageSees });
}

/** RGBA pixels: left half green, right half red. */
const halves = (w, h) => {
  const px = new Uint8Array(w * h * 4);
  for (let y = 0; y < h; y++) for (let x = 0; x < w; x++) px.set(x < w / 2 ? [0, 255, 0, 255] : [255, 0, 0, 255], (y * w + x) * 4);
  return px;
};

/** What WebGL itself uploads from a source: the error for each texture size tried. */
function rawUpload(source, sizes) {
  const c = canvas(4, 4);
  const gl = c.getContext('webgl2');
  const res = {};
  for (const [w, h] of sizes) {
    const t = gl.createTexture();
    gl.bindTexture(gl.TEXTURE_2D, t);
    gl.texStorage2D(gl.TEXTURE_2D, 1, gl.RGBA8, w, h);
    while (gl.getError());
    gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, gl.RGBA, gl.UNSIGNED_BYTE, source);
    res[`${w}x${h}`] = gl.getError();
    // Read the corners back through a framebuffer.
    const fb = gl.createFramebuffer();
    gl.bindFramebuffer(gl.FRAMEBUFFER, fb);
    gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.TEXTURE_2D, t, 0);
    const px = new Uint8Array(4);
    const at = (x, y) => (gl.readPixels(x, y, 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, px), [...px]);
    res[`${w}x${h} texels`] = { x0y0: at(0, 0), xMaxY0: at(w - 1, 0), x0yMax: at(0, h - 1), xMaxYMax: at(w - 1, h - 1) };
    gl.bindFramebuffer(gl.FRAMEBUFFER, null);
    gl.deleteFramebuffer(fb);
    gl.deleteTexture(t);
  }
  gl.getExtension('WEBGL_lose_context')?.loseContext();
  return res;
}

/** web3d's texture of a source, drawn on a full-canvas quad: the four quadrants' pixels. */
async function web3dTexture(source) {
  const r = await createRenderer(canvas(16, 16), OPTS);
  flat(r);
  const t = attempt(() => r.createTexture(source, { srgb: false, mipmaps: false, wrapS: 'clamp', wrapT: 'clamp' }));
  const d = { texture: t.threw ? t.message : 'created' };
  if (!t.threw) {
    const m = r.createMaterial({ texture: t.value });
    r.createObject(r.createMesh(quad()), m);
    r.frame();
    d.quadrants = { leftTop: pixel(r, -0.5, 0.5), rightTop: pixel(r, 0.5, 0.5), leftBottom: pixel(r, -0.5, -0.5), rightBottom: pixel(r, 0.5, -0.5) };
    d.getError = r.gl.getError();
  }
  r.dispose();
  return d;
}

/** E8: VideoFrames whose display size differs from the visible rectangle, and turned frames. */
async function videoFrames() {
  if (typeof VideoFrame !== 'function') return log('E8', 'VideoFrame unsupported', true, {});
  const make = (w, h, init) => new VideoFrame(halves(w, h), { format: 'RGBA', codedWidth: w, codedHeight: h, timestamp: 0, ...init });
  const cases = {
    'display 4x4 of visible 8x8': [make(8, 8, { displayWidth: 4, displayHeight: 4 }), [[8, 8], [4, 4]]],
    'display 16x16 of visible 8x8': [make(8, 8, { displayWidth: 16, displayHeight: 16 }), [[8, 8], [16, 16]]],
    'display 4x8 of visible 8x8 (non-square pixels)': [make(8, 8, { displayWidth: 4, displayHeight: 8 }), [[8, 8], [4, 8]]],
    'rotation 90, 4x2': [make(4, 2, { rotation: 90 }), [[2, 4], [4, 2]]],
    'rotation 270, 4x2': [make(4, 2, { rotation: 270 }), [[2, 4], [4, 2]]],
    'rotation 180, 4x2': [make(4, 2, { rotation: 180 }), [[4, 2]]],
    'rotation 90 with display 1x2, 4x2': [make(4, 2, { rotation: 90, displayWidth: 1, displayHeight: 2 }), [[2, 4], [1, 2]]],
  };
  const d = {};
  let silentCorner = false;
  for (const [name, [frame, sizes]] of Object.entries(cases)) {
    const vr = frame.visibleRect;
    d[name] = { visible: `${vr.width}x${vr.height}`, display: `${frame.displayWidth}x${frame.displayHeight}`, rotation: frame.rotation, raw: rawUpload(frame, sizes), web3d: await web3dTexture(frame) };
    const q = d[name].web3d.quadrants;
    // A texture made but only partly filled shows black (0, 0, 0) in some quadrant.
    if (q && Object.values(q).some((p) => p[0] < 30 && p[1] < 30 && p[2] < 30)) silentCorner = true;
    frame.close();
  }
  log('E8', 'every VideoFrame web3d accepts fills its whole texture (none made and only partly filled)', !silentCorner, d);
}

/** E9: DataView bytes: a sub-range, another realm's, a detached one, a shared one. */
async function dataViews() {
  const bytes = new Uint8Array(await (await fetch('/web3d/pkg/loadngo_web3d.wasm')).arrayBuffer());
  const d = {};
  const tryWith = async (name, wasm) => {
    const c = canvas();
    const got = await settle(createRenderer(c, { ...OPTS, wasm }));
    d[name] = got.ok ? 'created' : got.message.slice(0, 200);
    if (got.ok) {
      d[`${name} pixel`] = drawsGreen(got.r);
      got.r.dispose();
    } else {
      const again = await settle(createRenderer(c, OPTS));
      d[`${name} canvas usable after`] = again.ok;
      d[`${name} size after`] = [c.width, c.height];
      if (again.ok) again.r.dispose();
    }
  };
  const padded = new Uint8Array(bytes.length + 14);
  padded.fill(0xee);
  padded.set(bytes, 7);
  await tryWith('sub-range DataView', new DataView(padded.buffer, 7, bytes.length));
  const f = document.createElement('iframe');
  document.body.append(f);
  const theirs = new f.contentWindow.ArrayBuffer(bytes.length);
  new f.contentWindow.Uint8Array(theirs).set(bytes);
  await tryWith('another realm DataView', new f.contentWindow.DataView(theirs));
  f.remove();
  const gone = bytes.slice().buffer;
  const dv = new DataView(gone);
  structuredClone(gone, { transfer: [gone] });
  await tryWith('detached DataView', dv);
  d.crossOriginIsolated = self.crossOriginIsolated;
  if (typeof SharedArrayBuffer === 'function') {
    const sab = new SharedArrayBuffer(bytes.length);
    new Uint8Array(sab).set(bytes);
    await tryWith('shared DataView', new DataView(sab));
    await tryWith('shared Uint8Array', new Uint8Array(sab));
  }
  log('E9', 'DataView bytes compile from a sub-range and another realm', d['sub-range DataView'] === 'created' && d['another realm DataView'] === 'created' && green(d['sub-range DataView pixel']), d);
  log('E9b', 'a detached or shared buffer is refused with a web3d message and leaves the canvas usable', !/^created/.test(d['detached DataView']) && /web3d/.test(d['detached DataView']) && d['detached DataView canvas usable after'] && (d['shared DataView'] === undefined || d['shared DataView'] === 'created' || /web3d/.test(d['shared DataView'])), d);
}

/** E10: resize and frame right after loseContext, before the event. */
async function rightAfterLoss() {
  const c = canvas(32, 32);
  const r = await createRenderer(c, { width: 32, height: 32, antialias: false });
  const lc = r.gl.getExtension('WEBGL_lose_context');
  const lost = event(c, 'webglcontextlost');
  lc.loseContext();
  const d = { frame: attempt(() => r.frame()), resize: attempt(() => r.resize(8, 8)), size: [c.width, c.height], create: attempt(() => r.createMaterial({})) };
  await lost;
  r.dispose();
  lc.restoreContext();
  log('E10', 'right after a loss, before its event: frame() is false, resize and creates throw "lost", the canvas keeps its size', d.frame.value === false && d.resize.threw && /lost/.test(d.resize.message) && d.size[0] === 32 && d.create.threw, d);
}

/** E11: a context lost meanwhile, during createRenderer, on a canvas whose context the page made (no listener of its own). */
async function lostMeanwhile() {
  const c = canvas();
  const gl = c.getContext('webgl2', { antialias: false });
  const lc = gl.getExtension('WEBGL_lose_context');
  let hook = 0;
  const p = settle(createRenderer(c, { ...OPTS, onContextRestored: () => hook++ }));
  lc.loseContext(); // in the same task as the call: its event has not been dispatched yet
  const res = await p;
  const lost = event(c, 'webglcontextlost');
  await Promise.race([lost, timeout(1000)]);
  while (gl.getError());
  const restored = event(c, 'webglcontextrestored');
  lc.restoreContext();
  const restoreError = gl.getError();
  const got = await Promise.race([restored, timeout(1500)]);
  await timeout(50);
  const d = { create: res.ok ? 'created' : res.message, restoreError: `0x${restoreError.toString(16)}`, restored: got, hookCalls: hook };
  if (res.ok) res.r.dispose();
  log('E11', 'WEB3D.md: "A context lost so recently that its event has not been dispatched yet is still allowed a restore" (a loss during the create, page-made context)', got === 'webglcontextrestored' && hook === 1, d);
}

/** E12: the canvas lost while no renderer holds it: LOST_AT_CREATE promises a restore. */
async function lostWhileFree() {
  const c = canvas();
  const r = await createRenderer(c, OPTS);
  const gl = r.gl;
  const lc = gl.getExtension('WEBGL_lose_context');
  r.dispose(); // the canvas is free; nobody listens now
  const lost = event(c, 'webglcontextlost');
  lc.loseContext();
  await lost;
  let hook = 0;
  const res = await settle(createRenderer(c, { ...OPTS, onContextRestored: () => hook++ }));
  while (gl.getError());
  const restored = event(c, 'webglcontextrestored');
  lc.restoreContext();
  const restoreError = gl.getError();
  const got = await Promise.race([restored, timeout(1500)]);
  const d = { create: res.ok ? 'created' : res.message, restoreError: `0x${restoreError.toString(16)}`, restored: got, hookCalls: hook };
  log('E12', 'note: a canvas lost while no renderer holds it is never restored; the create message still promises onContextRestored', true, d);
}

async function main() {
  for (const p of [throwingHook, restoreWindow, createInsideHook, manyOnOneCanvas, disposeTimings, staleHooks, drain, videoFrames, dataViews, rightAfterLoss, lostMeanwhile, lostWhileFree]) {
    try {
      await p();
    } catch (e) {
      log(p.name, 'probe threw', false, String(e?.stack ?? e));
    }
  }
  out.passed = out.probes.filter((p) => p.pass).length;
  return out;
}
window.__probe8.done = main();
