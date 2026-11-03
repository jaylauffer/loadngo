// Sixth critic's probes for web3d phase 1 (loadngo web3d-phase1 at 59b74ced). Each probe states the
// claim it tests; `pass` means the claim held. Results in window.__probe6.done.
//   G1-G3  the 53-bit handle layout, and forgery through the wrapper and straight into the exports
//   G4     other renderers' handles, live and disposed, after many create/dispose cycles
//   G5     the page-wide serial across module copies, and a re-imported module
//   G6     the serial across realms: a same-origin iframe's own copy of the module
//   G7     Array matrices, vectors and colours: each element and the length read once, no valueOf
//   G8     texture sizes the wrapper reads against what WebGL uploads
//   G9     context settings: a page-made context without depth; a create during a context loss
//   G10    a failed create when the context is lost during instantiation
//   G11    the per-URL compile cache, counted at the server
//   G12    a slot through every generation: retirement, none issued twice
//   G13    many live renderers at once (the old 32-renderer limit's intent)
// BSD-3-Clause, as loadngo.

import * as web3d from '/web3d/pkg/web3d.js';

const { createRenderer } = web3d;
const out = { probes: [] };
window.__probe6 = { done: null, out };
const log = (id, claim, pass, detail) => out.probes.push({ id, claim, pass: Boolean(pass), detail });

const N = 0.1;
const F = 10;
const ORTHO = new Float32Array([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, -2 / (F - N), 0, 0, 0, -(F + N) / (F - N), 1]);
const IDENT = new Float32Array([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1]);
const PI = Math.PI;
function quad(x0, y0, x1, y1, z) {
  return {
    positions: new Float32Array([x0, y0, z, x1, y0, z, x1, y1, z, x0, y1, z]),
    normals: new Float32Array([0, 0, 1, 0, 0, 1, 0, 0, 1, 0, 0, 1]),
    uvs: new Float32Array([0, 1, 1, 1, 1, 0, 0, 0]),
    indices: new Uint32Array([0, 1, 2, 0, 2, 3]),
  };
}
const attempt = (fn) => {
  try {
    return { threw: false, value: fn() };
  } catch (e) {
    return { threw: true, message: String(e?.message ?? e) };
  }
};
const settle = (p) => p.then((r) => ({ ok: true, r }), (e) => ({ ok: false, message: String(e?.message ?? e) }));
const wait = (ms) => new Promise((res) => setTimeout(res, ms));
const timeout = (ms) => new Promise((res) => setTimeout(() => res('timeout'), ms));
const reader = (r) => {
  const px = new Uint8Array(4);
  return (nx, ny) => {
    const { width, height } = r.canvas;
    r.gl.readPixels(Math.floor(((nx + 1) / 2) * width), Math.floor(((ny + 1) / 2) * height), 1, 1, r.gl.RGBA, r.gl.UNSIGNED_BYTE, px);
    return [px[0], px[1], px[2], px[3]];
  };
};
const flat = (r) => {
  r.setCamera(IDENT, ORTHO);
  r.setKeyLight(0, 0, 1, 0, 0, 0, 0);
  r.setAmbient(1, 1, 1, 1, 1, 1, PI);
  r.setClearColor(0, 0, 0);
};
const canvas = (w = 16, h = 16) => {
  const c = document.createElement('canvas');
  c.width = w;
  c.height = h;
  document.body.appendChild(c);
  return c;
};
/** A handle's fields, by the documented layout serial << 33 | kind << 30 | generation << 16 | index + 1. */
const decode = (h) => {
  const b = BigInt(h);
  return { serial: Number(b >> 33n), kind: Number((b >> 30n) & 7n), generation: Number((b >> 16n) & 0x3fffn), index: Number(b & 0xffffn) };
};
const green = (p) => p[1] > 200 && p[0] < 60;

/** G6 first, so the top page's first renderer has serial 1, as the frame's first does. */
async function g6Realms() {
  const c = canvas();
  const top = await createRenderer(c, { width: 16, height: 16, antialias: false });
  flat(top);
  const mesh = top.createMesh(quad(-1, -1, 1, 1, -5));
  const material = top.createMaterial({ baseColor: [0, 1, 0, 1] });
  const object = top.createObject(mesh, material);
  const frame = document.getElementById('frame').contentWindow;
  for (let i = 0; i < 100 && !frame.frameReady; i++) await wait(50);
  const theirs = await frame.makeWithSerial(top._serial);
  const d = { topSerial: top._serial, frameSerial: theirs.serial, top: { mesh, material, object }, frame: theirs };
  d.sameNumber = theirs.object === object;
  // The frame's object handle, given to the top renderer.
  d.hideWithTheirs = attempt(() => top.setVisible(theirs.object, false));
  top.frame();
  d.pixelAfter = reader(top)(0, 0);
  top.setVisible(object, true);
  d.destroyWithTheirMaterial = attempt(() => top.destroyMaterial(theirs.material));
  frame.kept.dispose();
  top.dispose();
  log(
    'G6',
    'WEB3D.md "no handle is issued twice in a page"; the serial counter is "shared by every copy of the module": a same-origin iframe\'s renderer must not issue a handle the top renderer accepts',
    !d.sameNumber && d.hideWithTheirs.threw,
    d,
  );
}

async function g1Layout() {
  const r = await createRenderer(canvas(), { width: 16, height: 16, antialias: false });
  const mesh = r.createMesh(quad(-1, -1, 1, 1, -5));
  const tex = r.createTexture(new ImageData(new Uint8ClampedArray(16 * 4).fill(255), 4, 4));
  const material = r.createMaterial({ texture: tex });
  const object = r.createObject(mesh, material);
  const handles = { mesh, tex, material, object };
  const d = { serial: r._serial, decoded: Object.fromEntries(Object.entries(handles).map(([k, h]) => [k, decode(h)])), handles };
  const kinds = { mesh: 1, tex: 2, material: 3, object: 4 };
  const ok = Object.entries(handles).every(([k, h]) => {
    const f = decode(h);
    return Number.isSafeInteger(h) && h < 2 ** 53 && f.serial === r._serial && f.kind === kinds[k] && f.generation === 0 && f.index === 1;
  });
  r.dispose();
  log('G1', 'slots.rs: a handle is serial << 33 | kind << 30 | generation << 16 | (index + 1), an exact double below 2^53', ok, d);
}

async function g2g3Forgery() {
  const r = await createRenderer(canvas(), { width: 16, height: 16, antialias: false });
  flat(r);
  const mesh = r.createMesh(quad(-1, -1, 1, 1, -5));
  const material = r.createMaterial({ baseColor: [0, 1, 0, 1] });
  const o = r.createObject(mesh, material);
  // A second object at slot 2, so index + 1 is a live object's index only when the rest matches.
  const o2 = r.createObject(mesh, material, new Float32Array([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 50, 0, 0, 1]));
  const ulp = 2 ** (Math.floor(Math.log2(o)) - 52);
  const values = {
    'o + 0.5': o + 0.5, 'o - 0.5': o - 0.5, 'o + ulp (a fraction a double holds here)': o + ulp, 'o - ulp': o - ulp,
    '-o': -o, '-0': -0, '0': 0, NaN: NaN, Infinity: Infinity, '-Infinity': -Infinity,
    '2^53': 2 ** 53, '2^53 + 2': 2 ** 53 + 2, 'o + 2^53': o + 2 ** 53, '2o': 2 * o, '1e308': 1e308, '2^-1074': 2 ** -1074,
    'MAX_SAFE_INTEGER': Number.MAX_SAFE_INTEGER, 'o + 2^33 (next serial)': o + 2 ** 33, 'o - 2^33 (previous serial)': o - 2 ** 33,
    'o + 2^30 (kind 5)': o + 2 ** 30, 'o + 2^31 (kind 6)': o + 2 ** 31, 'o + 3 * 2^30 (kind 7)': o + 3 * 2 ** 30, 'o - 4 * 2^30 (kind 0)': o - 4 * 2 ** 30,
    'o - 2^30 (a material handle)': o - 2 ** 30, 'mesh handle': mesh, 'o + 2^16 (next generation)': o + 2 ** 16, 'o - 1 (index 0)': o - 1,
    'o + 0xfffe (index 65535)': o + 0xfffe, 'o2 + 2^16': o2 + 2 ** 16, 'fround(o)': Math.fround(o), 'String(o)': String(o),
    'new Number(o)': new Number(o), 'BigInt(o)': BigInt(o), '[o]': [o], '{ valueOf }': { valueOf: () => o },
  };
  const wrapper = {};
  for (const [k, v] of Object.entries(values)) {
    if (typeof v === 'number' && v === o && !Object.is(v, o)) continue;
    wrapper[k] = attempt(() => r.setVisible(v, false));
    if (!wrapper[k].threw) r.setVisible(o, true);
  }
  wrapper['fround(o) equals o'] = Math.fround(o) === o;
  r.frame();
  const pixel = reader(r)(0, 0);
  const glError = r.gl.getError();
  const wrapperOk = Object.entries(wrapper).every(([k, t]) => typeof t === 'boolean' || t.threw) && green(pixel) && glError === 0;
  log('G2', 'WEB3D.md "Handles must be whole numbers from 1 to 2^53-1 ... anything else [is refused] by the Rust side": every forged value is refused and the object stays drawn', wrapperOk, { o, ulp, wrapper, pixel, glError });

  // Straight into the exports, behind the wrapper (its private fields are out of the documented
  // scope, but the Rust side's own refusal is claimed).
  const x = r._x;
  const w = r._w;
  const raw = {};
  for (const [k, v] of Object.entries(values)) {
    if (typeof v !== 'number') continue;
    raw[k] = x.web3d_object_set_visible(w, v, 0);
    if (raw[k] === 0) x.web3d_object_set_visible(w, o, 1);
  }
  raw.control = x.web3d_object_set_visible(w, o, 1);
  const rawMaterial = {
    'texture NaN': x.web3d_material_create(w, NaN, 0, 0),
    'texture 0.5': x.web3d_material_create(w, 0.5, 0, 0),
    'texture -0 (read as none)': x.web3d_material_create(w, -0, 0, 0),
    'object of mesh + 0.5': x.web3d_object_create(w, mesh + 0.5, material),
  };
  r.frame();
  const pixel2 = reader(r)(0, 0);
  const rawOk = Object.entries(raw).every(([k, s]) => (k === 'control' ? s === 0 : s === 1)) &&
    rawMaterial['texture NaN'] === 0 && rawMaterial['texture 0.5'] === 0 && rawMaterial['object of mesh + 0.5'] === 0 && green(pixel2) && r.gl.getError() === 0;
  r.dispose();
  log('G3', 'web.rs handle_in: a raw export refuses every number that is not a whole number from 1 to below 2^53 naming a live handle', rawOk, { raw, rawMaterial, pixel2 });
}

async function g4OtherRenderers() {
  const ca = canvas();
  const cb = canvas();
  const make = async (c, colour) => {
    const r = await createRenderer(c, { width: 16, height: 16, antialias: false });
    flat(r);
    const mesh = r.createMesh(quad(-1, -1, 1, 1, -5));
    const material = r.createMaterial({ baseColor: colour });
    return { r, mesh, material, object: r.createObject(mesh, material) };
  };
  const a = await make(ca, [0, 1, 0, 1]);
  const b = await make(cb, [0, 1, 0, 1]);
  const d = { a: [a.mesh, a.material, a.object], b: [b.mesh, b.material, b.object] };
  d.aOnB = [attempt(() => b.r.setVisible(a.object, false)), attempt(() => b.r.destroyMaterial(a.material)), attempt(() => b.r.createObject(a.mesh, b.material))];
  d.bOnA = [attempt(() => a.r.setVisible(b.object, false)), attempt(() => a.r.destroyMesh(b.mesh))];
  // 300 renderers come and go on a third canvas; every handle they issued is then tried on a and b.
  const cc = canvas();
  const seen = new Set();
  let twice = 0;
  const keep = (h) => {
    if (seen.has(h)) twice++;
    seen.add(h);
    return h;
  };
  const t0 = performance.now();
  for (let i = 0; i < 300; i++) {
    const r = await createRenderer(cc, { width: 8, height: 8, antialias: false });
    const mesh = keep(r.createMesh(quad(-1, -1, 1, 1, -5)));
    const mat = keep(r.createMaterial({}));
    r.destroyObject(keep(r.createObject(mesh, mat)));
    keep(r.createObject(mesh, mat));
    r.dispose();
  }
  d.churnMs = Math.round(performance.now() - t0);
  let accepted = 0;
  const acceptedSamples = [];
  for (const h of seen) {
    for (const [name, r] of [['a', a.r], ['b', b.r]]) {
      for (const call of [() => r.setVisible(h, true), () => r.destroyMesh(h), () => r.destroyMaterial(h), () => r.destroyTexture(h)]) {
        const t = attempt(call);
        if (!t.threw) {
          accepted++;
          if (acceptedSamples.length < 5) acceptedSamples.push({ h, name });
        }
      }
    }
  }
  a.r.frame();
  const pa = reader(a.r)(0, 0);
  b.r.frame();
  const pb = reader(b.r)(0, 0);
  d.churnHandles = seen.size;
  d.twice = twice;
  d.accepted = accepted;
  d.acceptedSamples = acceptedSamples;
  d.pixels = [pa, pb];
  d.serials = [a.r._serial, b.r._serial];
  a.r.dispose();
  b.r.dispose();
  const refusedAll = [...d.aOnB, ...d.bOnA].every((t) => t.threw && /another renderer|not a valid/.test(t.message));
  log(
    'G4',
    'WEB3D.md "another renderer\'s handle, live or disposed, is refused"; "no two renderers in a page issue the same handle": two live renderers refuse each other\'s handles, and 1,200 handles from 300 disposed renderers are issued once each and refused by both',
    refusedAll && twice === 0 && seen.size === 1200 && accepted === 0 && green(pa) && green(pb),
    d,
  );
}

async function g5ModuleCopies() {
  const A = await import('/web3d/pkg/web3d.js?copy=a');
  const B = await import('/web3d/pkg/web3d.js?copy=b');
  const c1 = canvas();
  const c2 = canvas();
  const c3 = canvas();
  const ra = await A.createRenderer(c1, { width: 16, height: 16, antialias: false });
  const rb = await B.createRenderer(c2, { width: 16, height: 16, antialias: false });
  const d = { serials: [ra._serial, rb._serial], copiesDiffer: A.Renderer !== B.Renderer && A.Renderer !== web3d.Renderer };
  const ma = ra.createMesh(quad(-1, -1, 1, 1, -5));
  const mb = rb.createMesh(quad(-1, -1, 1, 1, -5));
  d.handles = [ma, mb];
  d.crossMesh = attempt(() => rb.destroyMesh(ma));
  d.sameCanvasOtherCopy = await settle(B.createRenderer(c1, { width: 16, height: 16, antialias: false }));
  // A canvas whose context copy A made without antialias, then asked for with it through copy B.
  const r3 = await A.createRenderer(c3, { width: 16, height: 16, antialias: false });
  r3.dispose();
  d.otherCopyMismatch = await settle(B.createRenderer(c3, { width: 16, height: 16, antialias: true }));
  ra.dispose();
  rb.dispose();
  // A module imported again later (as a dev server's reload would): its serials carry on.
  const C = await import('/web3d/pkg/web3d.js?copy=c');
  const rc = await C.createRenderer(canvas(), { width: 16, height: 16, antialias: false });
  d.reloadedSerial = rc._serial;
  rc.dispose();
  d.sameCanvasOtherCopy = d.sameCanvasOtherCopy.ok ? 'created' : d.sameCanvasOtherCopy.message;
  d.otherCopyMismatch = d.otherCopyMismatch.ok ? 'created' : d.otherCopyMismatch.message;
  log(
    'G5',
    'WEB3D.md: the serial comes from "a page-wide counter, shared by every copy of the module, that never repeats"; canvas ownership and context settings are shared too',
    d.copiesDiffer && d.serials[0] !== d.serials[1] && d.crossMesh.threw && /another renderer/.test(d.crossMesh.message) &&
      /already has a live renderer/.test(d.sameCanvasOtherCopy) && /antialias false/.test(d.otherCopyMismatch) && d.reloadedSerial > Math.max(...d.serials),
    d,
  );
}

async function g7Arrays() {
  const r = await createRenderer(canvas(), { width: 16, height: 16, antialias: false });
  flat(r);
  const mesh = r.createMesh(quad(-1, -1, 1, 1, -5));
  const material = r.createMaterial({ baseColor: [0, 1, 0, 1] });
  const o = r.createObject(mesh, material);
  const d = {};
  // Every property read of a Proxy over a plain Array.
  const reads = [];
  const spy = new Proxy(Array.from(IDENT), { get(t, k, rcv) { reads.push(String(k)); return Reflect.get(t, k, rcv); } });
  d.proxy = attempt(() => r.setTransform(o, spy));
  d.proxyReads = reads.slice();
  const expected = ['length', ...Array.from({ length: 16 }, (_, i) => String(i))];
  d.proxyReadsExact = reads.length === 17 && expected.every((k, i) => reads[i] === k);
  // An accessor element, read once.
  const acc = Array.from(IDENT);
  let accReads = 0;
  Object.defineProperty(acc, 5, { get: () => (accReads++, 1), enumerable: true });
  d.accessor = attempt(() => r.setTransform(o, acc));
  d.accessorReads = accReads;
  // A hole.
  const holey = Array.from(IDENT);
  delete holey[7];
  d.hole = attempt(() => r.setTransform(o, holey));
  // Accessors that call the renderer, or throw.
  const calling = (fn) => {
    const a = Array.from(IDENT);
    Object.defineProperty(a, 3, { get: () => (fn(), 0) });
    return a;
  };
  d.elementCallsFrame = attempt(() => r.setTransform(o, calling(() => r.frame())));
  d.elementDisposes = attempt(() => r.setTransform(o, calling(() => r.dispose())));
  d.liveAfterDispose = Boolean(r._w);
  d.elementThrows = attempt(() => r.setTransform(o, calling(() => { throw new Error('boom'); })));
  d.usableAfterThrow = attempt(() => r.setTransform(o, IDENT));
  // Non-finite and out-of-f32-range numbers are numbers: the Rust side must refuse them.
  const withAt = (i, v) => { const a = Array.from(IDENT); a[i] = v; return a; };
  d.nanElement = attempt(() => r.setTransform(o, withAt(12, NaN)));
  d.bigElement = attempt(() => r.setTransform(o, withAt(12, 1e39)));
  d.lookAtNaN = attempt(() => r.setCameraLookAt([0, 0, NaN], [0, 0, 0], 0.8, 0.1, 100));
  d.colourNaN = attempt(() => r.createMaterial({ baseColor: [NaN, 0, 0, 1] }));
  d.colourHuge = attempt(() => r.createMaterial({ baseColor: [1e39, 0, 0, 1] }));
  d.keyHuge = attempt(() => r.setKeyLight(0, 1, 0, 1e39, 1, 1, 1));
  d.ambientNaN = attempt(() => r.setAmbient(NaN, 1, 1, 1, 1, 1, 1));
  d.clearInf = attempt(() => r.setClearColor(Infinity, 0, 0));
  // Typed arrays of other kinds.
  d.float64 = attempt(() => r.setTransform(o, new Float64Array(IDENT)));
  d.int32 = attempt(() => r.setTransform(o, new Int32Array(IDENT)));
  d.bigint = attempt(() => r.setTransform(o, new BigInt64Array(16)));
  const detached = new Float32Array(IDENT);
  structuredClone(detached.buffer, { transfer: [detached.buffer] });
  d.detached = attempt(() => r.setTransform(o, detached));
  d.arrayLike = attempt(() => r.setTransform(o, { length: 16, ...Array.from(IDENT) }));
  // A colour as a Proxy over an Array, and a mesh descriptor as a Proxy: each read once.
  const colourReads = [];
  const colour = new Proxy([0, 1, 0, 1], { get(t, k, rcv) { colourReads.push(String(k)); return Reflect.get(t, k, rcv); } });
  d.colourProxy = attempt(() => r.createMaterial({ baseColor: colour }));
  d.colourReads = colourReads;
  const fieldReads = [];
  const q = quad(-1, -1, 1, 1, -5);
  const targetReads = [];
  const targets = new Proxy([new Float32Array(12)], { get(t, k, rcv) { targetReads.push(String(k)); return Reflect.get(t, k, rcv); } });
  const desc = new Proxy({ ...q, morphTargets: targets }, { get(t, k, rcv) { fieldReads.push(String(k)); return Reflect.get(t, k, rcv); } });
  d.meshProxy = attempt(() => r.createMesh(desc));
  d.fieldReads = fieldReads;
  d.targetReads = targetReads;
  r.setTransform(o, IDENT);
  r.setCamera(IDENT, ORTHO);
  r.frame();
  d.pixel = reader(r)(0, 0);
  d.glError = r.gl.getError();
  r.dispose();
  const num = (t) => t.threw && /must be a number/.test(t.message);
  const refused = (t) => t.threw;
  const reentrant = (t) => t.threw && /while the renderer was reading/.test(t.message);
  const counts = (list) => list.every((k) => list.filter((x) => x === k).length === 1);
  log(
    'G7',
    'WEB3D.md "an Array\'s length and each of its elements is read once"; "no valueOf runs"; re-entrancy refused; non-finite values refused by the Rust side',
    !d.proxy.threw && d.proxyReadsExact && !d.accessor.threw && d.accessorReads === 1 && num(d.hole) && reentrant(d.elementCallsFrame) &&
      reentrant(d.elementDisposes) && d.liveAfterDispose && d.elementThrows.threw && /boom/.test(d.elementThrows.message) && !d.usableAfterThrow.threw &&
      refused(d.nanElement) && refused(d.bigElement) && refused(d.lookAtNaN) && refused(d.colourNaN) && refused(d.colourHuge) && refused(d.keyHuge) &&
      refused(d.ambientNaN) && refused(d.clearInf) && !d.float64.threw && !d.int32.threw && refused(d.bigint) && refused(d.detached) && refused(d.arrayLike) &&
      !d.colourProxy.threw && counts(colourReads) && !d.meshProxy.threw && counts(fieldReads) && counts(targetReads) && green(d.pixel) && d.glError === 0,
    d,
  );
}

/** Four quadrants: top-left red, top-right green, bottom-left blue, bottom-right white. */
function quadrants(n) {
  const c = document.createElement('canvas');
  c.width = n;
  c.height = n;
  const g = c.getContext('2d');
  const h = n / 2;
  [['#ff0000', 0, 0], ['#00ff00', h, 0], ['#0000ff', 0, h], ['#ffffff', h, h]].forEach(([s, x, y]) => {
    g.fillStyle = s;
    g.fillRect(x, y, h, h);
  });
  return c;
}

/** Draws a texture on a full-canvas quad and reads the four quadrants' centres: TL, TR, BL, BR. */
async function sampleTexture(source, label) {
  const r = await createRenderer(canvas(32, 32), { width: 32, height: 32, antialias: false });
  flat(r);
  const res = { label };
  const t = attempt(() => r.createTexture(source, { mipmaps: false, srgb: false }));
  res.create = t.threw ? t.message : 'created';
  res.errorAfterCreate = r.gl.getError();
  if (!t.threw) {
    r.createObject(r.createMesh(quad(-1, -1, 1, 1, -5)), r.createMaterial({ texture: t.value }));
    r.frame();
    const read = reader(r);
    res.quadrants = [read(-0.5, 0.5), read(0.5, 0.5), read(-0.5, -0.5), read(0.5, -0.5)].map((p) => p.slice(0, 3));
    res.errorAfterFrame = r.gl.getError();
  }
  r.dispose();
  return res;
}
const looksRight = (q) =>
  q && q[0][0] > 200 && q[0][1] < 60 && q[1][1] > 200 && q[1][0] < 60 && q[2][2] > 200 && q[2][0] < 60 && q[3].every((v) => v > 200);

async function g8TextureSizes() {
  const d = {};
  // An image whose srcset says 2x: naturalWidth is half the bitmap's width.
  const url = quadrants(8).toDataURL('image/png');
  const img = new Image();
  img.srcset = `${url} 2x`;
  await img.decode();
  d.srcset = { naturalWidth: img.naturalWidth, naturalHeight: img.naturalHeight, ...(await sampleTexture(img, 'img srcset 2x, 8x8 bitmap')) };
  const plain = new Image();
  plain.src = url;
  await plain.decode();
  d.plainImage = { naturalWidth: plain.naturalWidth, ...(await sampleTexture(plain, 'img 8x8')) };
  // VideoFrames whose display size differs from their coded size.
  if (typeof VideoFrame === 'function') {
    const big = new VideoFrame(quadrants(4), { timestamp: 0, displayWidth: 8, displayHeight: 8 });
    d.videoFrameLarger = { displayWidth: big.displayWidth, codedWidth: big.codedWidth, ...(await sampleTexture(big, 'VideoFrame coded 4x4, display 8x8')) };
    big.close();
    const small = new VideoFrame(quadrants(4), { timestamp: 0, displayWidth: 2, displayHeight: 2 });
    d.videoFrameSmaller = { displayWidth: small.displayWidth, codedWidth: small.codedWidth, ...(await sampleTexture(small, 'VideoFrame coded 4x4, display 2x2')) };
    small.close();
    const same = new VideoFrame(quadrants(4), { timestamp: 0 });
    d.videoFrame = await sampleTexture(same, 'VideoFrame 4x4');
    same.close();
  }
  const closed = await createImageBitmap(quadrants(4));
  closed.close();
  d.closedBitmap = await sampleTexture(closed, 'closed ImageBitmap');
  const broken = new Image();
  broken.src = 'data:,';
  await new Promise((res) => { broken.onerror = res; broken.onload = res; });
  d.brokenImage = await sampleTexture(broken, 'broken image');
  d.imageData = await sampleTexture(quadrants(4).getContext('2d').getImageData(0, 0, 4, 4), 'ImageData 4x4');
  // A cross-origin image without CORS: WebGL refuses it.
  const foreign = new Image();
  foreign.src = `${window.__foreignOrigin}/quadrants.png`;
  const loaded = await new Promise((res) => { foreign.onload = () => res(true); foreign.onerror = () => res(false); });
  d.crossOrigin = loaded ? await sampleTexture(foreign, 'cross-origin image, no CORS') : 'did not load';
  const ok = looksRight(d.plainImage.quadrants) && looksRight(d.imageData.quadrants) && /allows 1 to/.test(d.closedBitmap.create) && /allows 1 to/.test(d.brokenImage.create) &&
    (d.crossOrigin === 'did not load' || (/refused the texture upload/.test(d.crossOrigin.create) && d.crossOrigin.errorAfterCreate === 0));
  const srcsetOk = d.srcset.errorAfterCreate === 0 && (d.srcset.create !== 'created' || looksRight(d.srcset.quadrants));
  const vfOk = !d.videoFrameLarger || ([d.videoFrameLarger, d.videoFrameSmaller].every((v) => v.errorAfterCreate === 0 && (v.create !== 'created' || looksRight(v.quadrants))));
  log('G8a', 'createTexture: plain images, ImageData, closed bitmaps, broken and cross-origin images are uploaded or refused cleanly (no GL error)', ok, d);
  log('G8b', 'web3d.js header and WEB3D.md: a texture is made at the size the wrapper reads, so the upload fits it; an <img> with a srcset density: naturalWidth against the bitmap WebGL uploads', srcsetOk, d.srcset);
  log('G8c', 'the same for a VideoFrame whose display size is not its coded size (displayWidth is what the wrapper reads)', vfOk, { larger: d.videoFrameLarger, smaller: d.videoFrameSmaller, same: d.videoFrame });
}

async function g9ContextSettings() {
  const d = {};
  // A page-made context without a depth buffer, antialias matching.
  const c = canvas(16, 16);
  const gl = c.getContext('webgl2', { antialias: false, depth: false });
  d.pageDepth = gl.getContextAttributes().depth;
  const made = await settle(createRenderer(c, { width: 16, height: 16, antialias: false }));
  d.noDepth = made.ok ? 'created' : made.message;
  if (made.ok) {
    const r = made.r;
    flat(r);
    // Near green at z = -2, far red at z = -5, same pixels: the near one must win.
    r.createObject(r.createMesh(quad(-1, -1, 1, 1, -5)), r.createMaterial({ baseColor: [1, 0, 0, 1] }));
    r.createObject(r.createMesh(quad(-1, -1, 1, 1, -2)), r.createMaterial({ baseColor: [0, 1, 0, 1] }));
    r.frame();
    d.noDepthPixel = reader(r)(0, 0);
    d.noDepthGlError = r.gl.getError();
    d.rendererDepth = r.gl.getContextAttributes().depth;
    r.dispose();
  }
  // Other kinds of context the page made first.
  for (const kind of ['webgl', '2d']) {
    const k = canvas(20, 10);
    k.getContext(kind);
    const first = await settle(createRenderer(k, { width: 48, height: 24, antialias: false }));
    const second = await settle(createRenderer(k, { width: 48, height: 24, antialias: false }));
    d[kind] = { first: first.ok ? 'created' : first.message, second: second.ok ? 'created' : second.message, size: [k.width, k.height] };
  }
  log(
    'G9a',
    'WEB3D.md (A frame, step 2 and Shading): opaque draws rely on a depth buffer; a page-made context without one should be refused like one with the wrong antialias, or the depth test silently does nothing',
    d.noDepth !== 'created' || (d.noDepthPixel && green(d.noDepthPixel)),
    d,
  );
  log('G9b', 'a page-made WebGL1 or 2d context is refused with a clear message, the canvas is not left claimed, and its size is kept', ['webgl', '2d'].every((k) => /another kind of context/.test(d[k].first) && /another kind of context/.test(d[k].second) && d[k].size[0] === 20 && d[k].size[1] === 10), { webgl: d.webgl, '2d': d['2d'] });

  // A create during a context loss, with the default antialias (true), then the documented recovery.
  const e = {};
  const lc = canvas(32, 32);
  let lostRes;
  let restoredRes;
  const lost = new Promise((res) => (lostRes = res));
  const restored = new Promise((res) => (restoredRes = res));
  const old = await createRenderer(lc, { width: 32, height: 32, onContextLost: () => lostRes('lost'), onContextRestored: () => restoredRes('restored') });
  e.oldAntialias = old.info.antialias;
  const lose = old.gl.getExtension('WEBGL_lose_context');
  lose.loseContext();
  e.lostEvent = await Promise.race([lost, timeout(3000)]);
  await wait(0);
  e.attributesWhileLost = old.gl.getContextAttributes();
  const during = await settle(createRenderer(lc, { width: 32, height: 32 }));
  e.createDuringLoss = during.ok ? 'created' : during.message;
  if (during.ok) during.r.dispose();
  lose.restoreContext();
  e.restoredEvent = await Promise.race([restored, timeout(3000)]);
  old.dispose();
  e.attributesAfterRestore = old.gl.getContextAttributes()?.antialias;
  const after = await settle(createRenderer(lc, { width: 32, height: 32 }));
  e.createAfterRestore = after.ok ? 'created' : after.message;
  if (after.ok) after.r.dispose();
  const again = await settle(createRenderer(lc, { width: 32, height: 32, antialias: false }));
  e.thenAskingFalse = again.ok ? 'created' : again.message;
  if (again.ok) again.r.dispose();
  const third = await settle(createRenderer(lc, { width: 32, height: 32 }));
  e.thenAskingTrue = third.ok ? 'created' : third.message;
  if (third.ok) third.r.dispose();
  log(
    'G9c',
    'WEB3D.md Context loss: "every create throws" while lost, and "create a new one after onContextRestored": with the default antialias, a create during the loss must say the context is lost, and the documented recovery must then work',
    /context is lost/.test(e.createDuringLoss) && e.createAfterRestore === 'created',
    e,
  );
}

async function g10LostDuringInstantiate() {
  const c = canvas(20, 10);
  const gl = c.getContext('webgl2', { antialias: false });
  const lose = gl.getExtension('WEBGL_lose_context');
  let hookCalls = 0;
  const module = await web3d.compile();
  // Simulates a GPU reset between the shim's loss check and init (a test hook on a built-in).
  const real = WebAssembly.instantiate;
  WebAssembly.instantiate = async (...args) => {
    const result = await real.apply(WebAssembly, args);
    lose.loseContext();
    return result;
  };
  const d = {};
  try {
    const res = await settle(createRenderer(c, { width: 48, height: 24, antialias: false, wasm: module, onContextLost: () => hookCalls++ }));
    d.result = res.ok ? 'created' : res.message;
  } finally {
    WebAssembly.instantiate = real;
  }
  d.size = [c.width, c.height];
  await wait(50);
  d.hookCallsAfterRealLoss = hookCalls;
  c.dispatchEvent(new Event('webglcontextlost'));
  d.hookCallsAfterSyntheticEvent = hookCalls;
  lose.restoreContext();
  await wait(200);
  const later = await settle(createRenderer(c, { width: 16, height: 16, antialias: false }));
  d.laterCreate = later.ok ? 'created' : later.message;
  if (later.ok) later.r.dispose();
  log(
    'G10',
    'WEB3D.md "A failed create leaves nothing behind ... removes the shim\'s context-loss listeners, releases the canvas and puts its size back", for a context lost during instantiation; the error should say the context is lost',
    d.result !== 'created' && d.size[0] === 20 && d.size[1] === 10 && d.hookCallsAfterSyntheticEvent === 0 && d.laterCreate === 'created',
    d,
  );
  d.errorSaysLost = /context is lost/.test(d.result);
}

async function g11CompileCache() {
  const C = await import('/web3d/pkg/web3d.js?copy=cache');
  const counts = async () => (await fetch('/count/n')).json();
  const d = { before: await counts() };
  const c = canvas();
  const tryWith = async (wasm) => {
    const r = await settle(C.createRenderer(c, { width: 8, height: 8, antialias: false, wasm }));
    if (r.ok) r.r.dispose();
    return r.ok ? 'created' : r.message;
  };
  d.first = await tryWith('/count/mod.wasm');
  d.second = await tryWith('/count/mod.wasm');
  d.sameHref = await tryWith(new URL('/count/../count/mod.wasm', location.href));
  d.garbage1 = await tryWith('/count/garbage.wasm');
  d.garbage2 = await tryWith('/count/garbage.wasm');
  let toStringCalls = 0;
  d.object = await tryWith({ toString: () => (toStringCalls++, '/count/obj.wasm') });
  d.toStringCalls = toStringCalls;
  const response = await fetch('/count/resp.wasm');
  d.response1 = await tryWith(response);
  d.response2 = await tryWith(response);
  d.after = await counts();
  log(
    'G11',
    'WEB3D.md "The compiled module is cached per URL, and a failure is not cached": one fetch for a URL used three times (one spelt differently); a module that fails to compile is fetched again',
    d.first === 'created' && d.second === 'created' && d.sameHref === 'created' && d.after['/count/mod.wasm'] === 1 && d.garbage1 !== 'created' && d.garbage2 !== 'created' && d.after['/count/garbage.wasm'] === 2,
    d,
  );
}

async function g12Generations() {
  const r = await createRenderer(canvas(), { width: 16, height: 16, antialias: false });
  flat(r);
  const mesh = r.createMesh(quad(-1, -1, 1, 1, -5));
  const material = r.createMaterial({ baseColor: [0, 1, 0, 1] });
  const kept = r.createObject(mesh, material);
  const seen = new Set();
  const byIndex = {};
  let twice = 0;
  let first = 0;
  let lastOfSlot2 = 0;
  const t0 = performance.now();
  for (let i = 0; i < 16384 + 16; i++) {
    const h = r.createObject(mesh, material, new Float32Array([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 50, 0, 0, 1]));
    if (seen.has(h)) twice++;
    seen.add(h);
    const f = decode(h);
    byIndex[f.index] = (byIndex[f.index] ?? 0) + 1;
    if (i === 0) first = h;
    if (f.index === 2) lastOfSlot2 = h;
    r.destroyObject(h);
  }
  const d = { ms: Math.round(performance.now() - t0), issued: seen.size, twice, byIndex, lastOfSlot2: decode(lastOfSlot2) };
  d.firstRefused = attempt(() => r.setVisible(first, true));
  d.lastOfSlot2Refused = attempt(() => r.setVisible(lastOfSlot2, true));
  r.frame();
  d.pixel = reader(r)(0, 0);
  d.keptLive = !attempt(() => r.setVisible(kept, true)).threw;
  r.dispose();
  log(
    'G12',
    'slots.rs: "Each slot serves 16,384 values before it retires"; no handle is issued twice; a retired slot\'s handles stay refused',
    twice === 0 && byIndex[2] === 16384 && byIndex[3] === 16 && d.lastOfSlot2.generation === 16383 && d.firstRefused.threw && d.lastOfSlot2Refused.threw && green(d.pixel) && d.keptLive,
    d,
  );
}

async function g13ManyLive() {
  const d = { created: 0, lost: [], errors: [] };
  const rs = [];
  const cs = [];
  for (let i = 0; i < 24; i++) {
    const c = canvas(8, 8);
    cs.push(c);
    const res = await settle(createRenderer(c, { width: 8, height: 8, antialias: false }));
    if (res.ok) {
      d.created++;
      rs.push(res.r);
      flat(res.r);
      res.r.createObject(res.r.createMesh(quad(-1, -1, 1, 1, -5)), res.r.createMaterial({ baseColor: [0, 1, 0, 1] }));
    } else d.errors.push(res.message);
  }
  await wait(300);
  rs.forEach((r, i) => {
    if (r.lost) d.lost.push(i);
  });
  d.drawnLive = rs.filter((r) => !r.lost).map((r) => {
    r.frame();
    return green(reader(r)(0, 0)) && r.gl.getError() === 0;
  });
  // A create on a canvas whose context Chrome dropped, with the default antialias (true).
  const firstLost = d.lost[0];
  if (firstLost !== undefined) {
    let hook = 0;
    const res = await settle(createRenderer(cs[firstLost], { width: 8, height: 8, onContextLost: () => hook++ }));
    d.createOnDroppedDefault = res.ok ? 'created' : res.message;
    const res2 = await settle(createRenderer(cs[firstLost], { width: 8, height: 8, antialias: false, onContextLost: () => hook++ }));
    d.createOnDroppedFalse = res2.ok ? 'created' : res2.message;
    cs[firstLost].dispatchEvent(new Event('webglcontextlost'));
    d.hookAfterFailedCreates = hook;
  }
  rs.forEach((r) => r.dispose());
  const fresh = await settle(createRenderer(canvas(8, 8), { width: 8, height: 8, antialias: false }));
  d.freshAfter = fresh.ok ? 'created' : fresh.message;
  if (fresh.ok) fresh.r.dispose();
  log(
    'G13',
    'no limit of live renderers but the browser\'s: 24 live renderers are all created; the live ones draw; failed creates on a dropped context leave no listener; a new canvas works afterwards',
    d.created === 24 && d.drawnLive.every(Boolean) && (d.hookAfterFailedCreates ?? 0) === 0 && d.freshAfter === 'created',
    d,
  );
}

async function main() {
  const probes = [g6Realms, g1Layout, g2g3Forgery, g4OtherRenderers, g5ModuleCopies, g7Arrays, g8TextureSizes, g9ContextSettings, g10LostDuringInstantiate, g11CompileCache, g12Generations, g13ManyLive];
  for (const p of probes) {
    try {
      await p();
    } catch (e) {
      log(p.name, 'the probe ran to the end', false, { error: String(e?.stack ?? e) });
    }
  }
  out.passed = out.probes.filter((t) => t.pass).length;
  out.failed = out.probes.filter((t) => !t.pass).map((t) => t.id);
}

window.__probe6.done = main().then(() => out);
