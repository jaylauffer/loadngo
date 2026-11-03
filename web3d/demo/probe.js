// web3d's regression probes, read back from the GPU. Started from the phase-1 critic's probe
// (evidence/phase1/critic/probe-site/probe.js) and extended with the lighting reference, the input
// refusals and the context-loss recovery the critic found broken. window.__probe.done resolves to
// { info, tests: [{ name, pass, detail }], ... }; check.mjs fails unless every test passes.
//
// BSD-3-Clause; see the repository's LICENSE.

import { createRenderer } from '../pkg/web3d.js';

const out = { tests: [] };
window.__probe = { done: null, out };
const log = (name, pass, detail) => out.tests.push({ name, pass: Boolean(pass), detail });

const N = 0.1;
const F = 10;
const ORTHO = new Float32Array([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, -2 / (F - N), 0, 0, 0, -(F + N) / (F - N), 1]);
const IDENT = new Float32Array([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1]);
const S = 256;
const PI = Math.PI;

function quad(x0, y0, x1, y1, z, away = false) {
  const nz = away ? -1 : 1;
  return {
    positions: new Float32Array([x0, y0, z, x1, y0, z, x1, y1, z, x0, y1, z]),
    normals: new Float32Array([0, 0, nz, 0, 0, nz, 0, 0, nz, 0, 0, nz]),
    uvs: new Float32Array([0, 1, 1, 1, 1, 0, 0, 0]),
    indices: away ? new Uint32Array([0, 2, 1, 0, 3, 2]) : new Uint32Array([0, 1, 2, 0, 2, 3]),
  };
}
const translate = (x, y, z) => new Float32Array([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, x, y, z, 1]);
const scale3 = (sx, sy, sz) => new Float32Array([sx, 0, 0, 0, 0, sy, 0, 0, 0, 0, sz, 0, 0, 0, 0, 1]);
const srgb = (c) => {
  c = Math.min(1, Math.max(0, c));
  return c <= 0.0031308 ? c * 12.92 : 1.055 * c ** (1 / 2.4) - 0.055;
};
const byte = (c) => Math.round(srgb(c) * 255);
const lin = (s8) => {
  const c = s8 / 255;
  return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
};
const close = (a, b, tol = 2) => a.every((v, i) => Math.abs(v - b[i]) <= tol);
const bitmap = (w, h, rgba) =>
  createImageBitmap(new ImageData(new Uint8ClampedArray(rgba), w, h), { premultiplyAlpha: 'none', colorSpaceConversion: 'none' });
const attempt = (fn) => {
  try {
    return { threw: false, value: fn() };
  } catch (e) {
    return { threw: true, message: String(e?.message ?? e) };
  }
};
/** Reads the pixel at a point in normalised device coordinates, in the task that drew it. */
const reader = (r) => {
  const px = new Uint8Array(4);
  return (nx, ny) => {
    const { width, height } = r.canvas;
    r.gl.readPixels(Math.floor(((nx + 1) / 2) * width), Math.floor(((ny + 1) / 2) * height), 1, 1, r.gl.RGBA, r.gl.UNSIGNED_BYTE, px);
    return [px[0], px[1], px[2]];
  };
};

async function rendering(r) {
  const read = reader(r);
  const stats = new Float64Array(8);
  r.setCamera(IDENT, ORTHO);
  r.setClearColor(0, 0, 0);
  const all = [];
  const show = (...objs) => {
    for (const o of all) r.setVisible(o, objs.includes(o));
  };
  const obj = (mesh, mat, m = IDENT) => {
    const o = r.createObject(mesh, mat, m);
    all.push(o);
    return o;
  };
  const full = r.createMesh(quad(-1, -1, 1, 1, -5));
  const behind = r.createMesh(quad(-1, -1, 1, 1, -8));
  const white = r.createMaterial({ baseColor: [1, 1, 1, 1] });
  const red = r.createMaterial({ baseColor: [1, 0, 0, 1] });

  // A: sRGB-encoded output. No key; a uniform hemisphere of irradiance E: a white dielectric is
  // E/pi diffuse plus 0.04 E/pi reflected.
  r.setKeyLight(0, 0, 1, 0, 0, 0, 0);
  r.setAmbient(1, 1, 1, 1, 1, 1, 0.5 * PI);
  const oA = obj(full, white);
  show(oA);
  r.frame();
  const a = read(0, 0);
  log('A output is sRGB-encoded (white under a uniform hemisphere)', close(a, [byte(0.52), byte(0.52), byte(0.52)]), { got: a, expected: byte(0.52), ifLinear: Math.round(0.52 * 255) });
  r.setClearColor(0.5, 0.5, 0.5);
  show();
  r.frame();
  const cc = read(0, 0);
  log('A2 the clear colour is linear and encoded', close(cc, [byte(0.5), byte(0.5), byte(0.5)], 1), { got: cc, expected: byte(0.5) });
  r.setClearColor(0, 0, 0);

  // B: texture orientation (glTF: the image's first row is v = 0) and sRGB decoding. Hemisphere
  // irradiance pi / 1.04, so a texel t comes out as t + 0.04 scaled by 1 / 1.04.
  r.setAmbient(1, 1, 1, 1, 1, 1, PI / 1.04);
  const texB = r.createTexture(await bitmap(2, 2, [255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 128, 128, 128, 255]), { mipmaps: false });
  const oB = obj(full, r.createMaterial({ texture: texB }));
  show(oB);
  r.frame();
  const tl = read(-0.5, 0.5);
  const tr = read(0.5, 0.5);
  const bl = read(-0.5, -0.5);
  const br = read(0.5, -0.5);
  const lo = byte(0.04 / 1.04);
  const mid = byte((lin(128) + 0.04) / 1.04);
  log('B1 texture upright: top-left red, top-right green, bottom-left blue', close(tl, [255, lo, lo], 3) && close(tr, [lo, 255, lo], 3) && close(bl, [lo, lo, 255], 3), { tl, tr, bl, lo });
  log('B2 the sRGB texture is decoded (128 is linear 0.216)', close(br, [mid, mid, mid], 3), { got: br, expected: mid });

  // C: alpha mask over a red quad behind: the left half (alpha 0) is discarded.
  r.setAmbient(1, 1, 1, 1, 1, 1, PI);
  const texC = r.createTexture(await bitmap(2, 1, [0, 255, 0, 0, 0, 255, 0, 255]), { mipmaps: false, wrapS: 'clamp', wrapT: 'clamp' });
  const oCb = obj(behind, red);
  const oC = obj(full, r.createMaterial({ texture: texC, alphaMode: 'mask', alphaCutoff: 0.5 }));
  show(oCb, oC);
  r.frame();
  const cl = read(-0.75, 0);
  const cr = read(0.75, 0);
  log('C alpha mask: left discarded (red behind shows), right drawn (green)', cl[0] > 200 && cl[1] < 70 && cr[1] > 200 && cr[0] < 70, { left: cl, right: cr });

  // D: a blended quad created before the opaque one behind it still draws after it, blended in
  // encoded space: 0.5 * encode(blue) + 0.5 * encode(white).
  const front = r.createMesh(quad(-1, -1, 1, 1, -2));
  const oD = obj(front, r.createMaterial({ baseColor: [0, 0, 1, 0.5], alphaMode: 'blend' }));
  const oDb = obj(behind, white);
  show(oD, oDb);
  r.frame();
  const d = read(0, 0);
  const src = [0.04, 0.04, 1.04].map((v) => srgb(v) * 255);
  const ed = src.map((v) => Math.round(0.5 * v + 0.5 * 255));
  log('D blend over opaque, drawn last, blended in encoded space', close(d, ed, 3), { got: d, expected: ed });

  // E: depth, whatever the creation order.
  const green = r.createMaterial({ baseColor: [0, 1, 0, 1] });
  const near = r.createMesh(quad(-0.5, -0.5, 0.5, 0.5, -3));
  const oEn = obj(near, green);
  const oEf = obj(full, red);
  show(oEn, oEf);
  r.frame();
  const e = read(0, 0);
  const e2 = read(0.9, 0.9);
  log('E depth: the near quad wins, the far one shows around it', e[1] > 200 && e[0] < 70 && e2[0] > 200, { centre: e, corner: e2 });

  // F: back faces culled; a double-sided material draws them, its normal turned round.
  r.setAmbient(0, 0, 0, 0, 0, 0, 0);
  r.setKeyLight(0, 0, 1, 1, 1, 1, PI);
  const away = r.createMesh(quad(-1, -1, 1, 1, -5, true));
  const oF = obj(away, white);
  show(oF);
  r.frame();
  const f1 = read(0, 0);
  const oF2 = obj(away, r.createMaterial({ baseColor: [1, 1, 1, 1], doubleSided: true }));
  show(oF2);
  r.frame();
  const f2 = read(0, 0);
  log('F a single-sided back face is culled; double-sided draws it, lit from the front', f1[0] < 10 && f2[0] > 200, { culled: f1, doubleSided: f2 });

  // G: a mirroring transform keeps a single-sided quad drawn and lit.
  const oG = obj(full, white, scale3(-1, 1, 1));
  show(oG);
  r.frame();
  const g = read(0, 0);
  log('G a mirrored single-sided quad is still drawn and lit', g[0] > 200, { got: g });

  // H: frustum culling.
  const oH = obj(full, white, translate(5, 0, 0));
  show(oH);
  r.frame();
  r.readStats(stats);
  log('H an object outside the view is culled', stats[3] === 0 && stats[5] === 1, { draws: stats[3], culled: stats[5] });

  // N: u32 indices.
  {
    const n = 65537;
    const pos = new Float32Array(n * 3);
    pos.set([-1, -1, -5, 1, -1, -5], 0);
    pos.set([0, 1, -5], (n - 1) * 3);
    const nor = new Float32Array(n * 3);
    for (let i = 0; i < n; i++) nor[i * 3 + 2] = 1;
    const oN = obj(r.createMesh({ positions: pos, normals: nor, indices: new Uint32Array([0, 1, n - 1]) }), white);
    show(oN);
    r.frame();
    const nn = read(0, 0);
    log('N a 65,537-vertex mesh with u32 indices draws', nn[0] > 200, { got: nn });
  }

  // P: WebGL 2's primitive restart. A u16 index of 0xFFFF always restarts the primitive, so a
  // mesh of exactly 65,536 vertices must take u32 indices or its triangle on the last vertex
  // vanishes (the second critic found it did). Each mesh's only triangle uses its last vertex.
  for (const n of [65535, 65536, 65537]) {
    const pos = new Float32Array(n * 3);
    for (let i = 0; i < n; i++) pos[i * 3 + 2] = -5;
    pos.set([3, -1, -5], 0);
    pos.set([-1, 3, -5], 3);
    pos.set([-1, -1, -5], (n - 1) * 3);
    const nor = new Float32Array(n * 3);
    for (let i = 0; i < n; i++) nor[i * 3 + 2] = 1;
    const o = obj(r.createMesh({ positions: pos, normals: nor, indices: new Uint32Array([n - 1, 0, 1]) }), white);
    show(o);
    r.frame();
    const px = read(0, 0);
    log(`P${n - 65534} a ${n.toLocaleString('en')}-vertex mesh draws its triangle on vertex ${(n - 1).toLocaleString('en')}`, px[0] > 200, { pixel: px });
  }

  // R: lighting against three.js r180 (tools/three-reference.txt, computed from three's chunks).
  // The cases that face the camera: normal and view +Z at the centre pixel.
  {
    const text = await (await fetch('../tools/three-reference.txt')).text();
    let lightingCase = 0;
    const cases = text
      .split('\n')
      .filter((l) => l && !l.startsWith('#'))
      .map((l) => {
        const [name, ...v] = l.trim().split(/\s+/);
        return { name, v: v.map(Number) };
      })
      .filter(({ v }) => v[7] === 1 && v[13] === 1);
    for (const { name, v } of cases) {
      const [br0, bg0, bb0, metallic, roughness] = v;
      const mat = r.createMaterial({ baseColor: [br0, bg0, bb0, 1], roughness, metallic });
      const o = obj(full, mat);
      r.setKeyLight(v[8], v[9], v[10], v[14], v[15], v[16], 1);
      r.setAmbient(v[17], v[18], v[19], v[20], v[21], v[22], 1);
      show(o);
      r.frame();
      const got = read(0, 0);
      // web3d adds its environment stand-in: f0 times the hemisphere at the mirror direction
      // (+Z here, so half sky, half ground), as radiance.
      const expected = [0, 1, 2].map((c) => {
        const f0 = 0.04 * (1 - metallic) + [br0, bg0, bb0][c] * metallic;
        const env = (f0 * (0.5 * v[17 + c] + 0.5 * v[20 + c])) / PI;
        return byte(v[23 + c] + env);
      });
      log(`R${++lightingCase} lighting matches three.js r180: ${name}`, close(got, expected, 2), { got, expected, threeRadiance: v.slice(23) });
    }
    if (cases.length < 4) log('R0 lighting reference cases found', false, { cases: cases.length });
  }

  // M: the GPU timer.
  for (let i = 0; i < 10; i++) {
    r.frame();
    await new Promise((res) => requestAnimationFrame(res));
  }
  log('M a GPU timer reading is present', !r.info.timerQuery || r.gpuMs !== null, { gpuMs: r.gpuMs, timerQuery: r.info.timerQuery });

  // I: refusals, and what is accepted.
  const tri = new Float32Array([0, 0, 0, 1, 0, 0, 0, 1, 0]);
  const triIdx = new Uint16Array([0, 1, 2]);
  let refusal = 0;
  const check = (name, fn, want, pattern) => {
    const x = attempt(fn);
    const ok = want === 'throw' ? x.threw && (!pattern || pattern.test(x.message)) : !x.threw;
    log(`I${++refusal} ${name}`, ok, x);
  };
  check('an index past the end is refused', () => r.createMesh({ positions: new Float32Array(9), indices: new Uint32Array([0, 1, 3]) }), 'throw', /past the last vertex/);
  check('positions not a multiple of 3 are refused', () => r.createMesh({ positions: new Float32Array(8), indices: triIdx }), 'throw');
  check('a texture over MAX_TEXTURE_SIZE is refused', () => r.createTexture(new OffscreenCanvas(r.info.maxTextureSize + 1, 1)), 'throw');
  check('destroying a texture in use is refused', () => r.destroyTexture(texB), 'throw', /still used/);
  check('destroying a mesh in use is refused', () => r.destroyMesh(full), 'throw', /still used/);
  check('an unknown alpha mode is refused', () => r.createMaterial({ alphaMode: 'add' }), 'throw');
  check('a non-finite transform is refused', () => r.setTransform(oA, new Float32Array(16).fill(NaN)), 'throw');
  const gone = r.createMesh(quad(-1, -1, 1, 1, -5));
  r.destroyMesh(gone);
  check('a stale mesh handle is refused', () => r.createObject(gone, white), 'throw', /not live/);
  check('a NaN position is refused', () => r.createMesh({ positions: new Float32Array([0, 0, 0, NaN, 0, 0, 0, 1, 0]), indices: triIdx }), 'throw');
  check('Int16 (quantised) positions are refused', () => r.createMesh({ positions: new Int16Array([0, 0, 0, 32767, 0, 0, 0, 32767, 0]), indices: triIdx }), 'throw', /Float32Array/);
  check('a plain Array of positions is refused', () => r.createMesh({ positions: [0, 0, 0, 1, 0, 0, 0, 1, 0], indices: triIdx }), 'throw', /Float32Array/);
  check('Uint8 normalised weights are dequantised and accepted', () => r.createMesh({ positions: tri, indices: triIdx, joints: new Uint8Array(12), weights: new Uint8Array([255, 0, 0, 0, 255, 0, 0, 0, 128, 127, 0, 0]) }), 'accept');
  check('weights of 1.75 (7 a vertex) are refused', () => r.createMesh({ positions: tri, indices: triIdx, joints: new Uint16Array(12), weights: new Float32Array(12).fill(1.75) }), 'throw', /between 0 and 1/);
  check('weights summing to 2 are refused', () => r.createMesh({ positions: tri, indices: triIdx, joints: new Uint16Array(12), weights: new Float32Array(12).fill(0.5) }), 'throw', /sum to 1/);
  check('joint index 65535 is accepted until phase 3 has skeletons', () => r.createMesh({ positions: tri, indices: triIdx, joints: new Uint16Array(12).fill(65535), weights: new Float32Array(12).fill(0.25) }), 'accept');
  check('morph deltas of the wrong length are refused', () => r.createMesh({ positions: tri, indices: triIdx, morphTargets: [new Float32Array(6)] }), 'throw');
  check('an unknown wrap mode is refused', () => r.createTexture(new OffscreenCanvas(4, 4), { wrapS: 'clamp-to-border' }), 'throw', /wrap modes/);
  check("a wrap mode named after Object.prototype ('toString') is refused", () => r.createTexture(new OffscreenCanvas(4, 4), { wrapT: 'toString' }), 'throw', /wrap modes/);
  check('a NaN normal is refused', () => r.createMesh({ positions: tri, normals: new Float32Array([0, 0, 1, NaN, 0, 1, 0, 0, 1]), indices: triIdx }), 'throw', /normals must be finite/);
  check('a drawing buffer of 0x80000001 pixels is refused', () => r.resize(0x80000001, 64), 'throw', /whole number from 0 to 32768/);
  check('a fractional drawing-buffer size is refused', () => r.resize(64.5, 64), 'throw', /whole number/);
  check('257 morph targets are refused', () => r.createMesh({ positions: tri, indices: triIdx, morphTargets: Array.from({ length: 257 }, () => new Float32Array(9)) }), 'throw', /1 to 256/);
  {
    const first = r.createMaterial({});
    r.destroyMaterial(first);
    for (let i = 0; i < 1100; i++) r.destroyMaterial(r.createMaterial({}));
    const later = r.createMaterial({});
    check('after 1,100 create/destroy cycles the first handle is still refused', () => r.destroyMaterial(first), 'throw', /not live/);
    log('I handles are not reissued', later !== first, { first, later });
  }
  // H: forged handles: a handle plus 2^32, a fraction, a string, out of range. Handles cross into
  // wasm as exact doubles; the wrapper refuses what is not a whole number, and the Rust side the
  // rest (the third critic's probe H).
  {
    r.setKeyLight(0, 0, 1, 0, 0, 0, 0);
    r.setAmbient(1, 1, 1, 1, 1, 1, PI);
    const o = obj(full, white);
    show(o);
    const forged = [o + 2 ** 32, o + 0.75, String(o), -o, 0, 2 ** 30, NaN];
    const tries = forged.map((h) => attempt(() => r.setVisible(h, false)));
    r.frame();
    const still = read(0, 0);
    const refused = tries.every((t) => t.threw && /is not a valid object handle/.test(t.message));
    const others = [
      attempt(() => r.setTransform(o + 2 ** 32, IDENT)),
      attempt(() => r.destroyObject(String(o))),
      attempt(() => r.createObject(full + 0.5, white)),
      attempt(() => r.createMaterial({ texture: texB + 2 ** 32 })),
      attempt(() => r.destroyMesh(full + 2 ** 32)),
    ];
    log('HF forged handles (plus 2^32, fractional, a string, out of range) are refused, and the object stays drawn', refused && others.every((t) => t.threw) && still[0] > 200, { tries, others, pixel: still });
  }
  out.mainGlError = r.gl.getError();
  log('Z no GL error on the main renderer', out.mainGlError === 0, { getError: out.mainGlError });
}

async function disposal() {
  const r = await createRenderer(document.getElementById('disposed'), { width: 64, height: 64, antialias: false });
  const mesh = r.createMesh(quad(-1, -1, 1, 1, -5));
  r.dispose();
  const after = {
    frame: attempt(() => r.frame()),
    createMaterial: attempt(() => r.createMaterial({})),
    createMesh: attempt(() => r.createMesh(quad(-1, -1, 1, 1, -5))),
    setTransform: attempt(() => r.setTransform(1, IDENT)),
    destroyMesh: attempt(() => r.destroyMesh(mesh)),
    readStats: attempt(() => r.readStats(new Float64Array(8))),
    setCamera: attempt(() => r.setCamera(IDENT, ORTHO)),
    disposeAgain: attempt(() => r.dispose()),
    gpuMs: r.gpuMs,
  };
  const refused = ['frame', 'createMaterial', 'createMesh', 'setTransform', 'destroyMesh', 'readStats', 'setCamera'].every((k) => after[k].threw && /renderer disposed/.test(after[k].message));
  log('J every call after dispose() throws "renderer disposed"; disposing twice is harmless', refused && !after.disposeAgain.threw && after.gpuMs === null, after);
}

async function sharing() {
  // S: two renderers on one canvas would share its WebGL context (a React StrictMode double mount).
  const canvas = document.getElementById('shared');
  const first = await createRenderer(canvas, { width: 32, height: 32, antialias: false });
  const second = await createRenderer(canvas, { width: 32, height: 32, antialias: false }).then(
    (r) => (r.dispose(), 'created'),
    (e) => String(e.message),
  );
  // Two calls in a row, before either has finished, cannot both pass.
  first.dispose();
  const [a, b] = await Promise.allSettled([
    createRenderer(canvas, { width: 32, height: 32, antialias: false }),
    createRenderer(canvas, { width: 32, height: 32, antialias: false }),
  ]);
  const raced = [a.status, b.status];
  if (a.status === 'fulfilled') a.value.dispose();
  if (b.status === 'fulfilled') b.value.dispose();
  // Once the owner is disposed, the canvas is free again.
  const again = await createRenderer(canvas, { width: 32, height: 32, antialias: false }).then(
    (r) => (r.dispose(), 'created'),
    (e) => String(e.message),
  );
  log('S a second renderer on a canvas with a live one is refused; the canvas is free again after dispose', /already has a live renderer/.test(second) && raced.filter((x) => x === 'fulfilled').length === 1 && again === 'created', { second, raced, again });
}

async function contextLoss() {
  const canvas = document.getElementById('loss');
  let lostResolve;
  let restoredResolve;
  const lost = new Promise((res) => (lostResolve = res));
  const restored = new Promise((res) => (restoredResolve = res));
  const timeout = (ms) => new Promise((res) => setTimeout(() => res('timeout'), ms));
  const old = await createRenderer(canvas, { width: 64, height: 64, antialias: false, onContextLost: () => lostResolve('lost'), onContextRestored: () => restoredResolve('restored') });
  old.setCamera(IDENT, ORTHO);
  const tex = old.createTexture(new OffscreenCanvas(4, 4));
  const kept = old.createObject(old.createMesh(quad(-1, -1, 1, 1, -5)), old.createMaterial({ texture: tex }));
  old.frame();
  const lc = old.gl.getExtension('WEBGL_lose_context');
  lc.loseContext();
  const k = { lostEvent: await Promise.race([lost, timeout(2000)]) };
  // Continue in a later task: this await resumed inside the event's dispatch (a microtask), and
  // Chrome allows restoreContext() only once the dispatch has returned.
  await timeout(0);
  k.lostFlag = old.lost;
  k.frame = attempt(() => old.frame());
  k.createMesh = attempt(() => old.createMesh(quad(-1, -1, 1, 1, -5)));
  k.createTexture = attempt(() => old.createTexture(new OffscreenCanvas(4, 4)));
  k.createMaterial = attempt(() => old.createMaterial({}));
  k.createObject = attempt(() => old.createObject(1, 1));
  k.setTransformWhileLost = attempt(() => old.setTransform(kept, IDENT));
  k.createRendererWhileLost = await createRenderer(canvas, { width: 64, height: 64, antialias: false }).then(
    (r) => (r.dispose(), 'created'),
    (e) => String(e.message),
  );
  out.contextLoss = k;
  k.isContextLostBeforeRestore = old.gl.isContextLost();
  lc.restoreContext();
  k.restoredEvent = await Promise.race([restored, timeout(3000)]);
  k.oldStillLost = old.lost;
  // The harder order: a new renderer first, then the old one disposed. The old renderer must send
  // nothing into the restored context, which the new one owns.
  const fresh = await createRenderer(canvas, { width: 64, height: 64, antialias: false });
  fresh.setCamera(IDENT, ORTHO);
  fresh.setKeyLight(0, 0, 1, 0, 0, 0, 0);
  fresh.setAmbient(1, 1, 1, 1, 1, 1, PI);
  fresh.createObject(fresh.createMesh(quad(-1, -1, 1, 1, -5)), fresh.createMaterial({ baseColor: [0, 1, 0, 1] }));
  fresh.frame();
  const read = reader(fresh);
  k.pixelBefore = read(0, 0);
  k.errorBeforeOldDispose = fresh.gl.getError();
  old.dispose();
  k.errorAfterOldDispose = fresh.gl.getError();
  fresh.frame();
  k.pixelAfter = read(0, 0);
  k.errorAfterFrame = fresh.gl.getError();
  out.contextLoss = k;
  const createsRefused = ['createMesh', 'createTexture', 'createMaterial', 'createObject'].every((n) => k[n].threw && /context is lost/.test(k[n].message));
  log('KL loss: frame() returns false and every create throws "context is lost", a new renderer too', k.lostEvent === 'lost' && k.lostFlag && k.frame.value === false && createsRefused && !k.setTransformWhileLost.threw && /context is lost/.test(k.createRendererWhileLost), k);
  log('KR restore: the old renderer stays finished; a new one draws', k.restoredEvent === 'restored' && k.oldStillLost && k.pixelBefore[1] > 200 && k.pixelAfter[1] > 200, k);
  log('KD disposing the old renderer sends nothing into the restored context (getError stays 0)', k.errorBeforeOldDispose === 0 && k.errorAfterOldDispose === 0 && k.errorAfterFrame === 0, k);
  fresh.dispose();
}

// The boundary probes added after the fourth critic's round: typed arrays that lie, handles of
// another kind or renderer, the compile cache, failed creates, and re-entrancy.

const flat = (r) => {
  r.setCamera(IDENT, ORTHO);
  r.setKeyLight(0, 0, 1, 0, 0, 0, 0);
  r.setAmbient(1, 1, 1, 1, 1, 1, PI);
  r.setClearColor(0, 0, 0);
};
const settle = (p) => p.then((r) => ({ ok: true, r }), (e) => ({ ok: false, message: String(e?.message ?? e) }));
const timeout = (ms) => new Promise((res) => setTimeout(() => res('timeout'), ms));

/** Q: typed arrays are read by their internal slots; Rust refuses an array outside its staging. */
async function typedArrays() {
  const c = document.getElementById('typed');
  const r = await createRenderer(c, { width: 32, height: 32, antialias: false });
  flat(r);
  r.createObject(r.createMesh(quad(-1, -1, 1, 1, -5)), r.createMaterial({ baseColor: [0, 1, 0, 1] }));
  r.frame();
  const read = reader(r);
  const MARK = 1.2345e30;
  const markBits = new Uint32Array(new Float32Array([MARK]).buffer)[0];
  const marks = () => {
    const u = new Uint32Array(r._x.memory.buffer);
    let n = 0;
    for (let i = 0; i < u.length; i++) if (u[i] === markBits) n++;
    return n;
  };
  const d = { marksBefore: marks() };
  // A Proxy over a Float32Array passes instanceof and answers length 9, then 4,105.
  let reads = 0;
  const real = new Float32Array([0, 0, -5, 0.5, 0, -5, 0, 0.5, -5]);
  const evil = new Proxy(real, {
    get(t, k) {
      if (k === 'length') return ++reads === 1 ? 9 : 9 + 4096;
      if (typeof k === 'string' && /^\d+$/.test(k)) return Number(k) < 9 ? t[Number(k)] : MARK;
      const v = Reflect.get(t, k, t);
      return typeof v === 'function' ? v.bind(t) : v;
    },
  });
  d.passesInstanceof = evil instanceof Float32Array;
  d.proxy = attempt(() => r.createMesh({ positions: evil, indices: new Uint16Array([0, 1, 2]) }));
  d.proxyLengthReads = reads;
  // A subclass whose length getter grows: the real length (9 floats, 3 vertices) is what is staged.
  let lies = 0;
  class Lying extends Float32Array {
    get length() {
      return ++lies >= 2 ? 3009 : 9;
    }
  }
  const lying = new Lying([0, 0, -5, 0.5, 0, -5, 0, 0.5, -5]);
  d.lyingIndex1000 = attempt(() => r.createMesh({ positions: lying, indices: new Uint32Array([0, 1, 1000]) }));
  d.lyingAccepted = attempt(() => r.createMesh({ positions: lying, indices: new Uint32Array([0, 1, 2]) }));
  d.lyingGetterCalls = lies;
  // Straight into the module: a pointer and length outside any staging allocation, and a forged free.
  const x = r._x;
  d.rawOutside = x.web3d_mesh_create(r._w, 1024, 1_000_000, 0, 0, 0, 0, 2048, 3, 0, 0, 0, 0, 0, 0, 0);
  d.rawOutsideError = r._errorText();
  x.web3d_free(4096, 64);
  d.marksAfter = marks();
  d.frame = attempt(() => r.frame());
  d.pixel = read(0, 0);
  d.later = attempt(() => r.createMesh(quad(-1, -1, 1, 1, -5)));
  d.glError = r.gl.getError();
  r.dispose();
  log(
    'Q a Proxy typed array is refused and nothing is written past staging; a lying subclass stages its real length; Rust refuses an array outside staging',
    d.proxy.threw && /Float32Array/.test(d.proxy.message) && d.proxyLengthReads === 0 && d.marksAfter === d.marksBefore &&
      d.lyingIndex1000.threw && /index/i.test(d.lyingIndex1000.message) && !d.lyingAccepted.threw && d.lyingGetterCalls === 0 &&
      d.rawOutside === 0 && /staging allocation/.test(d.rawOutsideError) && d.frame.value === true && d.pixel[1] > 200 && !d.later.threw && d.glError === 0,
    d,
  );
}

/** W: handles name one kind in one renderer. */
async function kinds() {
  const c = document.getElementById('kinds');
  const r = await createRenderer(c, { width: 32, height: 32, antialias: false });
  flat(r);
  const left = r.createMesh(quad(-1, -1, 0, 1, -5));
  const right = r.createMesh(quad(0, -1, 1, 1, -5));
  const red = r.createMaterial({ baseColor: [1, 0, 0, 1] });
  const green = r.createMaterial({ baseColor: [0, 1, 0, 1] });
  const d = { handles: { left, right, red, green } };
  d.swapped = attempt(() => r.createObject(green, left));
  d.destroyMaterialWithMesh = attempt(() => r.destroyMaterial(right));
  d.setVisibleWithMesh = attempt(() => r.setVisible(left, false));
  d.small = [1, 2, 3, 4, 7].map((h) => attempt(() => r.setVisible(h, false)));
  const o = r.createObject(right, green);
  r.frame();
  d.rightHalf = reader(r)(0.5, 0);
  const c2 = document.getElementById('kinds2');
  const r2 = await createRenderer(c2, { width: 16, height: 16, antialias: false });
  const o2 = r2.createObject(r2.createMesh(quad(-1, -1, 1, 1, -5)), r2.createMaterial({}));
  d.handlesDiffer = o !== o2;
  d.foreignSetVisible = attempt(() => r2.setVisible(o, false));
  d.foreignDestroy = attempt(() => r2.destroyObject(o));
  d.ownStillWorks = attempt(() => r2.setVisible(o2, true));
  r.dispose();
  r2.dispose();
  log(
    'W a handle of another kind, a bare small number, or another renderer\'s handle is refused; the right object still draws',
    d.swapped.threw && /is not a valid mesh handle: it is a material handle/.test(d.swapped.message) &&
      d.destroyMaterialWithMesh.threw && /is not a valid material handle: it is a mesh handle/.test(d.destroyMaterialWithMesh.message) &&
      d.setVisibleWithMesh.threw && d.small.every((t) => t.threw) && d.rightHalf[1] > 200 &&
      d.handlesDiffer && d.foreignSetVisible.threw && /another renderer/.test(d.foreignSetVisible.message) && d.foreignDestroy.threw && !d.ownStillWorks.threw,
    d,
  );
}

/** L: a failed fetch is not cached, and a later wasm option is used, in a fresh copy of the module. */
async function compileCache() {
  const mod = await import('../pkg/web3d.js?probe-cache=1');
  const missing = new URL('../pkg/missing.wasm', location.href).href;
  const second = new URL('../pkg/loadngo_web3d.wasm?second=1', location.href).href;
  // Counts the module's fetches by URL (resource timing does not list every failed fetch).
  const counts = new Map();
  const realFetch = globalThis.fetch;
  globalThis.fetch = (input, init) => {
    const url = new URL(String(input), location.href).href;
    counts.set(url, (counts.get(url) ?? 0) + 1);
    return realFetch(input, init);
  };
  const fetched = (url) => counts.get(url) ?? 0;
  const d = {};
  d.first = await settle(mod.createRenderer(document.getElementById('cache1'), { width: 16, height: 16, antialias: false, wasm: missing }));
  d.retry = await settle(mod.createRenderer(document.getElementById('cache1'), { width: 16, height: 16, antialias: false, wasm: missing }));
  d.missingFetches = fetched(missing);
  d.byDefault = await settle(mod.createRenderer(document.getElementById('cache2'), { width: 16, height: 16, antialias: false }));
  if (d.byDefault.ok) d.byDefault.r.dispose();
  d.other = await settle(mod.createRenderer(document.getElementById('cache2'), { width: 16, height: 16, antialias: false, wasm: second }));
  if (d.other.ok) d.other.r.dispose();
  d.secondFetches = fetched(second);
  globalThis.fetch = realFetch;
  for (const k of ['byDefault', 'other']) if (d[k].ok) d[k] = 'created';
  log(
    'L a failed wasm fetch is not cached (fetched again, and the default still loads); a later wasm option is fetched, not ignored',
    !d.first.ok && !d.retry.ok && d.missingFetches === 2 && d.byDefault === 'created' && d.other === 'created' && d.secondFetches === 1,
    d,
  );
}

/** Y: a failed create leaves nothing behind; a canvas keeps its first context's settings. */
async function failedCreate() {
  const c = document.getElementById('failed');
  const calls = { early: 0, late: 0, good: 0 };
  const d = {};
  // Refused before any context is made.
  d.early = await settle(createRenderer(c, { width: 1.5, height: 32, onContextLost: () => calls.early++ }));
  // Fails after the context and the shim exist: a module that is not web3d.
  const empty = new WebAssembly.Module(new Uint8Array([0, 97, 115, 109, 1, 0, 0, 0]));
  d.late = await settle(createRenderer(c, { width: 32, height: 32, antialias: false, wasm: empty, onContextLost: () => calls.late++ }));
  // Forty more failures, then forty creates: no canvas is left held.
  for (let i = 0; i < 40; i++) await settle(createRenderer(c, { width: 32, height: 32, antialias: false, wasm: empty }));
  let created = 0;
  for (let i = 0; i < 40; i++) {
    const r = await createRenderer(c, { width: 32, height: 32, antialias: false });
    created++;
    r.dispose();
  }
  d.createdAfterFailures = created;
  // A canvas keeps its first context's settings: asking for others is refused.
  d.mismatch = await settle(createRenderer(c, { width: 32, height: 32, antialias: true }));
  const good = await createRenderer(c, { width: 32, height: 32, antialias: false, onContextLost: () => calls.good++ });
  d.goodAttributes = { antialias: good.info.antialias, samples: good.info.samples };
  const lost = new Promise((res) => c.addEventListener('webglcontextlost', () => setTimeout(res, 50), { once: true }));
  good.gl.getExtension('WEBGL_lose_context').loseContext();
  await Promise.race([lost, timeout(2000)]);
  good.dispose();
  d.calls = calls;
  for (const k of ['early', 'late', 'mismatch']) d[k] = d[k].ok ? 'created' : d[k].message;
  log(
    'Y a failed create (before or after the context exists) leaves no listener or claim behind; a canvas refuses other context settings',
    /whole number/.test(d.early) && /not loadngo-web3d/.test(d.late) && created === 40 &&
      /keeps its first context/.test(d.mismatch) && d.goodAttributes.antialias === false && d.goodAttributes.samples === 0 &&
      calls.early === 0 && calls.late === 0 && calls.good === 1,
    d,
  );
}

/** U: user code run while a call reads its arguments cannot dispose the renderer or call into it. */
async function reentrancy() {
  const c = document.getElementById('reenter');
  const r = await createRenderer(c, { width: 16, height: 16, antialias: false });
  flat(r);
  const o = r.createObject(r.createMesh(quad(-1, -1, 1, 1, -5)), r.createMaterial({ baseColor: [0, 1, 0, 1] }));
  const d = { valueOfCalls: 0 };
  const evil = { valueOf: () => (d.valueOfCalls++, r.dispose(), 0) };
  d.keyLight = attempt(() => r.setKeyLight(evil, 1, 0, 1, 1, 1, 1));
  d.clearColor = attempt(() => r.setClearColor(evil, 0, 0));
  const matrix = Array.from(IDENT);
  Object.defineProperty(matrix, 12, { get: () => (r.dispose(), 0) });
  d.getterDisposes = attempt(() => r.setTransform(o, matrix));
  const boxed = Array.from(IDENT);
  boxed[13] = { valueOf: () => (d.valueOfCalls++, r.frame(), 0) };
  d.valueOfFrames = attempt(() => r.setTransform(o, boxed));
  d.descriptorGetter = attempt(() =>
    r.createMesh({
      get positions() {
        r.setVisible(o, false);
        return new Float32Array(9);
      },
      indices: new Uint16Array([0, 1, 2]),
    }),
  );
  d.materialGetter = attempt(() => r.createMaterial({ get baseColor() { r.dispose(); return [1, 0, 0, 1]; } }));
  d.stillLive = attempt(() => r.frame());
  d.pixel = reader(r)(0, 0);
  d.glError = r.gl.getError();
  r.dispose();
  const reentrant = (t) => t.threw && /while the renderer was reading/.test(t.message);
  log(
    'U a getter or descriptor that calls the renderer is refused; numbers, in arguments and in Arrays, are never converted, so no valueOf runs; the renderer stays live and drawn',
    d.valueOfCalls === 0 && d.keyLight.threw && /must be a number/.test(d.keyLight.message) && d.clearColor.threw &&
      reentrant(d.getterDisposes) && d.valueOfFrames.threw && /must be a number/.test(d.valueOfFrames.message) &&
      reentrant(d.descriptorGetter) && reentrant(d.materialGetter) &&
      d.stillLive.value === true && d.pixel[1] > 200 && d.glError === 0,
    d,
  );
}

/** H6: no handle is issued twice under one counter, across many renderers, live or disposed. */
async function serials() {
  const c = document.getElementById('serials');
  const opts = { width: 16, height: 16, antialias: false };
  const seen = new Set();
  let twice = 0;
  const keep = (h) => {
    if (seen.has(h)) twice++;
    seen.add(h);
    return h;
  };
  const first = await createRenderer(c, opts);
  flat(first);
  const firstObject = keep(first.createObject(keep(first.createMesh(quad(-1, -1, 1, 1, -5))), keep(first.createMaterial({}))));
  first.dispose();
  // 100 renderers come and go on one canvas: more than three cycles of the 32 salts an earlier
  // scheme had. Each makes a mesh, a material and two objects, destroying one.
  for (let i = 0; i < 100; i++) {
    const r = await createRenderer(c, opts);
    const mesh = keep(r.createMesh(quad(-1, -1, 1, 1, -5)));
    const mat = keep(r.createMaterial({}));
    r.destroyObject(keep(r.createObject(mesh, mat)));
    keep(r.createObject(mesh, mat));
    r.dispose();
  }
  const last = await createRenderer(c, opts);
  flat(last);
  const lastObject = keep(last.createObject(keep(last.createMesh(quad(-1, -1, 1, 1, -5))), keep(last.createMaterial({ baseColor: [0, 1, 0, 1] }))));
  const d = { handles: seen.size, twice, firstObject, lastObject };
  d.staleOnLast = attempt(() => last.setVisible(firstObject, false));
  last.frame();
  d.pixel = reader(last)(0, 0);
  last.dispose();
  log(
    'H6 102 renderers on one canvas issue 406 handles, none twice; the first renderer\'s handle is refused by the last, whose object stays drawn',
    d.handles === 406 && twice === 0 && d.staleOnLast.threw && /another renderer/.test(d.staleOnLast.message) && d.pixel[1] > 200,
    d,
  );
}

/** C: Array elements must be numbers (no valueOf runs), and an Array's length is read once. */
async function arrays() {
  const r = await createRenderer(document.getElementById('arrays'), { width: 16, height: 16, antialias: false });
  flat(r);
  const mesh = r.createMesh(quad(-1, -1, 1, 1, -5));
  const mat = r.createMaterial({ baseColor: [0, 1, 0, 1] });
  const o = r.createObject(mesh, mat);
  const d = { valueOfCalls: 0 };
  const plain = Array.from(IDENT);
  d.plainArray = attempt(() => r.setTransform(o, plain));
  d.strings = attempt(() => r.createObject(mesh, mat, Array.from(IDENT, String)));
  d.valueOfs = attempt(() => r.setTransform(o, Array.from(IDENT, (v) => ({ valueOf: () => (d.valueOfCalls++, v) }))));
  d.lookAtStrings = attempt(() => r.setCameraLookAt(['0', '0', '5'], [0, 0, 0], 0.8, 0.1, 100));
  d.booleans = attempt(() => r.setCamera(Array.from(IDENT, (v) => v === 1), ORTHO));
  d.nulls = attempt(() => r.setCamera(Array.from(IDENT, (v) => (v === 0 ? null : v)), ORTHO));
  d.colourString = attempt(() => r.createMaterial({ baseColor: ['1', 0, 0, 1] }));
  let reads = 0;
  const lying = new Proxy(Array.from(IDENT), {
    get(t, k, rcv) {
      if (k === 'length') return ++reads === 1 ? 16 : 64;
      return Reflect.get(t, k, rcv);
    },
  });
  d.lyingLength = attempt(() => r.setTransform(o, lying));
  d.lengthReads = reads;
  r.setCamera(IDENT, ORTHO);
  r.frame();
  d.pixel = reader(r)(0, 0);
  d.glError = r.gl.getError();
  r.dispose();
  const refused = (t) => t.threw && /must be a number/.test(t.message);
  log(
    'AR Array matrices, vectors and colours must hold numbers (no valueOf runs) and their length is read once; plain number Arrays work',
    !d.plainArray.threw && refused(d.strings) && refused(d.valueOfs) && d.valueOfCalls === 0 && refused(d.lookAtStrings) &&
      refused(d.booleans) && refused(d.nulls) && refused(d.colourString) && !d.lyingLength.threw && d.lengthReads === 1 &&
      d.pixel[1] > 200 && d.glError === 0,
    d,
  );
}

/** T: a texture source resized by an option getter; a source resized behind the size check. */
async function textureSize() {
  const r = await createRenderer(document.getElementById('texsize'), { width: 16, height: 16, antialias: false });
  flat(r);
  const src = document.createElement('canvas');
  src.width = 4;
  src.height = 4;
  const g = src.getContext('2d');
  g.fillStyle = '#00ff00';
  g.fillRect(0, 0, 4, 4);
  const d = {};
  d.grownInGetter = attempt(() => r.createTexture(src, { get wrapS() { src.width = 64; src.height = 64; g.fillStyle = '#00ff00'; g.fillRect(0, 0, 64, 64); return 'clamp'; } }));
  d.errorAfterGrow = r.gl.getError();
  if (!d.grownInGetter.threw) {
    r.createObject(r.createMesh(quad(-1, -1, 1, 1, -5)), r.createMaterial({ texture: d.grownInGetter.value }));
    r.frame();
    d.pixel = reader(r)(0, 0);
  }
  // Behind the wrapper's back: the shim's own check before upload, given a size that is not the source's.
  const shim = r._shim;
  shim.stage(src, () => [4, 4]);
  d.shimRefused = r._x.web3d_texture_create(r._w, 64, 64, 1) === 0 ? r._errorText() : 'uploaded';
  shim.stage(null);
  d.errorAfterShim = r.gl.getError();
  r.dispose();
  log(
    'T a texture option getter that resizes the source gets a texture of the new size with no GL error; the shim refuses a size that is not the source\'s',
    !d.grownInGetter.threw && d.errorAfterGrow === 0 && d.pixel?.[1] > 200 && /refused the texture upload/.test(d.shimRefused) && d.errorAfterShim === 0,
    d,
  );
}

/**
 * Y2: a canvas whose WebGL2 context the page made, with other settings, and a failed create after
 * the canvas was resized. The fake module below exports what web3d.js needs, but its init fails.
 */
async function foreignContext() {
  const c = document.getElementById('foreign');
  const gl = c.getContext('webgl2', { antialias: false });
  const d = { pageAntialias: gl.getContextAttributes().antialias };
  d.askedForAntialias = await settle(createRenderer(c, { width: 16, height: 16, antialias: true }));
  const ok = await settle(createRenderer(c, { width: 16, height: 16, antialias: false }));
  d.askedForNone = ok.ok ? 'created' : ok.message;
  if (ok.ok) ok.r.dispose();
  // A module whose web3d_init fails, so the create fails after the canvas was sized.
  const failing = failingModule();
  const c2 = document.getElementById('resized');
  c2.width = 20;
  c2.height = 10;
  d.failedInit = await settle(createRenderer(c2, { width: 48, height: 24, antialias: false, wasm: failing }));
  d.sizeAfterFailure = [c2.width, c2.height];
  d.askedForAntialias = d.askedForAntialias.ok ? 'created' : d.askedForAntialias.message;
  d.failedInit = d.failedInit.ok ? 'created' : d.failedInit.message;
  log(
    'Y2 a canvas whose page-made context has no antialias refuses a renderer that asks for it, and accepts one that does not; a failed create puts the canvas size back',
    d.pageAntialias === false && /keeps its first context/.test(d.askedForAntialias) && d.askedForNone === 'created' &&
      /init failed on purpose/.test(d.failedInit) && d.sizeAfterFailure[0] === 20 && d.sizeAfterFailure[1] === 10,
    d,
  );
}

/**
 * A tiny wasm module, assembled here: memory, web3d_create returning 16, web3d_init returning 1
 * (failure), web3d_error_ptr/len pointing at "init failed on purpose", web3d_destroy doing nothing.
 */
function failingModule() {
  const message = new TextEncoder().encode('init failed on purpose');
  const leb = (n) => {
    const out = [];
    do {
      let b = n & 0x7f;
      n >>>= 7;
      if (n) b |= 0x80;
      out.push(b);
    } while (n);
    return out;
  };
  const section = (id, bytes) => [id, ...leb(bytes.length), ...bytes];
  const vec = (items) => [...leb(items.length), ...items.flat()];
  const name = (s) => [...leb(s.length), ...new TextEncoder().encode(s)];
  const I32 = 0x7f;
  const types = vec([
    [0x60, 1, I32, 1, I32], // (i32) -> i32
    [0x60, 3, I32, I32, I32, 1, I32], // (i32, i32, i32) -> i32
    [0x60, 1, I32, 0], // (i32) -> ()
  ]);
  const funcs = vec([[0], [1], [0], [0], [2]]);
  const memory = vec([[0x00, 1]]);
  const exports = vec([
    [...name('memory'), 0x02, 0],
    [...name('web3d_create'), 0x00, 0],
    [...name('web3d_init'), 0x00, 1],
    [...name('web3d_error_ptr'), 0x00, 2],
    [...name('web3d_error_len'), 0x00, 3],
    [...name('web3d_destroy'), 0x00, 4],
  ]);
  const body = (code) => [...leb(code.length + 1), 0, ...code]; // no locals
  // i32.const takes a signed LEB128.
  const sleb = (n) => {
    const out = [];
    for (;;) {
      const b = n & 0x7f;
      n >>= 7;
      const done = (n === 0 && (b & 0x40) === 0) || (n === -1 && (b & 0x40) !== 0);
      out.push(done ? b : b | 0x80);
      if (done) return out;
    }
  };
  const i32 = (n) => [0x41, ...sleb(n)];
  const code = vec([
    body([...i32(16), 0x0b]),
    body([...i32(1), 0x0b]),
    body([...i32(64), 0x0b]),
    body([...i32(message.length), 0x0b]),
    body([0x0b]),
  ]);
  const data = vec([[0x00, ...i32(64), 0x0b, ...leb(message.length), ...message]]);
  const bytes = new Uint8Array([
    0, 0x61, 0x73, 0x6d, 1, 0, 0, 0,
    ...section(1, types),
    ...section(3, funcs),
    ...section(5, memory),
    ...section(7, exports),
    ...section(10, code),
    ...section(11, data),
  ]);
  return new WebAssembly.Module(bytes);
}

/** K2: the documented recovery from a context loss, with the default options (antialias true). */
async function lossDefault() {
  const canvas = document.getElementById('loss2');
  let restoredResolve;
  const restored = new Promise((res) => (restoredResolve = res));
  const old = await createRenderer(canvas, { width: 32, height: 32, onContextRestored: () => restoredResolve('restored') });
  const lost = new Promise((res) => canvas.addEventListener('webglcontextlost', () => setTimeout(res, 0), { once: true }));
  const lc = old.gl.getExtension('WEBGL_lose_context');
  lc.loseContext();
  await Promise.race([lost, timeout(2000)]);
  const d = {};
  const during = await settle(createRenderer(canvas, { width: 32, height: 32 }));
  d.during = during.ok ? 'created' : during.message;
  lc.restoreContext();
  d.restored = await Promise.race([restored, timeout(3000)]);
  old.dispose();
  const after = await settle(createRenderer(canvas, { width: 32, height: 32 }));
  d.after = after.ok ? 'created' : after.message;
  if (after.ok) {
    const r = after.r;
    d.antialias = r.info.antialias;
    flat(r);
    r.createObject(r.createMesh(quad(-1, -1, 1, 1, -5)), r.createMaterial({ baseColor: [0, 1, 0, 1] }));
    r.frame();
    d.pixel = reader(r)(0, 0);
    d.glError = r.gl.getError();
    r.dispose();
  }
  log(
    'K2 with default options: a create during a loss says the context is lost; after the restore and dispose(), a default create works and draws',
    /context is lost/.test(d.during) && d.restored === 'restored' && d.after === 'created' && d.antialias === true && d.pixel?.[1] > 200 && d.glError === 0,
    d,
  );
}

/** K3: a context lost while the module is instantiated: the create says so, and the canvas recovers. */
async function lossDuringCreate() {
  const c = document.getElementById('loss3');
  const gl = c.getContext('webgl2');
  const lose = gl.getExtension('WEBGL_lose_context');
  const real = WebAssembly.instantiate;
  // A GPU reset just after instantiation: the test hooks a built-in for one call.
  WebAssembly.instantiate = async (...args) => {
    const result = await real.apply(WebAssembly, args);
    lose.loseContext();
    return result;
  };
  const d = {};
  let hook = 0;
  try {
    const first = await settle(createRenderer(c, { width: 16, height: 16, onContextLost: () => hook++ }));
    d.first = first.ok ? 'created' : first.message;
  } finally {
    WebAssembly.instantiate = real;
  }
  await timeout(50);
  const restored = new Promise((res) => c.addEventListener('webglcontextrestored', () => res('restored'), { once: true }));
  d.restoreThrew = attempt(() => lose.restoreContext()).threw;
  d.restored = await Promise.race([restored, timeout(2000)]);
  const again = await settle(createRenderer(c, { width: 16, height: 16 }));
  d.again = again.ok ? 'created' : again.message;
  if (again.ok) {
    flat(again.r);
    again.r.createObject(again.r.createMesh(quad(-1, -1, 1, 1, -5)), again.r.createMaterial({ baseColor: [0, 1, 0, 1] }));
    again.r.frame();
    d.pixel = reader(again.r)(0, 0);
    again.r.dispose();
  }
  d.hook = hook;
  log(
    'K3 a context lost during the create: the error says the context is lost, the context can be restored, and the next create draws',
    /context is lost/.test(d.first) && !d.restoreThrew && d.restored === 'restored' && d.again === 'created' && d.pixel?.[1] > 200 && hook === 0,
    d,
  );
}

/** G: a page-made context without a depth buffer is refused; texture sources sized by what is uploaded. */
async function contextAndSources() {
  const d = {};
  const c = document.getElementById('nodepth');
  c.getContext('webgl2', { depth: false });
  const nodepth = await settle(createRenderer(c, { width: 16, height: 16 }));
  d.noDepth = nodepth.ok ? 'created' : nodepth.message;
  const r = await createRenderer(document.getElementById('sources'), { width: 16, height: 16, antialias: false });
  flat(r);
  const img = new Image();
  img.src = 'data:image/gif;base64,R0lGODlhAQABAIAAAP///wAAACH5BAEAAAAALAAAAAABAAEAAAICRAEAOw==';
  await img.decode();
  d.image = attempt(() => r.createTexture(img));
  // A frame coded 4x4 whose display size is 8x8: WebGL uploads its 4x4 visible rectangle.
  const green = new Uint8Array(4 * 4 * 4);
  for (let i = 0; i < 16; i++) green.set([0, 255, 0, 255], i * 4);
  const frame = new VideoFrame(green, { format: 'RGBA', codedWidth: 4, codedHeight: 4, displayWidth: 8, displayHeight: 8, timestamp: 0 });
  d.frame = attempt(() => r.createTexture(frame));
  d.errorAfterFrame = r.gl.getError();
  if (!d.frame.threw) {
    r.createObject(r.createMesh(quad(-1, -1, 1, 1, -5)), r.createMaterial({ texture: d.frame.value }));
    r.frame();
    d.pixels = [reader(r)(-0.9, -0.9), reader(r)(0.9, 0.9)];
  }
  frame.close();
  d.glError = r.gl.getError();
  r.dispose();
  log(
    'CX a context without depth is refused; an <img> is refused (decode it first); a VideoFrame is sized by its visible rectangle and fills the texture',
    /no depth buffer/.test(d.noDepth) && d.image.threw && /decode it first/.test(d.image.message) && !d.frame.threw && d.errorAfterFrame === 0 &&
      d.pixels.every((p) => p[1] > 200 && p[0] < 60) && d.glError === 0,
    d,
  );
}

/**
 * K4: the recovery exactly as the LOST message words it: dispose in onContextLost, then create a new
 * renderer when onContextRestored comes. Also: frame() is false from the moment of the loss, and a
 * lost renderer cannot resize a canvas a new renderer owns.
 */
async function lossAsDocumented() {
  const canvas = document.getElementById('loss4');
  const d = { restoredCalls: 0 };
  let restoredResolve;
  const restored = new Promise((res) => (restoredResolve = res));
  let r = null;
  const make = (opts) =>
    createRenderer(canvas, {
      width: 32,
      height: 32,
      ...opts,
      onContextLost: () => r?.dispose(),
      onContextRestored: () => (d.restoredCalls++, restoredResolve('restored')),
    });
  r = await make({});
  const lc = r.gl.getExtension('WEBGL_lose_context');
  lc.loseContext();
  d.frameRightAfter = attempt(() => r.frame()); // before the event: already false
  const before = r;
  await timeout(50); // the event has run: onContextLost disposed the renderer
  d.disposed = attempt(() => before.frame());
  lc.restoreContext();
  d.restored = await Promise.race([restored, timeout(3000)]);
  const next = await settle(make({}));
  d.next = next.ok ? 'created' : next.message;
  if (next.ok) {
    r = next.r;
    flat(r);
    r.createObject(r.createMesh(quad(-1, -1, 1, 1, -5)), r.createMaterial({ baseColor: [0, 1, 0, 1] }));
    r.frame();
    d.pixel = reader(r)(0, 0);
  }
  // A second loss, the old renderer left undisposed: a new one takes the canvas, and the old one
  // cannot resize it.
  const c2 = document.getElementById('loss5');
  const old = await createRenderer(c2, { width: 32, height: 32, antialias: false });
  const lc2 = old.gl.getExtension('WEBGL_lose_context');
  const lost2 = new Promise((res) => c2.addEventListener('webglcontextlost', () => setTimeout(res, 0), { once: true }));
  const restored2 = new Promise((res) => c2.addEventListener('webglcontextrestored', () => res('restored'), { once: true }));
  lc2.loseContext();
  await lost2;
  lc2.restoreContext();
  await Promise.race([restored2, timeout(3000)]);
  const fresh = await createRenderer(c2, { width: 32, height: 32, antialias: false });
  d.oldResize = attempt(() => old.resize(8, 8));
  d.sizeAfter = [c2.width, c2.height];
  fresh.dispose();
  old.dispose();
  r.dispose();
  log(
    'K4 dispose in onContextLost, then onContextRestored still comes and a new renderer draws; frame() is false at once after a loss; a lost renderer cannot resize a canvas a new one owns',
    d.frameRightAfter.value === false && d.disposed.threw && /disposed/.test(d.disposed.message) && d.restored === 'restored' &&
      d.restoredCalls === 1 && d.next === 'created' && d.pixel?.[1] > 200 && d.oldResize.threw && /context is lost/.test(d.oldResize.message) &&
      d.sizeAfter[0] === 32 && d.sizeAfter[1] === 32,
    d,
  );
}

/** T3: a page's pending WebGL error is not taken for an upload's; DataView bytes compile; turned and bitmaprenderer sources. */
async function uploadsAndBytes() {
  const r = await createRenderer(document.getElementById('pending'), { width: 16, height: 16, antialias: false });
  flat(r);
  const d = { warnings: [] };
  const warn = console.warn;
  console.warn = (...a) => (d.warnings.push(String(a[0])), warn.apply(console, a));
  try {
    r.gl.enable(0x1234); // INVALID_ENUM, left pending by "the page"
    r.gl.lineWidth(-1); // and INVALID_VALUE (one flag a kind of error)
    out.expectedPending = (out.expectedPending ?? 0) + 2; // check.mjs allows exactly these
    const bitmap = await bitmap4(0, 255, 0);
    d.withPending = attempt(() => r.createTexture(bitmap));
  } finally {
    console.warn = warn;
  }
  d.errorAfter = r.gl.getError();
  // A VideoFrame turned 90 degrees, 4x2 coded: sized 2x4 if the browser turns it, else 4x2.
  const px = new Uint8Array(4 * 2 * 4).fill(255);
  let turned = null;
  try {
    turned = new VideoFrame(px, { format: 'RGBA', codedWidth: 4, codedHeight: 2, rotation: 90, timestamp: 0 });
  } catch (e) {
    d.turnedUnsupported = String(e?.message ?? e);
  }
  if (turned) {
    d.turned = attempt(() => r.createTexture(turned));
    turned.close();
  }
  d.errorAfterTurned = r.gl.getError();
  // A bitmaprenderer canvas, smaller and larger than its bitmap: refused before any upload.
  const small = document.createElement('canvas');
  small.width = 2;
  small.height = 2;
  small.getContext('bitmaprenderer').transferFromImageBitmap(await bitmap4(255, 0, 0));
  d.bitmaprendererSmaller = attempt(() => r.createTexture(small));
  const large = new OffscreenCanvas(16, 16);
  large.getContext('bitmaprenderer').transferFromImageBitmap(await bitmap4(255, 0, 0));
  d.bitmaprendererLarger = attempt(() => r.createTexture(large));
  // A canvas with no context yet gets a blank 2d one and uploads.
  d.blankCanvas = attempt(() => r.createTexture(new OffscreenCanvas(4, 4)));
  d.errorAfterCanvases = r.gl.getError();
  r.dispose();
  // The module's bytes as a DataView.
  const bytes = await (await fetch('../pkg/loadngo_web3d.wasm')).arrayBuffer();
  const viaView = await settle(createRenderer(document.getElementById('dataview'), { width: 16, height: 16, antialias: false, wasm: new DataView(bytes) }));
  d.dataView = viaView.ok ? 'created' : viaView.message;
  if (viaView.ok) viaView.r.dispose();
  const gone = new ArrayBuffer(8);
  const goneView = new DataView(gone);
  structuredClone(gone, { transfer: [gone] }); // detaches it
  const detached = await settle(createRenderer(document.getElementById('dataview'), { width: 16, height: 16, antialias: false, wasm: goneView }));
  d.detached = detached.ok ? 'created' : detached.message;
  log(
    'TU pending page errors are reported as pending and do not refuse a good texture; a turned VideoFrame uploads at its turned size; bitmaprenderer canvases are refused, a blank canvas uploads; a DataView of the module compiles, a detached one is refused',
    !d.withPending.threw && d.warnings.filter((w) => /was pending before a texture upload/.test(w)).length === 2 && d.errorAfter === 0 &&
      (d.turned ? !d.turned.threw : true) && d.errorAfterTurned === 0 &&
      [d.bitmaprendererSmaller, d.bitmaprendererLarger].every((t) => t.threw && /bitmaprenderer/.test(t.message) && /ImageBitmap itself/.test(t.message)) &&
      !d.blankCanvas.threw && d.errorAfterCanvases === 0 && d.dataView === 'created' && /detached/.test(d.detached),
    d,
  );
}

const bitmap4 = (r, g, b) => {
  const data = new Uint8ClampedArray(4 * 4 * 4);
  for (let i = 0; i < 16; i++) data.set([r, g, b, 255], i * 4);
  return createImageBitmap(new ImageData(data, 4, 4), { premultiplyAlpha: 'none', colorSpaceConversion: 'none' });
};

/**
 * LE: the loss controller, one a canvas. E1, a throwing restore hook does not stop the others; E4b,
 * one function given twice runs once; E6, a lost renderer left undisposed hears no later loss or
 * restore; E11, a loss during a create on a page-made context can be restored; E12, a canvas lost
 * while no renderer holds it can be restored; ET, a loss and restore that both fall inside a create
 * call that create's hook once, after it fails.
 */
async function lossController() {
  const d = {};
  const lose = (gl) => gl.getExtension('WEBGL_lose_context');
  const event = (c, type) => new Promise((res) => c.addEventListener(type, () => setTimeout(() => res(type), 0), { once: true }));
  const opts = { width: 16, height: 16, antialias: false };
  // A hook that throws on purpose: reported through reportError, which this listener cancels so the
  // page does not count it as an error.
  const PURPOSE = 'web3d probe: a restore hook that throws on purpose';
  const quiet = (e) => {
    if (e.error?.message === PURPOSE) e.preventDefault();
  };
  window.addEventListener('error', quiet);
  try {
    // E1: A disposed while lost (its hook throws), then a create that fails as lost: both hooks run.
    {
      const c = document.getElementById('le1');
      const calls = { a: 0, b: 0 };
      const a = await createRenderer(c, { ...opts, onContextRestored: () => (calls.a++, (() => { throw new Error(PURPOSE); })()) });
      const lc = lose(a.gl);
      const lost = event(c, 'webglcontextlost');
      lc.loseContext();
      await lost;
      a.dispose();
      d.e1Create = (await settle(createRenderer(c, { ...opts, onContextRestored: () => calls.b++ }))).message;
      const restored = event(c, 'webglcontextrestored');
      lc.restoreContext();
      await restored;
      d.e1 = calls;
    }
    // E4b: one function, given to a live renderer and to a create that fails on its lost canvas.
    {
      const c = document.getElementById('le4');
      let calls = 0;
      const hook = () => calls++;
      const r = await createRenderer(c, { ...opts, onContextRestored: hook });
      const lc = lose(r.gl);
      const lost = event(c, 'webglcontextlost');
      lc.loseContext();
      await lost;
      d.e4bCreate = (await settle(createRenderer(c, { ...opts, onContextRestored: hook }))).message;
      const restored = event(c, 'webglcontextrestored');
      lc.restoreContext();
      await restored;
      d.e4b = calls;
      r.dispose();
    }
    // E6: a lost renderer left undisposed, then a new one on the restored canvas, then a second loss.
    {
      const c = document.getElementById('le6');
      const old = { lost: 0, restored: 0 };
      const now = { lost: 0, restored: 0 };
      const first = await createRenderer(c, { ...opts, onContextLost: () => old.lost++, onContextRestored: () => old.restored++ });
      const lc = lose(first.gl);
      let second = null;
      for (const round of [1, 2]) {
        if (round === 2) {
          d.e6Second = await createRenderer(c, { ...opts, onContextLost: () => now.lost++, onContextRestored: () => now.restored++ }).then(
            (r) => ((second = r), 'created'),
            (e) => e.message,
          );
        }
        const lost = event(c, 'webglcontextlost');
        lc.loseContext();
        await lost;
        const restored = event(c, 'webglcontextrestored');
        lc.restoreContext();
        await restored;
      }
      d.e6 = { old, now };
      second?.dispose();
      first.dispose();
    }
    // E11: a page-made context, lost while a create instantiates the module.
    {
      const c = document.getElementById('le11');
      const gl = c.getContext('webgl2', { antialias: false });
      const lc = lose(gl);
      const real = WebAssembly.instantiate;
      WebAssembly.instantiate = async (...args) => {
        const result = await real.apply(WebAssembly, args);
        lc.loseContext();
        return result;
      };
      let calls = 0;
      try {
        d.e11Create = (await settle(createRenderer(c, { ...opts, onContextRestored: () => calls++ }))).message;
      } finally {
        WebAssembly.instantiate = real;
      }
      await timeout(50);
      const restored = event(c, 'webglcontextrestored');
      d.e11RestoreThrew = attempt(() => lc.restoreContext()).threw;
      d.e11Restored = await Promise.race([restored, timeout(2000)]);
      d.e11Calls = calls;
      const again = await settle(createRenderer(c, opts));
      d.e11Again = again.ok ? 'created' : again.message;
      if (again.ok) again.r.dispose();
    }
    // E12: a canvas web3d used, then left with no renderer, lost and restored.
    {
      const c = document.getElementById('le12');
      const r = await createRenderer(c, opts);
      const gl = r.gl;
      r.dispose();
      const lc = lose(gl);
      const lost = event(c, 'webglcontextlost');
      lc.loseContext();
      await lost;
      const restored = event(c, 'webglcontextrestored');
      d.e12RestoreThrew = attempt(() => lc.restoreContext()).threw;
      d.e12Restored = await Promise.race([restored, timeout(2000)]);
      const again = await settle(createRenderer(c, opts));
      d.e12Again = again.ok ? 'created' : again.message;
      if (again.ok) again.r.dispose();
    }
    // ET: a loss and its restore both inside the create's instantiation.
    {
      const c = document.getElementById('let');
      const r0 = await createRenderer(c, opts); // the canvas has a controller and a context
      const gl = r0.gl;
      r0.dispose();
      const lc = lose(gl);
      const real = WebAssembly.instantiate;
      WebAssembly.instantiate = async (...args) => {
        const result = await real.apply(WebAssembly, args);
        const lost = event(c, 'webglcontextlost');
        lc.loseContext();
        await lost;
        const restored = event(c, 'webglcontextrestored');
        lc.restoreContext();
        await restored;
        return result;
      };
      let calls = 0;
      try {
        d.etCreate = (await settle(createRenderer(c, { ...opts, onContextRestored: () => calls++ }))).message;
      } finally {
        WebAssembly.instantiate = real;
      }
      d.etCallsAtOnce = calls;
      await timeout(20);
      d.etCalls = calls;
      const again = await settle(createRenderer(c, opts));
      d.etAgain = again.ok ? 'created' : again.message;
      if (again.ok) again.r.dispose();
    }
  } finally {
    window.removeEventListener('error', quiet);
  }
  const lostMessage = (m) => /context is lost/.test(m ?? '');
  log(
    'LE one loss controller a canvas: a throwing hook does not stop the next (E1); one function runs once (E4b); a lost renderer left undisposed hears no later loss (E6); a page-made context lost during a create, and a canvas lost with no renderer, are restored (E11, E12); a loss and restore inside a create call its hook once, after (ET)',
    lostMessage(d.e1Create) && d.e1.a === 1 && d.e1.b === 1 &&
      lostMessage(d.e4bCreate) && d.e4b === 1 &&
      d.e6Second === 'created' && d.e6.old.lost === 1 && d.e6.old.restored === 1 && d.e6.now.lost === 1 && d.e6.now.restored === 1 &&
      lostMessage(d.e11Create) && !d.e11RestoreThrew && d.e11Restored === 'webglcontextrestored' && d.e11Calls === 1 && d.e11Again === 'created' &&
      !d.e12RestoreThrew && d.e12Restored === 'webglcontextrestored' && d.e12Again === 'created' &&
      lostMessage(d.etCreate) && d.etCallsAtOnce === 0 && d.etCalls === 1 && d.etAgain === 'created',
    d,
  );
}

/** H7: at the largest serial, handles stay below 2^50, where a handle plus a half is still a fraction. */
async function largestSerial() {
  const pool = globalThis[Symbol.for('loadngo-web3d')].serials;
  pool.next = 2 ** 20 - 1; // the counter's last serial; the next one wraps round to 1
  const c = document.getElementById('serial-max');
  const r = await createRenderer(c, { width: 16, height: 16, antialias: false });
  flat(r);
  const o = r.createObject(r.createMesh(quad(-1, -1, 1, 1, -5)), r.createMaterial({ baseColor: [0, 1, 0, 1] }));
  const d = { serial: r._serial, o, belowLimit: o < 2 ** 50, safe: Number.isSafeInteger(o) };
  d.half = attempt(() => r.setVisible(o + 0.5, false));
  d.quarter = attempt(() => r.setVisible(o - 0.25, false));
  d.limit = attempt(() => r.setVisible(2 ** 50, false));
  r.frame();
  d.pixel = reader(r)(0, 0);
  r.dispose();
  const next = await createRenderer(c, { width: 16, height: 16, antialias: false });
  d.nextSerial = next._serial;
  next.dispose();
  log(
    'H7 at the largest serial a handle is below 2^50, a handle plus 0.5 or minus 0.25 is refused as a fraction, and the next serial wraps round to 1',
    d.serial === 2 ** 20 - 1 && d.belowLimit && d.safe && d.half.threw && d.quarter.threw && d.limit.threw &&
      [d.half, d.quarter, d.limit].every((t) => /is not a valid object handle/.test(t.message)) && d.pixel[1] > 200 && d.nextSerial === 1,
    d,
  );
}

/** R: another realm (a same-origin iframe) runs its own copy of the module but shares the top window's serial counter, so its handles are not this realm's. */
async function realms() {
  const iframe = document.createElement('iframe');
  iframe.src = './probe-frame.html';
  document.body.append(iframe);
  await new Promise((res) => iframe.addEventListener('load', res, { once: true }));
  for (let i = 0; i < 50 && !iframe.contentWindow.__frame; i++) await timeout(20);
  const f = iframe.contentWindow.__frame;
  const theirs = await f.createRenderer(f.canvas, { width: 16, height: 16, antialias: false });
  const ours = await createRenderer(document.getElementById('realm'), { width: 16, height: 16, antialias: false });
  flat(ours);
  const ourObject = ours.createObject(ours.createMesh(quad(-1, -1, 1, 1, -5)), ours.createMaterial({ baseColor: [0, 1, 0, 1] }));
  const theirObject = theirs.createObject(theirs.createMesh(quad(-1, -1, 1, 1, -5)), theirs.createMaterial({}));
  const d = { ourSerial: ours._serial, theirSerial: theirs._serial, ourObject, theirObject };
  d.theirsOnOurs = attempt(() => ours.setVisible(theirObject, false));
  ours.frame();
  d.pixel = reader(ours)(0, 0);
  theirs.dispose();
  ours.dispose();
  iframe.remove();
  log(
    'RM a same-origin iframe\'s renderer takes the next serial from the shared counter, and its handle is refused here',
    d.ourSerial === (d.theirSerial % (2 ** 20 - 1)) + 1 && d.ourObject !== d.theirObject && d.theirsOnOurs.threw && d.pixel[1] > 200,
    d,
  );
}

// The tenth round's probes: the ninth critic's findings N1 (LR), N8 (PS), N10 (RF) and N11 (NC),
// and its notes N7b' and N21 (MS). Each makes its own canvases, and releases their contexts when it
// is done, so Chrome's limit on live contexts drops none of the others.

/** A canvas in the document with an id (the check counts its listeners by id). */
function canvasFor(id, w = 16, h = 16) {
  const c = document.createElement('canvas');
  c.id = id;
  c.width = w;
  c.height = h;
  document.body.append(c);
  return c;
}
/** Releases a context a probe is done with (its loss is cancelled by web3d, which is harmless). */
const release = (gl) => {
  if (gl && !gl.isContextLost()) gl.getExtension('WEBGL_lose_context')?.loseContext();
};
const eventOf = (target, type) => new Promise((res) => target.addEventListener(type, () => setTimeout(() => res(type), 0), { once: true }));
const isGreen = (p) => Array.isArray(p) && p[1] > 200 && p[0] < 70 && p[2] < 70;
/** Draws a green quad over the whole canvas and reads its centre, in the same task. */
const greenOn = (r) => {
  flat(r);
  r.createObject(r.createMesh(quad(-1, -1, 1, 1, -5)), r.createMaterial({ baseColor: [0, 1, 0, 1] }));
  r.frame();
  return reader(r)(0, 0);
};
/** The pixel at a point of the canvas's own drawing buffer, whatever framebuffer is bound for reading. */
const canvasPixel = (gl, nx, ny) => {
  const prior = gl.getParameter(gl.READ_FRAMEBUFFER_BINDING);
  gl.bindFramebuffer(gl.READ_FRAMEBUFFER, null);
  const px = new Uint8Array(4);
  gl.readPixels(Math.floor(((nx + 1) / 2) * gl.drawingBufferWidth), Math.floor(((ny + 1) / 2) * gl.drawingBufferHeight), 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, px);
  gl.bindFramebuffer(gl.READ_FRAMEBUFFER, prior);
  return [px[0], px[1], px[2]];
};
/** Leaves transform feedback active on `gl`, as a page that stopped mid-capture would. */
function beginTransformFeedback(gl) {
  const shader = (type, source) => {
    const sh = gl.createShader(type);
    gl.shaderSource(sh, source);
    gl.compileShader(sh);
    return sh;
  };
  const p = gl.createProgram();
  gl.attachShader(p, shader(gl.VERTEX_SHADER, '#version 300 es\nout float v_out;\nvoid main() { v_out = 1.0; gl_Position = vec4(0.0, 0.0, 0.0, 1.0); gl_PointSize = 1.0; }'));
  gl.attachShader(p, shader(gl.FRAGMENT_SHADER, '#version 300 es\nprecision mediump float;\nout vec4 o;\nvoid main() { o = vec4(0.0); }'));
  gl.transformFeedbackVaryings(p, ['v_out'], gl.SEPARATE_ATTRIBS);
  gl.linkProgram(p);
  gl.useProgram(p);
  gl.bindTransformFeedback(gl.TRANSFORM_FEEDBACK, gl.createTransformFeedback());
  const b = gl.createBuffer();
  gl.bindBufferBase(gl.TRANSFORM_FEEDBACK_BUFFER, 0, b);
  gl.bufferData(gl.TRANSFORM_FEEDBACK_BUFFER, 64, gl.STREAM_COPY);
  gl.beginTransformFeedback(gl.POINTS);
  return { active: gl.getParameter(gl.TRANSFORM_FEEDBACK_ACTIVE) };
}

/**
 * PS: a page-made context that suits (a depth buffer, the antialias asked for), handed over with
 * state the page left. web3d puts WebGL 2's default state back, so it draws and uploads as on a new
 * context: the ninth critic's N8 (its nine draw states and three upload states), widened to the rest
 * of what src/renderer.rs resets that changes what is drawn, uploaded or read back. Every case reads
 * the canvas's own drawing buffer, raises no WebGL error, and (for objects the page made) leaves
 * them as they were, only unbound.
 */
async function pageState() {
  const opts = { width: 16, height: 16, antialias: false };
  const rgbw = () => bitmap(2, 2, [255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255]);
  const greenRed = () => bitmap(2, 1, [0, 255, 0, 255, 255, 0, 0, 255]);
  // Draws for a case; returns what to check. Bitmaps are made before, so this runs in one task.
  const draw = {
    green: (r, gl) => {
      flat(r);
      r.createObject(r.createMesh(quad(-1, -1, 1, 1, -5)), r.createMaterial({ baseColor: [0, 1, 0, 1] }));
      r.frame();
      return { pixel: canvasPixel(gl, 0, 0), ok: (px) => isGreen(px) };
    },
    // An opaque material of alpha 0: drawn whole, unless alpha-to-coverage turns its alpha into no samples.
    alpha0: (r, gl) => {
      flat(r);
      r.createObject(r.createMesh(quad(-1, -1, 1, 1, -5)), r.createMaterial({ baseColor: [0, 1, 0, 0] }));
      r.frame();
      return { pixel: canvasPixel(gl, 0, 0), ok: (px) => isGreen(px) };
    },
    // A blended material of alpha 1 over black: green by addition, black by reverse subtraction.
    blend: (r, gl) => {
      flat(r);
      r.createObject(r.createMesh(quad(-1, -1, 1, 1, -5)), r.createMaterial({ baseColor: [0, 1, 0, 1], alphaMode: 'blend' }));
      r.frame();
      return { pixel: canvasPixel(gl, 0, 0), ok: (px) => isGreen(px) };
    },
    // A mipmapped 2x2 texture: its top-left texel is red; a sampler with TEXTURE_MIN_LOD 10 would
    // sample the 1x1 level, the four texels' average, everywhere.
    mip: (r, gl, b) => {
      flat(r);
      const t = r.createTexture(b, { wrapS: 'clamp', wrapT: 'clamp' });
      r.createObject(r.createMesh(quad(-1, -1, 1, 1, -5)), r.createMaterial({ texture: t }));
      r.frame();
      return { pixel: canvasPixel(gl, -0.75, 0.75), ok: (px) => px[0] > 200 && px[1] < 70 && px[2] < 70 };
    },
    // A textured mesh without uvs reads the uv attribute's generic value, (0, 0) by default: the
    // green texel; at (0.9, 0.5) it would read the red one.
    attrib: (r, gl, b) => {
      flat(r);
      const t = r.createTexture(b, { mipmaps: false, wrapS: 'clamp', wrapT: 'clamp' });
      const { positions, normals, indices } = quad(-1, -1, 1, 1, -5);
      r.createObject(r.createMesh({ positions, normals, indices }), r.createMaterial({ texture: t }));
      r.frame();
      return { pixel: canvasPixel(gl, 0, 0), ok: (px) => isGreen(px) };
    },
    // An upload: the 2x2 texture upright, every texel where it belongs.
    upload: (r, gl, b) => {
      flat(r);
      const t = r.createTexture(b, { mipmaps: false, wrapS: 'clamp', wrapT: 'clamp' });
      r.createObject(r.createMesh(quad(-1, -1, 1, 1, -5)), r.createMaterial({ texture: t }));
      r.frame();
      // Inside the edge texels, which clamping keeps pure under linear filtering.
      const tl = canvasPixel(gl, -0.75, 0.75);
      const tr = canvasPixel(gl, 0.75, 0.75);
      const bl = canvasPixel(gl, -0.75, -0.75);
      const only = (px, c) => px.every((v, i) => (i === c ? v > 200 : v < 70));
      return { pixel: { tl, tr, bl }, ok: () => only(tl, 0) && only(tr, 1) && only(bl, 2) };
    },
  };
  const cases = [
    ['scissor test on, a 1x1 box at the corner', {}, (gl) => (gl.enable(gl.SCISSOR_TEST), gl.scissor(0, 0, 1, 1))],
    ['colour mask all false', {}, (gl) => gl.colorMask(false, false, false, false)],
    ['depth func NEVER', {}, (gl) => gl.depthFunc(gl.NEVER)],
    ['cull face FRONT', {}, (gl) => gl.cullFace(gl.FRONT)],
    ['clear depth 0', {}, (gl) => gl.clearDepth(0)],
    ['depth range 1 to 1', {}, (gl) => gl.depthRange(1, 1)],
    ['rasterizer discard on', {}, (gl) => gl.enable(gl.RASTERIZER_DISCARD)],
    ['draw buffers NONE on the default framebuffer', {}, (gl) => gl.drawBuffers([gl.NONE])],
    ['read buffer NONE on the default framebuffer', {}, (gl) => gl.readBuffer(gl.NONE)],
    [
      'a framebuffer of its own left bound',
      {},
      (gl, keep) => {
        const fb = gl.createFramebuffer();
        const tex = gl.createTexture();
        gl.bindTexture(gl.TEXTURE_2D, tex);
        gl.texStorage2D(gl.TEXTURE_2D, 1, gl.RGBA8, 16, 16);
        gl.bindFramebuffer(gl.FRAMEBUFFER, fb);
        gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.TEXTURE_2D, tex, 0);
        gl.bindTexture(gl.TEXTURE_2D, null);
        // Kept, only unbound: its attachment is still its texture afterwards.
        keep.check = () => {
          gl.bindFramebuffer(gl.FRAMEBUFFER, fb);
          const same = gl.getFramebufferAttachmentParameter(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.FRAMEBUFFER_ATTACHMENT_OBJECT_NAME) === tex;
          gl.bindFramebuffer(gl.FRAMEBUFFER, null);
          return same;
        };
      },
    ],
    ['stencil test on with func NEVER, on a context with stencil', { stencil: true }, (gl) => (gl.enable(gl.STENCIL_TEST), gl.stencilFunc(gl.NEVER, 0, 0xff))],
    ['polygon offset fill on, pushing depth past the far plane', {}, (gl) => (gl.enable(gl.POLYGON_OFFSET_FILL), gl.polygonOffset(0, 1e9))],
    ['sample coverage on at 0, multisampled', { antialias: true }, (gl) => (gl.enable(gl.SAMPLE_COVERAGE), gl.sampleCoverage(0, false))],
    ['alpha to coverage on, multisampled, an opaque material of alpha 0', { antialias: true }, (gl) => gl.enable(gl.SAMPLE_ALPHA_TO_COVERAGE), 'alpha0'],
    ['blend equation FUNC_REVERSE_SUBTRACT, a blended material', {}, (gl) => gl.blendEquation(gl.FUNC_REVERSE_SUBTRACT), 'blend'],
    [
      'a sampler object on unit 0 with TEXTURE_MIN_LOD 10, a mipmapped texture',
      {},
      (gl) => {
        const sampler = gl.createSampler();
        gl.samplerParameterf(sampler, gl.TEXTURE_MIN_LOD, 10);
        gl.bindSampler(0, sampler);
      },
      'mip',
      rgbw,
    ],
    ["the uv attribute's generic value at (0.9, 0.5), a textured mesh without uvs", {}, (gl) => gl.vertexAttrib4f(2, 0.9, 0.5, 0, 1), 'attrib', greenRed],
    ['transform feedback left active', {}, (gl, keep) => (keep.before = beginTransformFeedback(gl))],
    [
      'a TIME_ELAPSED query left running',
      {},
      (gl, keep) => {
        const ext = gl.getExtension('EXT_disjoint_timer_query_webgl2');
        keep.timer = Boolean(ext);
        if (ext) gl.beginQuery(ext.TIME_ELAPSED_EXT, gl.createQuery());
      },
    ],
    [
      'an occlusion query left running',
      {},
      (gl, keep) => {
        gl.beginQuery(gl.ANY_SAMPLES_PASSED, gl.createQuery());
        keep.check = () => gl.getQuery(gl.ANY_SAMPLES_PASSED, gl.CURRENT_QUERY) === null;
      },
    ],
    [
      'a vertex array of its own left bound, holding an index buffer',
      {},
      (gl, keep) => {
        const vao = gl.createVertexArray();
        const b = gl.createBuffer();
        gl.bindVertexArray(vao);
        gl.bindBuffer(gl.ELEMENT_ARRAY_BUFFER, b);
        // Kept as it was: web3d's index buffers went into its own vertex arrays.
        keep.check = () => {
          gl.bindVertexArray(vao);
          const same = gl.getParameter(gl.ELEMENT_ARRAY_BUFFER_BINDING) === b;
          gl.bindVertexArray(null);
          return same;
        };
      },
    ],
    ['a pixel pack buffer bound and PACK_SKIP_PIXELS 3 (the readback here would fail)', {}, (gl) => (gl.bindBuffer(gl.PIXEL_PACK_BUFFER, gl.createBuffer()), gl.pixelStorei(gl.PACK_SKIP_PIXELS, 3))],
    ['UNPACK_SKIP_PIXELS 1, an upload', {}, (gl) => gl.pixelStorei(gl.UNPACK_SKIP_PIXELS, 1), 'upload', rgbw],
    ['UNPACK_SKIP_ROWS 1, an upload', {}, (gl) => gl.pixelStorei(gl.UNPACK_SKIP_ROWS, 1), 'upload', rgbw],
    ['UNPACK_ROW_LENGTH 1, an upload', {}, (gl) => gl.pixelStorei(gl.UNPACK_ROW_LENGTH, 1), 'upload', rgbw],
    ['a pixel unpack buffer left bound, an upload', {}, (gl) => gl.bindBuffer(gl.PIXEL_UNPACK_BUFFER, gl.createBuffer()), 'upload', rgbw],
    ['UNPACK_FLIP_Y_WEBGL and UNPACK_PREMULTIPLY_ALPHA_WEBGL on, an upload', {}, (gl) => (gl.pixelStorei(gl.UNPACK_FLIP_Y_WEBGL, true), gl.pixelStorei(gl.UNPACK_PREMULTIPLY_ALPHA_WEBGL, true)), 'upload', rgbw],
    // State WebGL 2 extensions add, where the browser has the extension (else the case is skipped).
    [
      'EXT_clip_control: an upper-left origin and depth from 0 to 1',
      {},
      (gl, keep) => {
        const x = gl.getExtension('EXT_clip_control');
        if (!x) return (keep.unsupported = true);
        x.clipControlEXT(x.UPPER_LEFT_EXT, x.ZERO_TO_ONE_EXT);
      },
    ],
    [
      'EXT_depth_clamp on',
      {},
      (gl, keep) => {
        const x = gl.getExtension('EXT_depth_clamp');
        if (!x) return (keep.unsupported = true);
        gl.enable(x.DEPTH_CLAMP_EXT);
        keep.check = () => !gl.isEnabled(x.DEPTH_CLAMP_EXT);
      },
    ],
    [
      'WEBGL_clip_cull_distance: CLIP_DISTANCE0 on (web3d writes no clip distance)',
      {},
      (gl, keep) => {
        const x = gl.getExtension('WEBGL_clip_cull_distance');
        if (!x) return (keep.unsupported = true);
        gl.enable(x.CLIP_DISTANCE0_WEBGL);
        keep.check = () => !gl.isEnabled(x.CLIP_DISTANCE0_WEBGL);
      },
    ],
    ['nothing changed (control), a draw', {}, () => {}],
    ['nothing changed (control), an upload', {}, () => {}, 'upload', rgbw],
  ];
  const results = [];
  let n = 0;
  for (const [name, attributes, leave, kind = 'green', source = null] of cases) {
    const c = canvasFor(`ps-${++n}`);
    const gl = c.getContext('webgl2', { antialias: false, ...attributes });
    const b = source ? await source() : null;
    const keep = {};
    leave(gl, keep);
    const result = { name, pageError: gl.getError() };
    if (keep.unsupported) {
      results.push({ ...result, skipped: 'this browser does not have the extension', pass: true });
      release(gl);
      continue;
    }
    const r = await settle(createRenderer(c, { ...opts, antialias: Boolean(attributes.antialias) }));
    if (!r.ok) {
      results.push({ ...result, created: r.message });
      release(gl);
      continue;
    }
    try {
      const { pixel, ok } = draw[kind](r.r, gl, b);
      r.r.frame();
      result.pixel = pixel;
      result.drawn = ok(pixel);
    } catch (e) {
      result.threw = String(e?.message ?? e).slice(0, 200);
    }
    result.glError = gl.getError();
    r.r.dispose();
    if (keep.before) result.before = keep.before;
    if (keep.timer !== undefined) result.timer = keep.timer;
    if (keep.check) result.kept = keep.check();
    result.pass = result.pageError === 0 && result.drawn === true && result.glError === 0 && result.kept !== false && (keep.before ? keep.before.active === true : true);
    results.push(result);
    release(gl);
  }
  log(
    'PS a page-made context that suits, left with state of the page\'s (each of the ninth critic\'s N8 cases and every other state the renderer resets): web3d draws, uploads and reads back as on a new context, with no WebGL error, and the page\'s own objects are kept',
    results.length === cases.length && results.every((x) => x.pass),
    results,
  );
}

/**
 * LR: the order of listeners on the canvas (the ninth critic's N1). A page's own restored listener,
 * added before web3d's, disposes the renderer live at the loss: its onContextRestored still comes,
 * once. The same in the page's lost listener; and a dispose after web3d handled the restore adds no
 * second call.
 */
async function listenerOrder() {
  const opts = { width: 16, height: 16, antialias: false };
  const d = {};
  const lossAndRestore = async (c, lc) => {
    const lost = eventOf(c, 'webglcontextlost');
    lc.loseContext();
    await lost;
    const restored = eventOf(c, 'webglcontextrestored');
    lc.restoreContext();
    return Promise.race([restored, timeout(3000)]);
  };
  for (const order of ['restored', 'lost']) {
    const c = canvasFor(`lr-${order}`);
    const calls = { lost: 0, restored: 0 };
    let r = null;
    c.addEventListener(`webglcontext${order}`, () => {
      d[`${order}LostAtDispose`] = r.gl.isContextLost();
      r.dispose();
    }, { once: true });
    r = await createRenderer(c, { ...opts, onContextLost: () => calls.lost++, onContextRestored: () => calls.restored++ });
    d[`${order}Event`] = await lossAndRestore(c, r.gl.getExtension('WEBGL_lose_context'));
    await timeout(20);
    d[order] = { ...calls };
    const next = await settle(createRenderer(c, opts));
    d[`${order}Next`] = next.ok ? greenOn(next.r) : next.message;
    if (next.ok) {
      next.r.dispose();
      release(next.r.gl);
    }
  }
  // Disposed later, after web3d's restored listener ran: the hook already came; no second call.
  {
    const c = canvasFor('lr-late');
    const calls = { restored: 0 };
    const r = await createRenderer(c, { ...opts, onContextRestored: () => calls.restored++ });
    d.lateEvent = await lossAndRestore(c, r.gl.getExtension('WEBGL_lose_context'));
    await timeout(20);
    const before = calls.restored;
    r.dispose();
    await timeout(20);
    d.late = { before, after: calls.restored, frame: attempt(() => r.frame()).message };
    release(r.gl);
  }
  log(
    "LR a page's own restored listener that runs before web3d's disposes the live renderer: onContextRestored comes once (N1); disposed in the page's lost listener instead: once, and no onContextLost; disposed after the restore was handled: no second call",
    d.restoredEvent === 'webglcontextrestored' && d.restoredLostAtDispose === false && d.restored.lost === 1 && d.restored.restored === 1 && isGreen(d.restoredNext) &&
      d.lostEvent === 'webglcontextrestored' && d.lostLostAtDispose === true && d.lost.lost === 0 && d.lost.restored === 1 && isGreen(d.lostNext) &&
      d.lateEvent === 'webglcontextrestored' && d.late.before === 1 && d.late.after === 1,
    d,
  );
}

/**
 * RF: removed and navigated frames (the ninth critic's N10 and N10e). A same-origin frame's copy of
 * web3d takes a context from a canvas of this document. With this window's copy loaded, as here, the
 * listeners are this copy's, and removing the frame changes nothing (a). Without one (simulated by
 * taking the installer out of the shared state, as on a page whose top window has no copy): the
 * frame's copy makes them; a create here after the frame is removed puts them back (b); a create
 * here while it is still there moves them (c); a navigated frame counts as gone (d). The documented
 * limit: a loss after the frame is removed and before any create is not cancelled, and the next
 * create says so (e). A frame's renderer left live when the frame goes still holds the canvas until a
 * window that holds it disposes it (f). After each reinstall the canvas holds one listener of each
 * kind.
 */
async function removedFrames() {
  const st = globalThis[Symbol.for('loadngo-web3d')];
  const opts = { width: 16, height: 16, antialias: false };
  const d = {};
  const frame = async () => {
    const iframe = document.createElement('iframe');
    iframe.src = './probe-frame.html';
    document.body.append(iframe);
    await new Promise((res) => iframe.addEventListener('load', res, { once: true }));
    for (let i = 0; i < 50 && !iframe.contentWindow.__frame; i++) await timeout(20);
    return iframe;
  };
  const listeners = async (c) => (typeof window.__listeners === 'function' ? window.__listeners(c.id) : 'no driver');
  const one = (l) => l?.webglcontextlost === 1 && l?.webglcontextrestored === 1;
  /** A loss and restore with a renderer of this window's: whether the loss was cancelled, and its hooks. */
  const recovery = async (c) => {
    const calls = { lost: 0, restored: 0 };
    const r = await createRenderer(c, { ...opts, onContextLost: () => calls.lost++, onContextRestored: () => calls.restored++ });
    const out = { drawn: greenOn(r), listeners: await listeners(c) };
    const lc = r.gl.getExtension('WEBGL_lose_context');
    let prevented = null;
    c.addEventListener('webglcontextlost', (e) => (prevented = e.defaultPrevented), { once: true }); // after web3d's
    const lost = eventOf(c, 'webglcontextlost');
    lc.loseContext();
    await lost;
    out.prevented = prevented;
    r.dispose();
    const restored = eventOf(c, 'webglcontextrestored');
    lc.restoreContext();
    out.restored = await Promise.race([restored, timeout(2000)]);
    await timeout(20);
    out.calls = calls;
    const next = await settle(createRenderer(c, opts));
    out.next = next.ok ? greenOn(next.r) : next.message;
    if (next.ok) next.r.dispose();
    release(r.gl);
    return out;
  };
  const recovered = (x) => isGreen(x.drawn) && one(x.listeners) && x.prevented === true && x.restored === 'webglcontextrestored' && x.calls.lost === 1 && x.calls.restored === 1 && isGreen(x.next);
  // a) This window's copy is loaded: the frame's renderer is left live when the frame is removed.
  {
    const c = canvasFor('rf-a');
    const iframe = await frame();
    const theirs = await iframe.contentWindow.__frame.createRenderer(c, opts);
    d.aTheirs = greenOn(theirs);
    iframe.remove();
    await timeout(50);
    const lc = c.getContext('webgl2').getExtension('WEBGL_lose_context');
    let prevented = null;
    c.addEventListener('webglcontextlost', (e) => (prevented = e.defaultPrevented), { once: true });
    const lost = eventOf(c, 'webglcontextlost');
    lc.loseContext();
    await lost;
    d.aPrevented = prevented;
    const restored = eventOf(c, 'webglcontextrestored');
    lc.restoreContext();
    d.aRestored = await Promise.race([restored, timeout(2000)]);
    d.a = await recovery(c); // the frame's renderer is lost, so the canvas is free
  }
  // f) The frame's renderer is left live when the frame goes: it still holds the canvas (any window
  // that holds it can still use it), so a create here is refused until this window disposes it.
  {
    const c = canvasFor('rf-f');
    const iframe = await frame();
    const theirs = await iframe.contentWindow.__frame.createRenderer(c, opts);
    iframe.remove();
    await timeout(50);
    d.fRefused = (await settle(createRenderer(c, opts))).message;
    d.fDispose = attempt(() => theirs.dispose());
    d.f = await recovery(c);
  }
  const installer = st.install;
  delete st.install; // as on a page whose top window has no copy of web3d
  try {
    // b) The frame's copy made the listeners and the frame is removed; a create here puts them back.
    {
      const c = canvasFor('rf-b');
      const iframe = await frame();
      const theirs = await iframe.contentWindow.__frame.createRenderer(c, opts);
      d.bTheirs = greenOn(theirs);
      theirs.dispose();
      iframe.remove();
      await timeout(50);
      d.b = await recovery(c);
    }
    // c) A create here while the frame is still there moves the listeners; then the frame goes.
    {
      const c = canvasFor('rf-c');
      const iframe = await frame();
      const theirs = await iframe.contentWindow.__frame.createRenderer(c, opts);
      theirs.dispose();
      const calls = { lost: 0, restored: 0 };
      const ours = await createRenderer(c, { ...opts, onContextLost: () => calls.lost++, onContextRestored: () => calls.restored++ });
      iframe.remove();
      await timeout(50);
      const lc = ours.gl.getExtension('WEBGL_lose_context');
      let prevented = null;
      c.addEventListener('webglcontextlost', (e) => (prevented = e.defaultPrevented), { once: true });
      const lost = eventOf(c, 'webglcontextlost');
      lc.loseContext();
      await lost;
      ours.dispose();
      const restored = eventOf(c, 'webglcontextrestored');
      lc.restoreContext();
      d.c = { prevented, restored: await Promise.race([restored, timeout(2000)]) };
      await timeout(20);
      d.c.calls = calls;
      d.c.listeners = await listeners(c);
      release(ours.gl);
    }
    // d) The frame navigates away instead of going: its listeners stop just the same.
    {
      const c = canvasFor('rf-d');
      const iframe = await frame();
      const theirs = await iframe.contentWindow.__frame.createRenderer(c, opts);
      theirs.dispose();
      const navigated = new Promise((res) => iframe.addEventListener('load', res, { once: true }));
      iframe.src = 'about:blank';
      await navigated;
      await timeout(50);
      d.d = await recovery(c);
      iframe.remove();
    }
    // e) The documented limit: the frame is removed and the loss comes before any create here.
    {
      const c = canvasFor('rf-e');
      const iframe = await frame();
      const theirs = await iframe.contentWindow.__frame.createRenderer(c, opts);
      theirs.dispose();
      iframe.remove();
      await timeout(50);
      let prevented = null;
      c.addEventListener('webglcontextlost', (e) => (prevented = e.defaultPrevented), { once: true });
      const lost = eventOf(c, 'webglcontextlost');
      c.getContext('webgl2').getExtension('WEBGL_lose_context').loseContext();
      await lost;
      let calls = 0;
      const after = await settle(createRenderer(c, { ...opts, onContextRestored: () => calls++ }));
      // No restoreContext: Chrome refuses it for a loss nobody cancelled.
      d.e = { prevented, create: after.ok ? 'created' : after.message, listeners: await listeners(c), calls };
    }
  } finally {
    st.install = installer;
  }
  log(
    "RF a frame's copy of web3d took a context from this document's canvas, then the frame went: with this window's copy loaded the loss is cancelled and recovered (a); without it, a create here puts the listeners back (b), one made while the frame is there moves them (c), and a navigated frame counts as gone (d), each cancelling the next loss and calling its hooks once, with one listener of each kind; a loss before any such create is not cancelled, and the create says why (e); a frame's renderer left live holds the canvas until this window disposes it (f)",
    isGreen(d.aTheirs) && d.aPrevented === true && d.aRestored === 'webglcontextrestored' && recovered(d.a) &&
      isGreen(d.bTheirs) && recovered(d.b) &&
      d.c.prevented === true && d.c.restored === 'webglcontextrestored' && d.c.calls.lost === 1 && d.c.calls.restored === 1 && one(d.c.listeners) &&
      recovered(d.d) &&
      d.e.prevented === false && /after the window whose copy of web3d watched this canvas went away/.test(d.e.create) && one(d.e.listeners) && d.e.calls === 0 &&
      /already has a live renderer/.test(d.fRefused) && !d.fDispose.threw && recovered(d.f),
    d,
  );
}

/**
 * NC: canvases with and without a context as texture sources (the ninth critic's N11), as
 * documented: one with no context is uploaded and has a '2d' context for good afterwards; one with a
 * context, 2d or WebGL, is unchanged; a 'bitmaprenderer' one is refused and stays one.
 */
async function contextlessCanvases() {
  const host = canvasFor('nc-host');
  const r = await createRenderer(host, { width: 16, height: 16, antialias: false });
  const d = {};
  const el = document.createElement('canvas');
  el.width = 4;
  el.height = 4;
  d.el = attempt(() => r.createTexture(el)).threw ? 'refused' : 'uploaded';
  d.elAfter = {
    webgl2: el.getContext('webgl2'),
    bitmaprenderer: el.getContext('bitmaprenderer'),
    transfer: attempt(() => el.transferControlToOffscreen()).threw,
    attributes: el.getContext('2d', { alpha: false, willReadFrequently: true })?.getContextAttributes(),
  };
  d.elAfter.webgl2 = d.elAfter.webgl2 === null ? null : 'a context';
  d.elAfter.bitmaprenderer = d.elAfter.bitmaprenderer === null ? null : 'a context';
  d.elRenderer = (await settle(createRenderer(el, { width: 4, height: 4, antialias: false }))).message ?? 'created';
  const off = new OffscreenCanvas(4, 4);
  d.off = attempt(() => r.createTexture(off)).threw ? 'refused' : 'uploaded';
  d.offWebgl2After = off.getContext('webgl2') === null ? null : 'a context';
  // A 2d canvas the page drew on, with its own settings: unchanged, and its pixels are the texture's.
  const two = document.createElement('canvas');
  two.width = 2;
  two.height = 2;
  const g = two.getContext('2d', { alpha: false, willReadFrequently: true });
  g.fillStyle = '#00ff00';
  g.fillRect(0, 0, 2, 2);
  const t = attempt(() => r.createTexture(two, { mipmaps: false }));
  d.two = { texture: t.threw ? t.message : 'uploaded', same: two.getContext('2d') === g, attributes: g.getContextAttributes() };
  // A WebGL 2 canvas: unchanged.
  const glc = document.createElement('canvas');
  const g2 = glc.getContext('webgl2');
  d.webgl = { texture: attempt(() => r.createTexture(glc)).threw ? 'refused' : 'uploaded', same: glc.getContext('webgl2') === g2, no2d: glc.getContext('2d') === null };
  release(g2);
  // A bitmaprenderer canvas: refused, and still one.
  const bmp = document.createElement('canvas');
  bmp.width = 4;
  bmp.height = 4;
  const bctx = bmp.getContext('bitmaprenderer');
  bctx.transferFromImageBitmap(await bitmap4(0, 255, 0));
  const refused = attempt(() => r.createTexture(bmp));
  d.bitmaprenderer = { refused: refused.threw && /bitmaprenderer/.test(refused.message), still: bmp.getContext('bitmaprenderer') === bctx };
  if (!t.threw) {
    flat(r);
    r.createObject(r.createMesh(quad(-1, -1, 1, 1, -5)), r.createMaterial({ texture: t.value }));
    r.frame();
    d.two.pixel = reader(r)(0, 0);
  }
  d.glError = r.gl.getError();
  r.dispose();
  release(r.gl);
  log(
    "NC as documented: a canvas with no context uploads and then has a '2d' context for good (no webgl2 or bitmaprenderer context, no transfer, later 2d options ignored), an OffscreenCanvas too; a canvas with its own 2d or WebGL context is unchanged and uploads its pixels; a bitmaprenderer canvas is refused and stays one",
    d.el === 'uploaded' && d.elAfter.webgl2 === null && d.elAfter.bitmaprenderer === null && d.elAfter.transfer && d.elAfter.attributes?.alpha === true &&
      d.elAfter.attributes?.willReadFrequently === false && /another kind of context/.test(d.elRenderer) && d.off === 'uploaded' && d.offWebgl2After === null &&
      d.two.texture === 'uploaded' && d.two.same && d.two.attributes.alpha === false && d.two.attributes.willReadFrequently === true && isGreen(d.two.pixel) &&
      d.webgl.texture === 'uploaded' && d.webgl.same && d.webgl.no2d && d.bitmaprenderer.refused && d.bitmaprenderer.still && d.glError === 0,
    d,
  );
}

/**
 * MS: what two refusals say (the ninth critic's notes N21 and N7b'). A detached OffscreenCanvas is
 * named as detached, a placeholder canvas as transferred. A context lost before web3d first used its
 * canvas: the create says the restore comes if the lost event is cancelled, which web3d does if the
 * event is still to come; lost in the same task as the create, it is, and the hook comes once; lost
 * with its event dispatched and not cancelled, no hook comes.
 */
async function refusalMessages() {
  const opts = { width: 16, height: 16, antialias: false };
  const d = {};
  const r = await createRenderer(canvasFor('ms-host'), opts);
  const placeholder = document.createElement('canvas');
  placeholder.transferControlToOffscreen();
  d.placeholder = attempt(() => r.createTexture(placeholder)).message;
  const sent = new OffscreenCanvas(4, 4);
  structuredClone(sent, { transfer: [sent] });
  d.detached = attempt(() => r.createTexture(sent)).message;
  r.dispose();
  release(r.gl);
  {
    const c = canvasFor('ms-pending');
    const gl = c.getContext('webgl2', { antialias: false });
    const lc = gl.getExtension('WEBGL_lose_context');
    let calls = 0;
    lc.loseContext(); // its event is dispatched in a later task
    d.pending = (await settle(createRenderer(c, { ...opts, onContextRestored: () => calls++ }))).message;
    await timeout(30);
    const restored = eventOf(c, 'webglcontextrestored');
    lc.restoreContext();
    d.pendingRestored = await Promise.race([restored, timeout(2000)]);
    await timeout(20);
    d.pendingCalls = calls;
    release(gl);
  }
  {
    const c = canvasFor('ms-dispatched');
    const gl = c.getContext('webgl2', { antialias: false });
    const lost = eventOf(c, 'webglcontextlost');
    gl.getExtension('WEBGL_lose_context').loseContext();
    await lost;
    let calls = 0;
    d.dispatched = (await settle(createRenderer(c, { ...opts, onContextRestored: () => calls++ }))).message;
    await timeout(50);
    d.dispatchedCalls = calls; // no restoreContext: Chrome refuses it for a loss nobody cancelled
  }
  const before = (m) => /lost before web3d first used this canvas/.test(m ?? '') && /which web3d now does if that event is still to come/.test(m) && !/only if the page cancelled/.test(m);
  log(
    "MS a detached OffscreenCanvas is refused as detached, a placeholder canvas as transferred (N21); a loss before web3d's first use: the message says web3d cancels a lost event still to come, as it does when the loss is in the create's own task (the hook comes once), and none comes for one already dispatched (N7b')",
    /control was transferred to an OffscreenCanvas/.test(d.placeholder) && /detached/.test(d.detached) && !/transferred to an OffscreenCanvas/.test(d.detached) &&
      before(d.pending) && d.pendingRestored === 'webglcontextrestored' && d.pendingCalls === 1 && before(d.dispatched) && d.dispatchedCalls === 0,
    d,
  );
}

async function main() {
  const r = await createRenderer(document.getElementById('main'), { width: S, height: S, antialias: false });
  out.info = r.info;
  await rendering(r);
  r.dispose();
  await disposal();
  await sharing();
  await contextLoss();
  await typedArrays();
  await kinds();
  await compileCache();
  await failedCreate();
  await reentrancy();
  await serials();
  await arrays();
  await textureSize();
  await foreignContext();
  await lossDefault();
  await lossDuringCreate();
  await contextAndSources();
  await realms();
  await lossAsDocumented();
  await uploadsAndBytes();
  await lossController();
  await largestSerial();
  // Each of these logs its own failure if it throws, so the rest still run.
  for (const [id, probe] of [
    ['PS', pageState],
    ['LR', listenerOrder],
    ['RF', removedFrames],
    ['NC', contextlessCanvases],
    ['MS', refusalMessages],
  ]) {
    try {
      await probe();
    } catch (e) {
      log(`${id} ran to its end`, false, String(e?.stack ?? e));
    }
  }
  out.passed = out.tests.filter((t) => t.pass).length;
  out.failed = out.tests.filter((t) => !t.pass).map((t) => t.name);
}

window.__probe.done = main().then(
  () => out,
  (e) => {
    out.error = String(e?.stack ?? e);
    out.failed = [...(out.failed ?? []), `error: ${out.error}`];
    return out;
  },
);
