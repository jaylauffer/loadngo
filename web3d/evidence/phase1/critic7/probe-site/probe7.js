// Seventh critic's probes for web3d phase 1 (round seven: lost-context creates, exact texture sizes,
// depth required, realm-shared serials, the 50-bit handle layout, the wasm option's brand check).
// window.__probe6.done resolves to { probes: [{ id, claim, pass, detail }], passed, failed }.
// `pass` means the behaviour is correct (or the documented claim holds); a FAIL is a finding.
import { createRenderer } from '/web3d/pkg/web3d.js';

const out = { probes: [] };
window.__probe6 = { done: null, out };
const log = (id, claim, pass, detail) => out.probes.push({ id, claim, pass: Boolean(pass), detail });

const KEY = Symbol.for('loadngo-web3d');
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
const frames = (n) => new Promise((res) => { const step = () => (n-- <= 0 ? res() : requestAnimationFrame(step)); step(); });
const made = [];
/** A canvas for one probe; contexts of finished probes are released so Chrome's 16-context limit drops none of ours. */
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
const rgba = (w, h, f) => {
  const d = new Uint8ClampedArray(w * h * 4);
  for (let y = 0; y < h; y++) for (let x = 0; x < w; x++) d.set(f(x, y), (y * w + x) * 4);
  return d;
};
const GREEN = [0, 255, 0, 255];
const RED = [255, 0, 0, 255];
const bitmap = (w, h, f = () => GREEN) =>
  createImageBitmap(new ImageData(rgba(w, h, f), w, h), { premultiplyAlpha: 'none', colorSpaceConversion: 'none' });
/** Draws a texture over the whole 64x64 canvas (no mips) and reads the top-left and bottom-right. */
async function drawTexture(r, tex) {
  const o = r.createObject(r.createMesh(quad()), r.createMaterial({ texture: tex }));
  r.frame();
  const read = { topLeft: pixel(r, -0.97, 0.97), centre: pixel(r, 0, 0), bottomRight: pixel(r, 0.97, -0.97) };
  r.destroyObject(o);
  return read;
}
const listeners = { lost: 0, restored: 0 };

// ---------------------------------------------------------------------------------------------
// LOST CONTEXTS

/** LC1: the LOST message's recovery, followed literally: dispose in onContextLost, then wait for onContextRestored. */
async function lc1() {
  const c = canvas();
  const d = { lostHook: 0, restoredHook: 0, pageRestoredEvent: 0 };
  c.addEventListener('webglcontextrestored', () => d.pageRestoredEvent++);
  const r = await createRenderer(c, {
    width: 16,
    height: 16,
    onContextLost: () => {
      d.lostHook++;
      r.dispose();
    },
    onContextRestored: () => d.restoredHook++,
  });
  const lc = r.gl.getExtension('WEBGL_lose_context');
  lc.loseContext();
  await timeout(50);
  d.restoreThrew = attempt(() => lc.restoreContext()).threw;
  await timeout(500);
  const again = await settle(createRenderer(c, { width: 16, height: 16 }));
  d.again = again.ok ? 'created' : again.message;
  if (again.ok) again.r.dispose();
  log(
    'LC1',
    'Following the LOST message literally ("dispose this renderer, then create a new one once the context is restored (onContextRestored)") reaches onContextRestored',
    d.lostHook === 1 && d.restoredHook === 1,
    d,
  );
}

/** LC2: a lost renderer left undisposed (allowed: a new one may be made beside it) can still resize the canvas the new renderer owns. */
async function lc2() {
  const c = canvas(32, 32);
  let restored;
  const restoredP = new Promise((res) => (restored = res));
  const old = await createRenderer(c, { width: 32, height: 32, antialias: false, onContextRestored: () => restored('restored') });
  const lc = old.gl.getExtension('WEBGL_lose_context');
  lc.loseContext();
  await timeout(50);
  lc.restoreContext();
  const d = { restored: await Promise.race([restoredP, timeout(2000)]) };
  const fresh = await createRenderer(c, { width: 32, height: 32, antialias: false });
  flat(fresh);
  fresh.createObject(fresh.createMesh(quad()), fresh.createMaterial({ baseColor: [0, 1, 0, 1] }));
  fresh.frame();
  d.before = { size: [c.width, c.height], topRight: pixel(fresh, 0.9, 0.9) };
  d.oldResize = attempt(() => old.resize(8, 8));
  d.oldSetClear = attempt(() => old.setClearColor(1, 0, 0));
  fresh.frame();
  d.after = { size: [c.width, c.height], topRight: pixel(fresh, 0.9, 0.9), glError: fresh.gl.getError() };
  old.dispose();
  fresh.dispose();
  log(
    'LC2',
    'A lost renderer cannot change the canvas that a new renderer (made beside it after the restore) now owns',
    d.oldResize.threw || (d.after.size[0] === 32 && d.after.size[1] === 32),
    d,
  );
}

/** LC3: a context lost during instantiation, whose event is dispatched before instantiation resolves. */
async function lc3() {
  const c = canvas();
  const gl = c.getContext('webgl2');
  const lose = gl.getExtension('WEBGL_lose_context');
  const real = WebAssembly.instantiate;
  WebAssembly.instantiate = async (...args) => {
    lose.loseContext();
    await timeout(30); // the lost event is dispatched now, while the create awaits
    return real.apply(WebAssembly, args);
  };
  const d = { hook: 0 };
  try {
    const first = await settle(createRenderer(c, { width: 16, height: 16, onContextLost: () => d.hook++ }));
    d.first = first.ok ? 'created' : first.message;
  } finally {
    WebAssembly.instantiate = real;
  }
  d.size = [c.width, c.height];
  const restoredP = new Promise((res) => c.addEventListener('webglcontextrestored', () => res('restored'), { once: true }));
  d.restoreThrew = attempt(() => lose.restoreContext()).threw;
  d.restored = await Promise.race([restoredP, timeout(2000)]);
  const again = await settle(createRenderer(c, { width: 16, height: 16 }));
  d.again = again.ok ? 'created' : again.message;
  if (again.ok) {
    flat(again.r);
    again.r.createObject(again.r.createMesh(quad()), again.r.createMaterial({ baseColor: [0, 1, 0, 1] }));
    again.r.frame();
    d.pixel = pixel(again.r, 0, 0);
    d.glError = again.r.gl.getError();
    again.r.dispose();
  }
  log(
    'LC3',
    'A loss whose event arrives while the module instantiates: the create says lost, the canvas is restorable and the next create draws',
    /context is lost/.test(d.first) && d.restored === 'restored' && d.again === 'created' && green(d.pixel) && d.glError === 0,
    d,
  );
  log(
    'LC3b',
    'A create that fails does not call the page\'s onContextLost hook (it has no renderer to dispose)',
    d.hook === 0,
    { hook: d.hook },
  );
}

/** LC4: a context lost during web3d_init (while the programs are checked); its event comes later. */
async function lc4() {
  const c = canvas();
  const gl = c.getContext('webgl2');
  const lose = gl.getExtension('WEBGL_lose_context');
  const real = WebAssembly.instantiate;
  let checks = 0;
  WebAssembly.instantiate = (module, imports) => {
    const g = imports.web3d_gl;
    const check = g.program_check;
    g.program_check = (p) => {
      if (checks++ === 1) lose.loseContext();
      return check(p);
    };
    return real.call(WebAssembly, module, imports);
  };
  const d = {};
  try {
    const first = await settle(createRenderer(c, { width: 16, height: 16 }));
    d.first = first.ok ? 'created' : first.message;
  } finally {
    WebAssembly.instantiate = real;
  }
  d.checks = checks;
  await timeout(50);
  const restoredP = new Promise((res) => c.addEventListener('webglcontextrestored', () => res('restored'), { once: true }));
  d.restoreThrew = attempt(() => lose.restoreContext()).threw;
  d.restored = await Promise.race([restoredP, timeout(2000)]);
  const again = await settle(createRenderer(c, { width: 16, height: 16 }));
  d.again = again.ok ? 'created' : again.message;
  if (again.ok) {
    flat(again.r);
    again.r.createObject(again.r.createMesh(quad()), again.r.createMaterial({ baseColor: [0, 1, 0, 1] }));
    again.r.frame();
    d.pixel = pixel(again.r, 0, 0);
    d.glError = again.r.gl.getError();
    again.r.dispose();
  }
  log(
    'LC4',
    'A loss during init: the create says lost (not a shader failure), the canvas is restorable, the next create draws with getError 0',
    /context is lost/.test(d.first) && d.restored === 'restored' && d.again === 'created' && green(d.pixel) && d.glError === 0,
    d,
  );
}

/** LC5: lost, then the live renderer disposed in the same task (its event still pending): the one-shot listener keeps it restorable. */
async function lc5() {
  const c = canvas();
  const r = await createRenderer(c, { width: 16, height: 16 });
  const lose = r.gl.getExtension('WEBGL_lose_context');
  const d = {};
  let defaultPrevented = null;
  lose.loseContext();
  r.dispose();
  // Registered after dispose(), so it runs after the shim's one-shot listener on the same event.
  c.addEventListener('webglcontextlost', (e) => (defaultPrevented = e.defaultPrevented), { once: true });
  const during = await settle(createRenderer(c, { width: 16, height: 16 }));
  d.createDuring = during.ok ? 'created' : during.message;
  await timeout(50);
  d.defaultPrevented = defaultPrevented;
  const restoredP = new Promise((res) => c.addEventListener('webglcontextrestored', () => res('restored'), { once: true }));
  d.restoreThrew = attempt(() => lose.restoreContext()).threw;
  d.restored = await Promise.race([restoredP, timeout(2000)]);
  // A second loss: the one-shot listener must be gone, so nothing prevents its default now.
  let second = null;
  const again = await settle(createRenderer(c, { width: 16, height: 16 }));
  d.again = again.ok ? 'created' : again.message;
  if (again.ok) {
    flat(again.r);
    again.r.createObject(again.r.createMesh(quad()), again.r.createMaterial({ baseColor: [0, 1, 0, 1] }));
    again.r.frame();
    d.pixel = pixel(again.r, 0, 0);
    d.glError = again.r.gl.getError();
    again.r.dispose();
    c.addEventListener('webglcontextlost', (e) => (second = e.defaultPrevented), { once: true });
    lose.loseContext();
    await timeout(50);
    d.secondLossPrevented = second; // the disposed renderer's shim, and the one-shot, are gone
  }
  log(
    'LC5',
    'A loss whose event is still pending when the renderer is disposed stays restorable (one preventDefault), and the one-shot listener does not outlive it',
    /context is lost/.test(d.createDuring) && d.defaultPrevented === true && d.restored === 'restored' && d.again === 'created' && green(d.pixel) && d.glError === 0 && d.secondLossPrevented === false,
    d,
  );
}

/** LC6: dispose while lost (before the restore), then restore and create: nothing sent, getError 0. */
async function lc6() {
  const c = canvas();
  const r = await createRenderer(c, { width: 16, height: 16, antialias: false });
  flat(r);
  r.createObject(r.createMesh(quad()), r.createMaterial({ texture: r.createTexture(await bitmap(4, 4)) }));
  r.frame();
  const lose = r.gl.getExtension('WEBGL_lose_context');
  lose.loseContext();
  await timeout(50);
  const d = {};
  d.frameWhileLost = attempt(() => r.frame());
  r.dispose();
  const restoredP = new Promise((res) => c.addEventListener('webglcontextrestored', () => res('restored'), { once: true }));
  lose.restoreContext();
  d.restored = await Promise.race([restoredP, timeout(2000)]);
  const gl = c.getContext('webgl2');
  d.errorAfterRestore = gl.getError();
  const again = await settle(createRenderer(c, { width: 16, height: 16, antialias: false }));
  d.again = again.ok ? 'created' : again.message;
  if (again.ok) {
    flat(again.r);
    again.r.createObject(again.r.createMesh(quad()), again.r.createMaterial({ baseColor: [0, 1, 0, 1] }));
    again.r.frame();
    d.pixel = pixel(again.r, 0, 0);
    d.glError = again.r.gl.getError();
    again.r.dispose();
  }
  log('LC6', 'Dispose while lost, then restore: a new renderer draws, getError 0', d.frameWhileLost.value === false && d.restored === 'restored' && d.again === 'created' && green(d.pixel) && d.glError === 0, d);
}

/** LC8: after a loss and restore, a new renderer's first texture is accepted (no CONTEXT_LOST_WEBGL left pending to misattribute). */
async function lc8() {
  const c = canvas(64, 64);
  const r = await createRenderer(c, { width: 64, height: 64, antialias: false });
  const lose = r.gl.getExtension('WEBGL_lose_context');
  const restoredP = new Promise((res) => c.addEventListener('webglcontextrestored', () => res('restored'), { once: true }));
  lose.loseContext();
  await timeout(50);
  lose.restoreContext();
  const d = { restored: await Promise.race([restoredP, timeout(2000)]) };
  r.dispose();
  const fresh = await createRenderer(c, { width: 64, height: 64, antialias: false });
  flat(fresh);
  const t = attempt(() => fresh.createTexture(await0, { mipmaps: false }));
  d.texture = t.threw ? t.message : 'created';
  if (!t.threw) d.draw = await drawTexture(fresh, t.value);
  d.glError = fresh.gl.getError();
  fresh.dispose();
  log('LC8', 'After a loss and restore, a new renderer\'s first texture is accepted and draws', d.texture === 'created' && green(d.draw?.centre) && d.glError === 0, d);
}

/** LC7: between loseContext() and its event: does frame() return false, as the docs say once a context is lost? */
async function lc7() {
  const c = canvas();
  const r = await createRenderer(c, { width: 16, height: 16, antialias: false });
  flat(r);
  r.createObject(r.createMesh(quad()), r.createMaterial({ baseColor: [0, 1, 0, 1] }));
  r.frame();
  const lose = r.gl.getExtension('WEBGL_lose_context');
  lose.loseContext();
  const d = { isContextLost: r.gl.isContextLost() };
  d.frame = attempt(() => r.frame()); // before anything reads r.lost, which latches the shim's flag
  d.lostGetter = r.lost;
  d.frameAfterGetter = attempt(() => r.frame());
  d.create = attempt(() => r.createMaterial({}));
  await timeout(50);
  d.frameAfterEvent = attempt(() => r.frame());
  r.dispose();
  log(
    'LC7',
    'Once the context is lost (isContextLost true, before the event), frame() returns false, as WEB3D.md says, consistent with r.lost and the creates',
    d.isContextLost && d.lostGetter && d.frame.value === false && d.create.threw,
    d,
  );
}

// ---------------------------------------------------------------------------------------------
// DEPTH

async function depth() {
  const d = {};
  const c1 = canvas();
  const before = [c1.width, c1.height];
  c1.getContext('webgl2', { depth: false });
  const a = await settle(createRenderer(c1, { width: 32, height: 32 }));
  const b = await settle(createRenderer(c1, { width: 32, height: 32 }));
  d.noDepth = [a.ok ? 'created' : a.message, b.ok ? 'created' : b.message];
  d.sizeKept = c1.width === before[0] && c1.height === before[1];
  const c2 = canvas();
  c2.getContext('webgl2', { depth: false, antialias: false });
  const e = await settle(createRenderer(c2, { width: 16, height: 16 }));
  const f = await settle(createRenderer(c2, { width: 16, height: 16, antialias: false }));
  d.noDepthNoAA = [e.ok ? 'created' : e.message, f.ok ? 'created' : f.message];
  const c3 = canvas();
  c3.getContext('webgl2', { depth: true, stencil: true, alpha: true, premultipliedAlpha: false, preserveDrawingBuffer: true });
  const g = await settle(createRenderer(c3, { width: 16, height: 16 }));
  d.otherAttributes = g.ok ? 'created' : g.message;
  if (g.ok) {
    flat(g.r);
    g.r.createObject(g.r.createMesh(quad(-2)), g.r.createMaterial({ baseColor: [0, 1, 0, 1] }));
    g.r.createObject(g.r.createMesh(quad(-8)), g.r.createMaterial({ baseColor: [1, 0, 0, 1] }));
    g.r.frame();
    d.otherPixel = pixel(g.r, 0, 0);
    d.otherAttrs = g.r.gl.getContextAttributes();
    g.r.dispose();
  }
  log(
    'D1',
    'A page-made context without depth is refused for depth, every time, with the size kept',
    d.noDepth.every((m) => /no depth buffer/.test(m)) && d.sizeKept,
    d,
  );
  log(
    'D2',
    'A page context with other attributes (stencil, alpha, unpremultiplied, preserved) but depth and matching antialias is accepted and draws near over far',
    d.otherAttributes === 'created' && green(d.otherPixel),
    { otherAttributes: d.otherAttributes, pixel: d.otherPixel, attrs: d.otherAttrs, noDepthNoAA: d.noDepthNoAA },
  );
}

// ---------------------------------------------------------------------------------------------
// TEXTURE SOURCES AND THE POST-UPLOAD getError

/** T1: an error left in a page-made context before web3d took it: is a good texture refused, and the page's error swallowed? */
async function t1() {
  const c = canvas(64, 64);
  const gl = c.getContext('webgl2', { antialias: false });
  gl.enable(0x1234); // INVALID_ENUM, left pending, as a page's own code might
  const r = await createRenderer(c, { width: 64, height: 64, antialias: false });
  flat(r);
  const d = {};
  d.first = attempt(() => r.createTexture(await0, { mipmaps: false }));
  d.errorAfterFirst = r.gl.getError();
  d.second = attempt(() => r.createTexture(await0, { mipmaps: false }));
  if (!d.second.threw) d.secondDraw = await drawTexture(r, d.second.value);
  r.dispose();
  log(
    'T1',
    'A good texture is not refused for a GL error that was already pending in the context (an error the page left), and that error is not swallowed',
    !d.first.threw,
    d,
  );
}
let await0;

/** T2: the post-upload check through the renderer's public gl (WEB3D: "for debugging (getError) only"): one pending error refuses the next texture. */
async function t2() {
  const c = canvas(64, 64);
  const r = await createRenderer(c, { width: 64, height: 64, antialias: false });
  flat(r);
  const d = {};
  r.gl.getParameter(0x1234); // INVALID_ENUM
  r.gl.bindBuffer(0x1234, null); // another INVALID_ENUM; Chrome keeps one flag per kind
  r.gl.drawArrays(0x1234, 0, 0); // INVALID_ENUM again
  r.gl.lineWidth(-1); // INVALID_VALUE
  d.a = attempt(() => r.createTexture(await0, { mipmaps: false }));
  d.b = attempt(() => r.createTexture(await0, { mipmaps: false }));
  d.c = attempt(() => r.createTexture(await0, { mipmaps: false }));
  d.after = r.gl.getError();
  r.dispose();
  log(
    'T2',
    'Pending GL errors from before the upload are not reported as the upload\'s',
    !d.a.threw && !d.b.threw,
    d,
  );
}

/** T3: canvases whose reported width differs from the bitmap WebGL uploads. */
async function t3() {
  const d = {};
  const c = canvas(64, 64);
  const r = await createRenderer(c, { width: 64, height: 64, antialias: false });
  flat(r);
  // (a) An HTMLCanvasElement with a bitmaprenderer context: width 300 x 150, bitmap 8 x 8.
  const br = document.createElement('canvas');
  br.getContext('bitmaprenderer').transferFromImageBitmap(await bitmap(8, 8));
  d.bitmaprenderer = { reported: [br.width, br.height] };
  const ta = attempt(() => r.createTexture(br, { mipmaps: false }));
  d.bitmaprenderer.create = ta.threw ? ta.message : 'created';
  d.bitmaprenderer.glError = r.gl.getError();
  if (!ta.threw) d.bitmaprenderer.draw = await drawTexture(r, ta.value);
  // (b) An OffscreenCanvas with a bitmaprenderer context.
  const obr = new OffscreenCanvas(64, 32);
  obr.getContext('bitmaprenderer').transferFromImageBitmap(await bitmap(8, 8));
  d.offscreenBitmaprenderer = { reported: [obr.width, obr.height] };
  const tb = attempt(() => r.createTexture(obr, { mipmaps: false }));
  d.offscreenBitmaprenderer.create = tb.threw ? tb.message : 'created';
  d.offscreenBitmaprenderer.glError = r.gl.getError();
  if (!tb.threw) d.offscreenBitmaprenderer.draw = await drawTexture(r, tb.value);
  // (c) A placeholder canvas whose OffscreenCanvas was resized to 8 x 8 and drawn green.
  const ph = document.createElement('canvas');
  document.body.append(ph);
  const off = ph.transferControlToOffscreen();
  off.width = 8;
  off.height = 8;
  const ctx = off.getContext('2d');
  ctx.fillStyle = '#00ff00';
  ctx.fillRect(0, 0, 8, 8);
  await frames(3);
  d.placeholder = { reported: [ph.width, ph.height] };
  const tc = attempt(() => r.createTexture(ph, { mipmaps: false }));
  d.placeholder.create = tc.threw ? tc.message : 'created';
  d.placeholder.glError = r.gl.getError();
  if (!tc.threw) d.placeholder.draw = await drawTexture(r, tc.value);
  // (d) A placeholder whose OffscreenCanvas is larger than the element reports.
  const ph2 = document.createElement('canvas');
  ph2.width = 4;
  ph2.height = 4;
  document.body.append(ph2);
  const off2 = ph2.transferControlToOffscreen();
  off2.width = 32;
  off2.height = 32;
  const ctx2 = off2.getContext('2d');
  ctx2.fillStyle = '#00ff00';
  ctx2.fillRect(0, 0, 32, 32);
  await frames(3);
  d.placeholderLarger = { reported: [ph2.width, ph2.height] };
  const td = attempt(() => r.createTexture(ph2, { mipmaps: false }));
  d.placeholderLarger.create = td.threw ? td.message : 'created';
  d.placeholderLarger.glError = r.gl.getError();
  if (!td.threw) d.placeholderLarger.draw = await drawTexture(r, td.value);
  r.dispose();
  const filled = (x) => x.create !== 'created' || (green(x.draw?.topLeft) && green(x.draw?.bottomRight));
  log(
    'T3',
    'Every canvas source is sized by what WebGL uploads (WEB3D: "sources whose upload size is exactly knowable"): a texture either fills with the source or is refused',
    filled(d.bitmaprenderer) && filled(d.offscreenBitmaprenderer) && filled(d.placeholder) && filled(d.placeholderLarger),
    d,
  );
}

/** T4: VideoFrames: a visibleRect with an offset, a rotated frame, and a flipped one. */
async function t4() {
  const d = {};
  const c = canvas(64, 64);
  const r = await createRenderer(c, { width: 64, height: 64, antialias: false });
  flat(r);
  // (a) coded 8x8, visible 4x4 at (2, 2): green inside, red outside.
  const data = rgba(8, 8, (x, y) => (x >= 2 && x < 6 && y >= 2 && y < 6 ? GREEN : RED));
  const f1 = new VideoFrame(data, { format: 'RGBA', codedWidth: 8, codedHeight: 8, visibleRect: { x: 2, y: 2, width: 4, height: 4 }, timestamp: 0 });
  const a = attempt(() => r.createTexture(f1, { mipmaps: false }));
  d.offset = { visible: [f1.visibleRect.width, f1.visibleRect.height], display: [f1.displayWidth, f1.displayHeight], create: a.threw ? a.message : 'created', glError: r.gl.getError() };
  if (!a.threw) d.offset.draw = await drawTexture(r, a.value);
  f1.close();
  // (b) coded 4x2, rotated 90 degrees: displayed 2x4.
  let f2;
  try {
    f2 = new VideoFrame(rgba(4, 2, () => GREEN), { format: 'RGBA', codedWidth: 4, codedHeight: 2, rotation: 90, timestamp: 0 });
  } catch (e) {
    d.rotated = { constructError: String(e) };
  }
  if (f2) {
    const b = attempt(() => r.createTexture(f2, { mipmaps: false }));
    d.rotated = { rotation: f2.rotation, visible: [f2.visibleRect.width, f2.visibleRect.height], display: [f2.displayWidth, f2.displayHeight], create: b.threw ? b.message : 'created', glError: r.gl.getError() };
    if (!b.threw) d.rotated.draw = await drawTexture(r, b.value);
    f2.close();
  }
  // (c) a closed frame.
  const f3 = new VideoFrame(rgba(4, 4, () => GREEN), { format: 'RGBA', codedWidth: 4, codedHeight: 4, timestamp: 0 });
  f3.close();
  const cc = attempt(() => r.createTexture(f3));
  d.closed = cc.threw ? cc.message : 'created';
  d.finalError = r.gl.getError();
  r.dispose();
  log(
    'T4',
    'A VideoFrame is sized by its visible rectangle and uploads exactly that region (offset rect, rotation), or is refused cleanly',
    d.offset.create === 'created' && green(d.offset.draw?.topLeft) && green(d.offset.draw?.bottomRight) &&
      (!d.rotated || d.rotated.constructError || d.rotated.create !== 'created' || (green(d.rotated.draw?.topLeft) && green(d.rotated.draw?.bottomRight))) &&
      d.closed !== 'created' && d.finalError === 0,
    d,
  );
}

/** T5: other sources: an <img> from another realm, a detached ImageData, an SVG image, a transferred OffscreenCanvas. */
async function t5() {
  const d = {};
  const c = canvas(64, 64);
  const r = await createRenderer(c, { width: 64, height: 64, antialias: false });
  flat(r);
  const ifr = document.createElement('iframe');
  document.body.append(ifr);
  const img = new ifr.contentWindow.Image();
  img.src = 'data:image/gif;base64,R0lGODlhAQABAIAAAP///wAAACH5BAEAAAAALAAAAAABAAEAAAICRAEAOw==';
  await img.decode();
  d.foreignImg = attempt(() => r.createTexture(img)).message ?? 'created';
  const id = new ImageData(rgba(4, 4, () => GREEN), 4, 4);
  structuredClone(id.data.buffer, { transfer: [id.data.buffer] });
  d.detachedImageData = { reported: [id.width, id.height] };
  const t = attempt(() => r.createTexture(id, { mipmaps: false }));
  d.detachedImageData.create = t.threw ? t.message : 'created';
  d.detachedImageData.glError = r.gl.getError();
  const svg = document.createElementNS('http://www.w3.org/2000/svg', 'image');
  d.svgImage = attempt(() => r.createTexture(svg)).message ?? 'created';
  const oc = new OffscreenCanvas(4, 4);
  structuredClone(oc, { transfer: [oc] });
  d.transferredOffscreen = attempt(() => r.createTexture(oc)).message ?? 'created';
  d.glError = r.gl.getError();
  ifr.remove();
  r.dispose();
  log(
    'T5',
    'An <img> from another realm, a detached ImageData, an SVG <image> and a transferred OffscreenCanvas are refused cleanly (no handle, getError 0)',
    /decode it first/.test(d.foreignImg) && d.detachedImageData.create !== 'created' && d.detachedImageData.glError === 0 && d.svgImage !== 'created' && d.transferredOffscreen !== 'created' && d.glError === 0,
    d,
  );
}

// ---------------------------------------------------------------------------------------------
// SERIALS ACROSS REALMS

const frameReport = (src) =>
  new Promise((res) => {
    const f = document.createElement('iframe');
    f.src = src;
    const onMessage = (e) => {
      if (e.source === f.contentWindow) {
        removeEventListener('message', onMessage);
        res({ frame: f, report: e.data });
      }
    };
    addEventListener('message', onMessage);
    document.body.append(f);
    setTimeout(() => res({ frame: f, report: 'timeout' }), 8000);
  });

async function realms() {
  const shared = window[KEY];
  const d = { firstSerialHere: window.__firstSerial, start: shared.serials.next, madeBefore: shared.serials.made };
  const ours = await createRenderer(canvas(), { width: 16, height: 16, antialias: false });
  flat(ours);
  const ourObject = ours.createObject(ours.createMesh(quad()), ours.createMaterial({ baseColor: [0, 1, 0, 1] }));
  d.ourSerial = ours._serial;
  // Same-origin frame.
  const same = await frameReport('/c/frame.html');
  d.sameOrigin = same.report;
  // Cross-origin frame, with a frame nested in it back at this origin.
  const foreign = window.__foreignOrigin;
  const cross = await frameReport(`${foreign}/c/frame.html?nest=${encodeURIComponent(`${location.origin}/c/frame.html`)}`);
  d.crossOrigin = cross.report;
  // A module worker.
  const w = new Worker('/c/worker.js', { type: 'module' });
  d.worker = await new Promise((res) => {
    w.onmessage = (e) => res(e.data);
    w.onerror = (e) => res({ error: String(e.message) });
    w.postMessage(0);
    setTimeout(() => res('timeout'), 8000);
  });
  w.terminate();
  // Each foreign handle on our renderer.
  const tryHandle = (h) => (typeof h === 'number' ? attempt(() => ours.setVisible(h, false)) : { skipped: true });
  d.sameOnOurs = tryHandle(d.sameOrigin.object);
  d.crossOnOurs = tryHandle(d.crossOrigin.object);
  d.nestedOnOurs = tryHandle(d.crossOrigin.nested?.object);
  d.workerOnOurs = tryHandle(d.worker.object);
  ours.frame();
  d.pixel = pixel(ours, 0, 0);
  d.madeAfter = shared.serials.made;
  ours.dispose();
  same.frame.contentWindow.__r?.dispose();
  same.frame.remove();
  cross.frame.remove();
  log(
    'S1',
    'A same-origin frame shares the top window\'s counter (its serial follows ours, one more renderer counted); a frame nested in a cross-origin frame, back at this origin, shares it too',
    d.sameOrigin.topShared === 'on top' && d.sameOrigin.serial === (d.ourSerial % (2 ** 20 - 1)) + 1 &&
      d.crossOrigin.nested?.topShared === 'on top' && d.crossOrigin.nested?.serial === (d.sameOrigin.serial % (2 ** 20 - 1)) + 1,
    { ourSerial: d.ourSerial, same: d.sameOrigin, nested: d.crossOrigin.nested, madeBefore: d.madeBefore, madeAfter: d.madeAfter },
  );
  log(
    'S2',
    'A cross-origin frame and a worker keep their own counters (random starts) and draw; every foreign handle is refused by our renderer, which still draws',
    d.crossOrigin.topShared === 'unreachable' && typeof d.crossOrigin.serial === 'number' && typeof d.worker.serial === 'number' && green(d.worker.pixel) &&
      d.sameOnOurs.threw && d.crossOnOurs.threw && d.nestedOnOurs.threw && d.workerOnOurs.threw && green(d.pixel),
    { cross: { serial: d.crossOrigin.serial, topShared: d.crossOrigin.topShared, object: d.crossOrigin.object }, worker: d.worker, refused: [d.sameOnOurs, d.crossOnOurs, d.nestedOnOurs, d.workerOnOurs].map((t) => t.threw), pixel: d.pixel },
  );
}

/** S3: the counter's end: made at its limit refuses (size kept, canvas free); next goes round from 2^20 - 1 to 1. */
async function counterEnd() {
  const pool = window[KEY].serials;
  const saved = { ...pool };
  const d = {};
  const c = canvas();
  try {
    pool.next = 2 ** 20 - 1;
    pool.made = 2 ** 20 - 3;
    const a = await createRenderer(c, { width: 16, height: 16, antialias: false });
    d.a = a._serial;
    a.dispose();
    const b = await createRenderer(c, { width: 16, height: 16, antialias: false });
    d.b = b._serial;
    b.dispose();
    d.madeNow = pool.made;
    const before = [c.width, c.height];
    const refused = await settle(createRenderer(c, { width: 32, height: 32, antialias: false }));
    d.refused = refused.ok ? 'created' : refused.message;
    d.sizeKept = c.width === before[0] && c.height === before[1];
    pool.made = 0;
    const again = await settle(createRenderer(c, { width: 16, height: 16, antialias: false }));
    d.afterReset = again.ok ? again.r._serial : again.message;
    if (again.ok) again.r.dispose();
  } finally {
    Object.assign(pool, saved);
  }
  log(
    'S3',
    'The counter goes round (2^20 - 1, then 1), refuses once it has made 2^20 - 1 renderers, and the refusal leaves the canvas usable',
    d.a === 2 ** 20 - 1 && d.b === 1 && /made 1048575 renderers/.test(d.refused) && d.sizeKept && d.afterReset === 2,
    d,
  );
}

// ---------------------------------------------------------------------------------------------
// HANDLES

async function handles() {
  const pool = window[KEY].serials;
  const saved = { ...pool };
  const d = {};
  pool.next = 2 ** 20 - 1;
  const c = canvas();
  const r = await createRenderer(c, { width: 16, height: 16, antialias: false });
  Object.assign(pool, saved, { made: saved.made + 1 });
  flat(r);
  const mesh = r.createMesh(quad());
  const mat = r.createMaterial({ baseColor: [0, 1, 0, 1] });
  const o = r.createObject(mesh, mat);
  const o2 = r.createObject(mesh, mat);
  const decode = (h) => ({ serial: Math.floor(h / 2 ** 30), kind: Math.floor(h / 2 ** 27) % 8, generation: Math.floor(h / 2 ** 16) % 2048, index: h % 2 ** 16 });
  d.serial = r._serial;
  d.o = decode(o);
  d.o2 = decode(o2);
  d.largestPossible = (2 ** 20 - 1) * 2 ** 30 + 4 * 2 ** 27 + 2047 * 2 ** 16 + 65535;
  d.largestBelow250 = d.largestPossible < 2 ** 50;
  const fracs = [0.5, -0.5, 0.25, -0.25, 0.125, -0.125, 0.375, 0.1, 0.9];
  d.fractions = fracs.map((f) => {
    const v = o + f;
    const t = attempt(() => r.setVisible(v, false));
    return { add: f, value: v, isWhole: Number.isInteger(v), refused: t.threw, msg: t.message };
  });
  // A fraction that rounds to the next whole number names o2: double rounding, not web3d's.
  d.roundsToNext = { add: 0.9375, equalsO2: o + 0.9375 === o2 };
  r.setVisible(o2, false);
  r.frame();
  d.pixel = pixel(r, 0, 0);
  // Raw export: fractions, -0, NaN, Infinity, 2^50, a negative, each straight into wasm.
  const raw = [o + 0.5, o - 0.125, -0, NaN, Infinity, 2 ** 50, 2 ** 50 + o, -o, o + 2 ** 30, o - 2 ** 27, o + 2 ** 16];
  d.raw = raw.map((v) => ({ v: String(v), status: r._x.web3d_object_set_visible(r._w, v, 0) }));
  r.frame();
  d.pixelAfterRaw = pixel(r, 0, 0);
  // Generation wrap: one material slot through all 2,048 generations.
  const issued = new Set();
  const first = r.createMaterial({});
  issued.add(first);
  r.destroyMaterial(first);
  let lastOnSlot;
  for (let i = 1; i < 2048; i++) {
    const m = r.createMaterial({});
    issued.add(m);
    lastOnSlot = m;
    r.destroyMaterial(m);
  }
  const next = r.createMaterial({});
  d.wrap = { first: decode(first), last: decode(lastOnSlot), next: decode(next), distinct: issued.size, nextIsNew: !issued.has(next) };
  d.wrap.firstRefused = attempt(() => r.destroyMaterial(first)).threw;
  d.wrap.lastRefused = attempt(() => r.destroyMaterial(lastOnSlot)).threw;
  r.destroyMaterial(next);
  // Exhaustion: 65,535 live objects, then one more.
  const t0 = performance.now();
  const objs = [];
  for (let i = objs.length; i < 65535 - 2; i++) objs.push(r.createObject(mesh, mat));
  const extra = attempt(() => r.createObject(mesh, mat));
  d.exhaust = { live: objs.length + 2, extra: extra.threw ? extra.message : 'created', ms: Math.round(performance.now() - t0) };
  for (const h of objs) r.setVisible(h, false);
  r.frame();
  d.exhaust.pixel = pixel(r, 0, 0);
  r.destroyObject(objs[100]);
  const reuse = r.createObject(mesh, mat);
  d.exhaust.reuse = decode(reuse);
  d.exhaust.oldRefused = attempt(() => r.setVisible(objs[100], true)).threw;
  d.glError = r.gl.getError();
  r.dispose();
  log(
    'H1',
    'At the largest serial, handles are whole numbers below 2^50; every fraction a double holds there (halves, quarters, eighths) is refused, by the wrapper and by the raw export; the object stays drawn',
    d.serial === 2 ** 20 - 1 && d.largestBelow250 && d.fractions.filter((f) => !f.isWhole).every((f) => f.refused) && d.raw.every((x) => x.status === 1) && green(d.pixel) && green(d.pixelAfterRaw),
    d,
  );
  log(
    'H2',
    'A slot serves 2,048 generations then retires; no handle is issued twice; 65,535 live values then a clean refusal; a freed slot is reused with a new generation; getError 0',
    d.wrap.distinct === 2048 && d.wrap.last.generation === 2047 && d.wrap.next.index !== d.wrap.first.index && d.wrap.nextIsNew && d.wrap.firstRefused && d.wrap.lastRefused &&
      /too many objects/.test(d.exhaust.extra) && green(d.exhaust.pixel) && d.exhaust.reuse.generation === 1 && d.exhaust.oldRefused && d.glError === 0,
    { wrap: d.wrap, exhaust: d.exhaust, glError: d.glError },
  );
}

// ---------------------------------------------------------------------------------------------
// THE wasm OPTION

async function wasmOption() {
  const d = {};
  const url = '/web3d/pkg/loadngo_web3d.wasm';
  const bytes = await (await fetch(url)).arrayBuffer();
  const ifr = document.createElement('iframe');
  document.body.append(ifr);
  const W = ifr.contentWindow;
  const tryWasm = async (name, wasm, draw = true) => {
    const c = canvas();
    const res = await settle(createRenderer(c, { width: 16, height: 16, antialias: false, wasm }));
    if (!res.ok) return (d[name] = res.message);
    if (draw) {
      flat(res.r);
      res.r.createObject(res.r.createMesh(quad()), res.r.createMaterial({ baseColor: [0, 1, 0, 1] }));
      res.r.frame();
      d[name] = green(pixel(res.r, 0, 0)) ? 'created, draws' : 'created, does not draw';
    } else d[name] = 'created';
    res.r.dispose();
  };
  await tryWasm('foreignModule', new W.WebAssembly.Module(new W.Uint8Array(bytes)));
  await tryWasm('foreignURL', new W.URL(url, location.href));
  await tryWasm('foreignResponse', await W.fetch(url));
  await tryWasm('foreignBytes', new W.Uint8Array(bytes));
  await tryWasm('foreignArrayBuffer', W.Uint8Array.from(new Uint8Array(bytes)).buffer);
  await tryWasm('dataView', new DataView(bytes));
  let toStringCalls = 0;
  await tryWasm('toStringObject', { toString: () => (toStringCalls++, url) });
  d.toStringCalls = toStringCalls;
  await tryWasm('stringObject', new String(url));
  await tryWasm('proxyResponse', new Proxy(await fetch(url), {}));
  await tryWasm('promise', fetch(url));
  await tryWasm('blob', new Blob([bytes]));
  await tryWasm('symbol', Symbol('x'));
  await tryWasm('number', 7);
  const usedResponse = await fetch(url);
  await usedResponse.arrayBuffer();
  await tryWasm('usedResponse', usedResponse);
  const lying = new Response('nope', { status: 404 });
  Object.defineProperty(lying, 'ok', { get: () => true });
  await tryWasm('lyingOkResponse', lying);
  ifr.remove();
  log(
    'W1',
    'The wasm option takes another realm\'s Module, URL, Response, bytes and ArrayBuffer, and draws',
    ['foreignModule', 'foreignURL', 'foreignResponse', 'foreignBytes', 'foreignArrayBuffer', 'dataView'].every((k) => d[k] === 'created, draws'),
    d,
  );
  log(
    'W2',
    'Anything else is refused before it is used (no toString runs): an object with toString, a String object, a Proxy over a Response, a Promise, a Blob, a symbol, a number',
    ['toStringObject', 'stringObject', 'proxyResponse', 'promise', 'blob', 'symbol', 'number'].every((k) => /wasm option must be/.test(d[k])) && d.toStringCalls === 0,
    { toStringObject: d.toStringObject, stringObject: d.stringObject, proxyResponse: d.proxyResponse, promise: d.promise, blob: d.blob, symbol: d.symbol, number: d.number, toStringCalls: d.toStringCalls, usedResponse: d.usedResponse, lyingOkResponse: d.lyingOkResponse },
  );
}

async function main() {
  const first = await createRenderer(canvas(), { width: 16, height: 16, antialias: false });
  window.__firstSerial = first._serial;
  first.dispose();
  await0 = await bitmap(8, 8);
  for (const p of [lc1, lc2, lc3, lc4, lc5, lc6, lc7, lc8, depth, t1, t2, t3, t4, t5, realms, counterEnd, handles, wasmOption]) {
    try {
      await p();
    } catch (e) {
      log(p.name, 'probe ran to the end', false, { error: String(e?.stack ?? e) });
    }
  }
  out.firstSerial = window.__firstSerial;
  out.passed = out.probes.filter((t) => t.pass).length;
  out.failed = out.probes.filter((t) => !t.pass).map((t) => t.id);
}

window.__probe6.done = main().then(() => out, (e) => ({ ...out, error: String(e?.stack ?? e) }));
