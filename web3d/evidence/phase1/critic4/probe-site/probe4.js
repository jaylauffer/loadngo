// Fourth critic's GPU probes for web3d phase 1: attacks on the handle check, the canvas ownership,
// the staging of typed arrays, the compiled-module cache and re-entrancy. Nothing here is taken
// from the builder's probes. window.__p4 resolves to { info, tests: [{ name, pass, detail }] }.
import { createRenderer } from '/web3d/pkg/web3d.js';

const out = { tests: [] };
const log = (name, pass, detail) => out.tests.push({ name, pass: Boolean(pass), detail });
const attempt = (fn) => {
  try {
    const value = fn();
    return { threw: false, value: typeof value === 'bigint' ? String(value) : value };
  } catch (e) {
    return { threw: true, message: String(e?.message ?? e) };
  }
};
const settle = (p) => p.then((r) => ({ ok: true, r }), (e) => ({ ok: false, message: String(e?.message ?? e) }));
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
const reader = (gl, canvas) => {
  const px = new Uint8Array(4);
  return (nx, ny) => {
    gl.readPixels(Math.floor(((nx + 1) / 2) * (canvas.width - 1)), Math.floor(((ny + 1) / 2) * (canvas.height - 1)), 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, px);
    return [px[0], px[1], px[2]];
  };
};
const flat = (r) => {
  r.setCamera(IDENT, ORTHO);
  r.setKeyLight(0, 0, 1, 0, 0, 0, 0);
  r.setAmbient(1, 1, 1, 1, 1, 1, Math.PI);
  r.setClearColor(0, 0, 0);
};

/** X1, X2: a handle of the wrong kind, or from another renderer. */
async function kinds() {
  const c = document.getElementById('kinds');
  const r = await createRenderer(c, { width: 64, height: 64, antialias: false });
  flat(r);
  const left = r.createMesh(quad(-1, -1, 0, 1, -5));
  const right = r.createMesh(quad(0, -1, 1, 1, -5));
  const red = r.createMaterial({ baseColor: [1, 0, 0, 1] });
  const green = r.createMaterial({ baseColor: [0, 1, 0, 1] });
  const d = { handles: { left, right, red, green } };
  // Arguments swapped: the material handle `green` as the mesh, the mesh handle `left` as the material.
  d.swappedCreateObject = attempt(() => r.createObject(green, left));
  r.frame();
  const read = reader(r.gl, c);
  d.rightHalfAfterSwap = read(0.5, 0);
  d.leftHalfAfterSwap = read(-0.5, 0);
  // A mesh handle passed to destroyMaterial.
  d.destroyMaterialWithMeshHandle = attempt(() => r.destroyMaterial(right));
  d.greenStillUsable = attempt(() => r.createObject(left, green));
  // A mesh handle passed as an object handle.
  d.setVisibleWithMeshHandle = attempt(() => r.setVisible(left, false));
  r.frame();
  d.rightHalfAfterHide = read(0.5, 0);
  log('X1 a handle of one kind is refused where another kind is meant', d.swappedCreateObject.threw && d.destroyMaterialWithMeshHandle.threw && d.setVisibleWithMeshHandle.threw, d);

  const c2 = document.getElementById('kinds2');
  const r2 = await createRenderer(c2, { width: 16, height: 16, antialias: false });
  const m2 = r2.createMesh(quad(-1, -1, 1, 1, -5));
  const mat2 = r2.createMaterial({});
  const o2 = r2.createObject(m2, mat2);
  const foreign = d.swappedCreateObject.value;
  const cross = { foreignObjectHandle: foreign, ownObjectHandle: o2, setVisible: attempt(() => r2.setVisible(foreign, false)), destroyObject: attempt(() => r2.destroyObject(foreign)) };
  log('X2 a handle from another renderer is refused', cross.setVisible.threw && cross.destroyObject.threw, cross);
  r.dispose();
  r2.dispose();
}

/** X3: a Proxy over a Float32Array passes `instanceof` and answers `length` differently each time. */
async function proxy() {
  const c = document.getElementById('proxy');
  const r = await createRenderer(c, { width: 32, height: 32, antialias: false });
  flat(r);
  r.createObject(r.createMesh(quad(-1, -1, 1, 1, -5)), r.createMaterial({ baseColor: [0, 1, 0, 1] }));
  r.frame();
  const read = reader(r.gl, c);
  const d = { pixelBefore: read(0, 0) };
  const MARK = 1.2345e30;
  const markBits = new Uint32Array(new Float32Array([MARK]).buffer)[0];
  const countMarks = () => {
    const u = new Uint32Array(r._x.memory.buffer);
    let n = 0;
    for (let i = 0; i < u.length; i++) if (u[i] === markBits) n++;
    return n;
  };
  d.marksBefore = countMarks();
  const real = new Float32Array([0, 0, -5, 0.5, 0, -5, 0, 0.5, -5]);
  const BIG = 9 + 4096;
  let reads = 0;
  const evil = new Proxy(real, {
    get(t, k) {
      if (k === 'length') {
        reads++;
        return reads === 2 || reads === 3 ? BIG : 9;
      }
      if (typeof k === 'string' && /^\d+$/.test(k)) return Number(k) < 9 ? t[Number(k)] : MARK;
      const v = Reflect.get(t, k, t);
      return typeof v === 'function' ? v.bind(t) : v;
    },
  });
  d.passesInstanceof = evil instanceof Float32Array;
  d.create = attempt(() => r.createMesh({ positions: evil, indices: new Uint16Array([0, 1, 2]) }));
  d.lengthReads = reads;
  d.marksAfter = countMarks();
  d.memoryBytes = r._x.memory.buffer.byteLength;
  d.frame = attempt(() => r.frame());
  d.pixelAfter = read(0, 0);
  d.laterCreates = [];
  for (let i = 0; i < 12; i++) {
    const a = attempt(() => r.createMesh(quad(-1, -1, 1, 1, -5)));
    d.laterCreates.push(a.threw ? a.message.slice(0, 80) : 'ok');
  }
  d.frame2 = attempt(() => r.frame());
  d.pixelAfter2 = read(0, 0);
  d.glError = r.gl.getError();
  d.dispose = attempt(() => r.dispose());
  // The staged allocation holds 9 floats (36 bytes); every MARK in memory was written past it.
  log('X3 a Proxy positions array cannot write past its staging allocation', d.create.threw && d.marksAfter === d.marksBefore, d);
}

/** X3b: a Float32Array subclass whose `length` getter changes: wasm is told more floats than were staged. */
async function lying() {
  const c = document.createElement('canvas');
  const r = await createRenderer(c, { width: 16, height: 16, antialias: false });
  let reads = 0;
  class Lying extends Float32Array {
    get length() {
      reads++;
      return reads >= 3 ? 9 + 3 * 1000 : 9;
    }
  }
  const pos = new Lying([0, 0, -5, 0.5, 0, -5, 0, 0.5, -5]);
  // An index of 1,000 is past the 3 vertices staged; it is accepted only if wasm read 1,003.
  const d = { create: attempt(() => r.createMesh({ positions: pos, indices: new Uint32Array([0, 1, 1000]) })), lengthReads: reads };
  log('X3b a subclass with a changing length cannot make wasm read past the staged floats', d.create.threw, d);
  r.dispose();
}

/** X4: a create that fails after the context exists leaves its listeners and context attributes behind. */
async function failed() {
  const c = document.getElementById('failed');
  const calls = { failedHook: 0, goodHook: 0 };
  const bad = await settle(createRenderer(c, { width: 1.5, height: 32, onContextLost: () => calls.failedHook++ }));
  const good = await createRenderer(c, { width: 32, height: 32, antialias: false, onContextLost: () => calls.goodHook++ });
  const asked = { antialias: false, got: good.info.antialias, samples: good.info.samples };
  const lost = new Promise((res) => c.addEventListener('webglcontextlost', () => setTimeout(res, 50), { once: true }));
  good.gl.getExtension('WEBGL_lose_context').loseContext();
  await lost;
  log('X4 a failed create leaves nothing behind: its loss hook never fires', calls.failedHook === 0 && calls.goodHook === 1, { bad: bad.ok ? 'created' : bad.message, ...calls, attributesOfTheNextRenderer: asked });
  good.dispose();
}

/** X5: one failed wasm fetch, then a create with the default source, in a fresh copy of the module. */
async function cache() {
  const mod = await import('/web3d/pkg/web3d.js?critic4-cache=1');
  const first = await settle(mod.createRenderer(document.getElementById('cache1'), { width: 16, height: 16, wasm: '/web3d/pkg/missing.wasm' }));
  const second = await settle(mod.createRenderer(document.getElementById('cache2'), { width: 16, height: 16 }));
  if (second.ok) second.r.dispose();
  log('X5 a failed wasm fetch does not poison every later create', second.ok, { first: first.ok ? 'created' : first.message, second: second.ok ? 'created' : second.message });
}

/** X6: a renderer that left blending on, depth writes off, culling off and a CW front face, then a new one on the same canvas. */
async function handover() {
  const scene = (r) => {
    flat(r);
    r.createObject(r.createMesh(quad(-1, -1, 0, 1, -5)), r.createMaterial({ baseColor: [0, 1, 0, 1] }));
    r.createObject(r.createMesh(quad(0, -1, 1, 1, -6)), r.createMaterial({ baseColor: [0, 0, 1, 1] }));
    r.createObject(r.createMesh(quad(-1, -1, 1, 1, -7)), r.createMaterial({ baseColor: [1, 1, 0, 1] }));
    r.frame();
  };
  const c = document.getElementById('handover');
  const a = await createRenderer(c, { width: 32, height: 32, antialias: false });
  flat(a);
  a.createObject(a.createMesh(quad(-1, -1, 1, 1, -6)), a.createMaterial({ baseColor: [1, 0, 0, 1] }));
  const mirror = new Float32Array([-1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1]);
  a.createObject(a.createMesh(quad(-1, -1, 1, 1, -4)), a.createMaterial({ baseColor: [1, 1, 1, 0.5], alphaMode: 'blend', doubleSided: true }), mirror);
  a.frame();
  a.dispose();
  const b = await createRenderer(c, { width: 32, height: 32, antialias: false });
  scene(b);
  const rb = reader(b.gl, c);
  const handed = [rb(-0.5, 0), rb(0.5, 0)];
  const f = document.getElementById('fresh');
  const fresh = await createRenderer(f, { width: 32, height: 32, antialias: false });
  scene(fresh);
  const rf = reader(fresh.gl, f);
  const clean = [rf(-0.5, 0), rf(0.5, 0)];
  log('X6 a renderer on a canvas another renderer used draws as on a fresh canvas', JSON.stringify(handed) === JSON.stringify(clean), { handed, clean, errB: b.gl.getError() });
  b.dispose();
  fresh.dispose();
}

/** X7: re-entrancy through valueOf during the call into wasm. */
async function reenter() {
  const c = document.getElementById('reenter');
  const r = await createRenderer(c, { width: 16, height: 16, antialias: false });
  const evil = { valueOf() { r.dispose(); return 0; } };
  const d = { setKeyLight: attempt(() => r.setKeyLight(evil, 1, 0, 1, 1, 1, 1)) };
  const r2 = await createRenderer(document.getElementById('race'), { width: 16, height: 16, antialias: false });
  d.setClearColor = attempt(() => r2.setClearColor({ valueOf() { r2.dispose(); return 0.5; } }, 0, 0));
  log('X7 an argument whose valueOf disposes the renderer does not run the export on the freed renderer', d.setKeyLight.threw && d.setClearColor.threw, d);
}

/** X8: odd handle values. */
async function edges() {
  const c = document.getElementById('edges');
  const r = await createRenderer(c, { width: 16, height: 16, antialias: false });
  flat(r);
  const o = r.createObject(r.createMesh(quad(-1, -1, 1, 1, -5)), r.createMaterial({}));
  const values = { minusZero: -0, numberObject: new Number(o), bigint: BigInt(o), boolean: true, array: [o], valueOf: { valueOf: () => o }, string: String(o), nearlyWhole: o + 2 ** -30, limit: 2 ** 30, overInt32: 2 ** 31 + o, infinity: Infinity };
  const d = {};
  for (const [k, v] of Object.entries(values)) d[k] = attempt(() => r.setVisible(v, false));
  const allRefused = Object.values(d).every((a) => a.threw && /is not a valid object handle/.test(a.message));
  r.frame();
  d.stillDrawn = reader(r.gl, c)(0, 0);
  log('X8 odd handle values are all refused by the wrapper, and the object stays drawn', allRefused && d.stillDrawn[0] > 200, d);
  r.dispose();
}

window.__p4 = (async () => {
  for (const t of [kinds, proxy, lying, failed, cache, handover, reenter, edges]) {
    try {
      await t();
    } catch (e) {
      log(`${t.name} threw`, false, String(e?.stack ?? e));
    }
  }
  return out;
})();
