// Fifth critic's GPU probes for web3d phase 1 (generic; nothing application-specific). Groups are
// chosen with ?group=: main (conversion, Array re-reads, salt reuse, staging, texture routes,
// failed-create cleanup, compile cache, handle rounding, foreign contexts), heap (a heap past
// 2 GiB), exhaust (a handle table's whole life), cap (33 renderers). window.__p5 resolves to
// { tests: [{ name, pass, detail }] }; `pass` means the documented claim held.
import { createRenderer } from '/web3d/pkg/web3d.js';

const out = { tests: [] };
const log = (name, pass, detail) => out.tests.push({ name, pass: Boolean(pass), detail });
const attempt = (fn) => {
  try {
    const value = fn();
    return { threw: false, value };
  } catch (e) {
    return { threw: true, message: String(e?.message ?? e) };
  }
};
const settle = (p) => p.then((r) => ({ ok: true, r }), (e) => ({ ok: false, message: String(e?.message ?? e) }));
const tick = (ms = 0) => new Promise((res) => setTimeout(res, ms));
const N = 0.1;
const F = 10;
const ORTHO = new Float32Array([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, -2 / (F - N), 0, 0, 0, -(F + N) / (F - N), 1]);
const IDENT = new Float32Array([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1]);
const quad = (x0, y0, x1, y1, z) => ({
  positions: new Float32Array([x0, y0, z, x1, y0, z, x1, y1, z, x0, y1, z]),
  normals: new Float32Array([0, 0, 1, 0, 0, 1, 0, 0, 1, 0, 0, 1]),
  uvs: new Float32Array([0, 1, 1, 1, 1, 0, 0, 0]),
  indices: new Uint32Array([0, 1, 2, 0, 2, 3]),
});
const canvas = (w = 16, h = 16) => {
  const c = document.createElement('canvas');
  c.width = w;
  c.height = h;
  document.body.appendChild(c);
  return c;
};
const reader = (r) => {
  const px = new Uint8Array(4);
  return (nx, ny) => {
    const c = r.canvas;
    r.gl.readPixels(Math.floor(((nx + 1) / 2) * (c.width - 1)), Math.floor(((ny + 1) / 2) * (c.height - 1)), 1, 1, r.gl.RGBA, r.gl.UNSIGNED_BYTE, px);
    return [px[0], px[1], px[2]];
  };
};
const flat = (r) => {
  r.setCamera(IDENT, ORTHO);
  r.setKeyLight(0, 0, 1, 0, 0, 0, 0);
  r.setAmbient(1, 1, 1, 1, 1, 1, Math.PI);
  r.setClearColor(0, 0, 0);
};
const small = () => createRenderer(canvas(), { width: 16, height: 16, antialias: false });

/** C1: "Numbers must be numbers. Nothing is converted, so no valueOf runs." Matrices and vectors as Arrays. */
async function conversion() {
  const r = await small();
  flat(r);
  const mesh = r.createMesh(quad(-1, -1, 1, 1, -5));
  const mat = r.createMaterial({ baseColor: [0, 1, 0, 1] });
  const d = {};
  d.createObjectWithStringMatrix = attempt(() => r.createObject(mesh, mat, Array.from(IDENT, String)));
  let calls = 0;
  const o = r.createObject(mesh, mat);
  d.setTransformWithValueOfElements = attempt(() => r.setTransform(o, Array.from(IDENT, (v) => ({ valueOf: () => (calls++, v) }))));
  d.valueOfCalls = calls;
  d.lookAtWithStrings = attempt(() => r.setCameraLookAt(['0', '0', '5'], ['0', '0', '0'], 0.8, 0.1, 100));
  d.setCameraWithBooleans = attempt(() => r.setCamera(Array.from(IDENT, (v) => v === 1), ORTHO));
  d.setCameraWithNulls = attempt(() => r.setCamera(Array.from(IDENT, (v) => (v === 0 ? null : v)), ORTHO));
  r.frame();
  d.pixel = reader(r)(0, 0);
  d.glError = r.gl.getError();
  r.dispose();
  const refused = (t) => t.threw;
  log(
    'C1 non-numbers inside a matrix or vector Array are refused and no valueOf runs (WEB3D.md "Nothing is converted, so no valueOf runs")',
    refused(d.createObjectWithStringMatrix) && refused(d.setTransformWithValueOfElements) && d.valueOfCalls === 0 &&
      refused(d.lookAtWithStrings) && refused(d.setCameraWithBooleans) && refused(d.setCameraWithNulls),
    d,
  );
}

/** C2: "A caller's object is read once": an Array's length, through a Proxy. */
async function arrayRereads() {
  const r = await small();
  flat(r);
  const o = r.createObject(r.createMesh(quad(-1, -1, 1, 1, -5)), r.createMaterial({ baseColor: [0, 1, 0, 1] }));
  const d = {};
  let reads = 0;
  const lying = new Proxy(Array.from(IDENT), {
    get(t, k, rcv) {
      if (k === 'length') return ++reads === 1 ? 16 : 64;
      return Reflect.get(t, k, rcv);
    },
  });
  d.setTransform = attempt(() => r.setTransform(o, lying));
  d.lengthReadsSetTransform = reads;
  reads = 0;
  d.setCameraProjection = attempt(() => r.setCamera(IDENT, lying));
  d.lengthReadsProjection = reads;
  d.later = attempt(() => (r.setCamera(IDENT, ORTHO), r.frame()));
  d.pixel = reader(r)(0, 0);
  r.dispose();
  log(
    'C2 an Array argument\'s length is read once (WEB3D.md "Each field ... is read into a local before anything is checked or staged")',
    d.lengthReadsSetTransform === 1 && d.lengthReadsProjection === 1,
    d,
  );
}

/** H5: a disposed renderer's handle, used on a later renderer that got the same salt. */
async function saltReuse() {
  const c = canvas();
  const opts = { width: 16, height: 16, antialias: false };
  const a = await createRenderer(c, opts);
  flat(a);
  const oA = a.createObject(a.createMesh(quad(-1, -1, 1, 1, -5)), a.createMaterial({ baseColor: [1, 0, 0, 1] }));
  const d = { saltA: a._salt };
  a.dispose();
  d.between = [];
  for (let i = 0; i < 31; i++) {
    const t = await createRenderer(c, opts);
    d.between.push(t._salt);
    t.dispose();
  }
  const b = await createRenderer(c, opts);
  d.saltB = b._salt;
  flat(b);
  const oB = b.createObject(b.createMesh(quad(-1, -1, 1, 1, -5)), b.createMaterial({ baseColor: [0, 1, 0, 1] }));
  d.handles = { oA, oB, same: oA === oB };
  d.staleHandleOnB = attempt(() => b.setVisible(oA, false));
  b.frame();
  d.pixelAfter = reader(b)(0, 0);
  b.dispose();
  log(
    'H5 another (disposed) renderer\'s handle is refused, also once its salt is reused (WEB3D.md "another renderer\'s handle, is refused"; "never reissued")',
    d.staleHandleOnB.threw && d.pixelAfter[1] > 200,
    d,
  );
}

/** R1: the Rust staging check, straight through the exports. */
async function staging() {
  const r = await small();
  flat(r);
  const x = r._x;
  const w = r._w;
  const err = () => r._errorText();
  const d = {};
  const p = x.web3d_alloc(36) >>> 0;
  new Float32Array(x.memory.buffer, p, 9).set([-1, -1, -5, 1, -1, -5, -1, 1, -5]);
  const i = x.web3d_alloc(12) >>> 0;
  new Uint32Array(x.memory.buffer, i, 3).set([0, 1, 2]);
  const call = (pp, pl, ip, il) => {
    const h = x.web3d_mesh_create(w, pp, pl, 0, 0, 0, 0, ip, il, 0, 0, 0, 0, 0, 0, 0);
    return h ? { handle: h } : { refused: err() };
  };
  d.valid = call(p, 9, i, 3);
  d.oneFloatPastTheEnd = call(p, 10, i, 3);
  d.misaligned = call(p + 1, 8, i, 3);
  d.ioArea = call(r._ioPtr, 9, i, 3);
  d.lengthOverflow = call(p, 0x40000001, i, 3);
  d.indicesFromThePositionsAllocation = call(p, 9, p, 3);
  x.web3d_free(p, 35);
  d.afterWrongLengthFree = call(p, 9, i, 3);
  x.web3d_free(p, 36);
  d.afterFree = call(p, 9, i, 3);
  x.web3d_free(p, 36); // a second free of the same block: ignored
  x.web3d_free(0, 0);
  x.web3d_free(0xfffffff0, 16);
  const p2 = x.web3d_alloc(36) >>> 0;
  d.realloc = { p, p2, sameBlock: p2 === p, zeroed: Array.from(new Float32Array(x.memory.buffer, p2, 9)).every((v) => v === 0) };
  d.hugeAlloc = x.web3d_alloc(0xffffffff) >>> 0;
  x.web3d_free(p2, 36);
  x.web3d_free(i, 12);
  d.publicCreateAfter = attempt(() => r.createObject(r.createMesh(quad(-1, -1, 1, 1, -5)), r.createMaterial({ baseColor: [0, 1, 0, 1] })));
  r.frame();
  d.pixel = reader(r)(0, 0);
  d.glError = r.gl.getError();
  r.dispose();
  log(
    'R1 Rust refuses staged arrays that run past, are misaligned, lie outside staging or were freed; frees of anything else are ignored',
    d.valid.handle && d.oneFloatPastTheEnd.refused && d.misaligned.refused && d.ioArea.refused && d.lengthOverflow.refused &&
      d.afterWrongLengthFree.handle && d.afterFree.refused && d.realloc.zeroed && d.hugeAlloc === 0 && !d.publicCreateAfter.threw &&
      d.pixel[1] > 200 && d.glError === 0,
    d,
  );
}

/** R2: errors thrown midway through createMesh leave no staging allocation behind. */
async function stagingLeak() {
  const r = await small();
  const x = r._x;
  const big = new Float32Array(3 * 87382); // about 1 MiB of positions
  const bad = { positions: big, indices: new Uint32Array([0, 1, 2]), morphTargets: [new Float32Array(big.length), new Float32Array(3)] };
  const before = x.memory.buffer.byteLength;
  let thrown = 0;
  for (let k = 0; k < 300; k++) {
    try {
      r.createMesh(bad);
    } catch {
      thrown++;
    }
  }
  const after = x.memory.buffer.byteLength;
  const d = { thrown, memoryBefore: before, memoryAfter: after, frame: attempt(() => r.frame()) };
  r.dispose();
  log('R2 300 creates that throw after staging (about 5 MiB each) leave memory where it was', thrown === 300 && after - before < 64 << 20 && !d.frame.threw, d);
}

/** T1: texture uploads: options getters that call in, throw, or resize the source after its size was read. */
async function textureRoutes() {
  const r = await small();
  flat(r);
  const src = document.createElement('canvas');
  src.width = 4;
  src.height = 4;
  const g = src.getContext('2d');
  g.fillStyle = '#00ff00';
  g.fillRect(0, 0, 4, 4);
  const d = {};
  d.getterCallsFrame = attempt(() => r.createTexture(src, { get srgb() { r.frame(); return true; } }));
  d.getterDisposes = attempt(() => r.createTexture(src, { get mipmaps() { r.dispose(); return true; } }));
  d.getterThrows = attempt(() => r.createTexture(src, { get srgb() { throw new Error('boom'); } }));
  d.afterThrow = attempt(() => r.frame());
  d.glErrorBefore = r.gl.getError();
  // The size is read first, then the options; a getter grows the source in between.
  d.resizedInGetter = attempt(() => r.createTexture(src, { get wrapS() { src.width = 64; src.height = 64; return 'clamp'; } }));
  d.glErrorAfterResize = r.gl.getError();
  r.dispose();
  const reentrant = (t) => t.threw && /while the renderer was reading/.test(t.message);
  log('T1 a texture option getter that calls the renderer is refused, and one that throws leaves it usable', reentrant(d.getterCallsFrame) && reentrant(d.getterDisposes) && d.getterThrows.threw && d.afterThrow.value === true, d);
  log(
    'T2 a texture option getter that resizes the source after its size was read: refused, or no GL error (WEB3D.md "read once ... before anything is checked")',
    d.resizedInGetter.threw || d.glErrorAfterResize === 0,
    d,
  );
}

/** L5: a flaky network: the wasm fetch fails once, then succeeds; two creates racing on a failure. */
async function flaky() {
  const d = {};
  const url = '/flaky/once.wasm';
  d.first = await settle(createRenderer(canvas(), { width: 8, height: 8, antialias: false, wasm: url }));
  d.second = await settle(createRenderer(canvas(), { width: 8, height: 8, antialias: false, wasm: url }));
  if (d.second.ok) d.second.r.dispose();
  const race = '/flaky/race.wasm';
  const [a, b] = await Promise.all([
    settle(createRenderer(canvas(), { width: 8, height: 8, antialias: false, wasm: race })),
    settle(createRenderer(canvas(), { width: 8, height: 8, antialias: false, wasm: race })),
  ]);
  d.raceA = a.ok ? 'created' : a.message;
  d.raceB = b.ok ? 'created' : b.message;
  if (a.ok) a.r.dispose();
  if (b.ok) b.r.dispose();
  d.third = await settle(createRenderer(canvas(), { width: 8, height: 8, antialias: false, wasm: race }));
  if (d.third.ok) d.third.r.dispose();
  d.serverRequests = await (await fetch('/flaky/count')).json();
  for (const k of ['first', 'second', 'third']) d[k] = d[k].ok ? 'created' : d[k].message;
  log('L5 one failed fetch, then the same URL loads; two creates sharing one failed fetch both fail and the next one loads', d.first !== 'created' && d.second === 'created' && d.raceA !== 'created' && d.raceB !== 'created' && d.third === 'created', d);
}

/** Y5: a canvas whose WebGL2 context the page made first, with other settings. */
async function foreignContext() {
  const c = canvas();
  const gl = c.getContext('webgl2', { antialias: false });
  const d = { pageContextAntialias: gl.getContextAttributes().antialias };
  const r = await settle(createRenderer(c, { width: 16, height: 16, antialias: true }));
  d.create = r.ok ? 'created' : r.message;
  if (r.ok) {
    d.rendererInfoAntialias = r.r.info.antialias;
    d.samples = r.r.info.samples;
    r.r.dispose();
  }
  log(
    'Y5 a renderer that asks for antialias on a canvas whose context lacks it is refused (WEB3D.md "must ask for the same, or is refused")',
    d.create !== 'created' || d.rendererInfoAntialias === true,
    d,
  );
}

/** X8: does a fraction survive on a handle? Depends on the handle's size, not the check. Run first in its page (salt 0). */
async function rounding() {
  const r = await small();
  flat(r);
  const used = r.createMesh(quad(-1, -1, 1, 1, -5));
  const o = r.createObject(used, r.createMaterial({}));
  const spare = r.createMesh(quad(-1, -1, 1, 1, -5));
  const d = {
    salt: r._salt,
    spare,
    spareBelow2To23: spare < 2 ** 23,
    sparePlusTinyRepresentable: spare + 2 ** -30 !== spare,
    destroySparePlusTiny: attempt(() => r.destroyMesh(spare + 2 ** -30)),
    object: o,
    objectPlusTinyIsObject: o + 2 ** -30 === o,
    smallestObjectHandle: (4 << 22) | 1,
    setVisibleObjectPlusHalf: attempt(() => r.setVisible(o + 0.5, false)),
    x8Value: 385875969 + 2 ** -30 === 385875969,
  };
  d.spareStillLive = !attempt(() => r.destroyMesh(spare)).threw;
  r.dispose();
  log(
    'X8 a representable fraction on a handle is refused; o + 2^-30 is o itself for every object handle (2^24 and up)',
    d.spareBelow2To23 && d.sparePlusTinyRepresentable && d.destroySparePlusTiny.threw && d.spareStillLive && d.objectPlusTinyIsObject && d.setVisibleObjectPlusHalf.threw && d.x8Value,
    d,
  );
}

/** B1: the heap past 2 GiB ("Pointers and lengths are read unsigned"). */
async function bigHeap() {
  const r = await small();
  flat(r);
  const x = r._x;
  const d = {};
  const big = x.web3d_alloc(0x7fff0000) >>> 0;
  d.big = big;
  d.memoryBytes = x.memory.buffer.byteLength;
  d.create = attempt(() => r.createObject(r.createMesh(quad(-1, -1, 1, 1, -5)), r.createMaterial({ baseColor: [0, 1, 0, 1] })));
  const probe = x.web3d_alloc(64) >>> 0;
  d.nextStagingAt = probe;
  x.web3d_free(probe, 64);
  d.frame = attempt(() => r.frame());
  d.pixel = reader(r)(0, 0);
  d.errorText = (() => {
    const h = x.web3d_mesh_create(r._w, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0);
    return h === 0 ? r._errorText() : `handle ${h}`;
  })();
  d.glError = r.gl.getError();
  if (big) x.web3d_free(big, 0x7fff0000);
  r.dispose();
  log('B1 with the heap past 2 GiB, a mesh staged above 2 GiB draws', big !== 0 && d.nextStagingAt >= 2 ** 31 && !d.create.threw && d.pixel[1] > 200 && d.glError === 0, d);
}

/** E1: a handle table's whole life: about 4.2 million handles, never one twice, then a clean refusal. */
async function exhaust() {
  const r = await small();
  flat(r);
  const mesh = r.createMesh(quad(-1, -1, 1, 1, -5));
  const mat = r.createMaterial({ baseColor: [0, 1, 0, 1] });
  const keep = r.createObject(mesh, mat);
  const d = { keep };
  const first = r.createObject(mesh, mat);
  r.destroyObject(first);
  let issued = 2;
  let last = first;
  let mid = 0;
  let error = null;
  const seenLow = new Set(); // every handle of the first 20 slots: each must be new
  const t0 = performance.now();
  try {
    for (;;) {
      const h = r.createObject(mesh, mat);
      issued++;
      if ((h & 0x7fff) <= 20) {
        if (seenLow.has(h)) throw new Error(`handle ${h} issued twice`);
        seenLow.add(h);
      }
      if (issued === 2_000_000) mid = h;
      last = h;
      r.destroyObject(h);
    }
  } catch (e) {
    error = String(e?.message ?? e);
  }
  d.ms = Math.round(performance.now() - t0);
  d.issued = issued;
  d.error = error;
  d.lowSlotHandles = seenLow.size;
  d.firstRefused = attempt(() => r.setVisible(first, false)).threw;
  d.midRefused = attempt(() => r.setVisible(mid, false)).threw;
  d.lastRefused = attempt(() => r.setVisible(last, false)).threw;
  d.keepStillWorks = !attempt(() => r.setVisible(keep, true)).threw;
  d.frame = attempt(() => r.frame());
  d.pixel = reader(r)(0, 0);
  d.otherKindsStillWork = !attempt(() => r.createMaterial({})).threw;
  d.glError = r.gl.getError();
  r.dispose();
  log(
    'E1 an object table issues 32,767 x 128 handles in its life (here 1 kept + 32,766 x 128), none twice, then refuses cleanly and keeps drawing',
    issued === 1 + 32766 * 128 && /too many objects/i.test(error ?? '') && d.lowSlotHandles === 19 * 128 - 1 && d.firstRefused && d.midRefused && d.lastRefused &&
      d.keepStillWorks && d.frame.value === true && d.pixel[1] > 200 && d.glError === 0,
    d,
  );
}

/** K1: 32 live renderers, a 33rd refused and cleaned up, then one more once a slot is free. */
async function cap() {
  const live = [];
  const d = { lostEvents: 0 };
  for (let k = 0; k < 32; k++) {
    const c = canvas(4, 4);
    c.addEventListener('webglcontextlost', () => d.lostEvents++);
    live.push(await createRenderer(c, { width: 4, height: 4, antialias: false }));
  }
  d.salts = live.map((r) => r._salt);
  d.distinctSalts = new Set(d.salts).size;
  const c33 = canvas(4, 4);
  let failedHook = 0;
  const t = await settle(createRenderer(c33, { width: 4, height: 4, antialias: false, onContextLost: () => failedHook++ }));
  d.thirtyThird = t.ok ? 'created' : t.message;
  live.shift().dispose();
  const again = await settle(createRenderer(c33, { width: 4, height: 4, antialias: false }));
  d.afterOneDisposed = again.ok ? 'created' : again.message;
  if (again.ok) {
    const lost = new Promise((res) => c33.addEventListener('webglcontextlost', () => setTimeout(res, 50), { once: true }));
    again.r.gl.getExtension('WEBGL_lose_context').loseContext();
    await Promise.race([lost, tick(2000)]);
    again.r.dispose();
  }
  d.failedCreatesHookCalls = failedHook;
  for (const r of live) r.dispose();
  log(
    'K1 a 33rd live renderer is refused, its failed create leaves no listener, and a slot freed by dispose is usable',
    d.distinctSalts === 32 && /32 renderers are live/.test(d.thirtyThird) && d.afterOneDisposed === 'created' && failedHook === 0,
    d,
  );
}

const GROUPS = {
  x8: [rounding],
  main: [conversion, arrayRereads, saltReuse, staging, stagingLeak, textureRoutes, flaky, foreignContext],
  heap: [bigHeap],
  exhaust: [exhaust],
  cap: [cap],
};

async function main() {
  const group = new URLSearchParams(location.search).get('group') ?? 'main';
  for (const f of GROUPS[group]) {
    try {
      await f();
    } catch (e) {
      log(`${f.name}: error`, false, String(e?.stack ?? e));
    }
  }
  return out;
}

window.__p5 = main();
