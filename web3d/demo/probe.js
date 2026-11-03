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
  log('B texture upright: top-left red, top-right green, bottom-left blue', close(tl, [255, lo, lo], 3) && close(tr, [lo, 255, lo], 3) && close(bl, [lo, lo, 255], 3), { tl, tr, bl, lo });
  log('B the sRGB texture is decoded (128 is linear 0.216)', close(br, [mid, mid, mid], 3), { got: br, expected: mid });

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
    log(`P a ${n.toLocaleString('en')}-vertex mesh draws its triangle on vertex ${(n - 1).toLocaleString('en')}`, px[0] > 200, { pixel: px });
  }

  // R: lighting against three.js r180 (tools/three-reference.txt, computed from three's chunks).
  // The cases that face the camera: normal and view +Z at the centre pixel.
  {
    const text = await (await fetch('../tools/three-reference.txt')).text();
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
      log(`R lighting matches three.js r180: ${name}`, close(got, expected, 2), { got, expected, threeRadiance: v.slice(23) });
    }
    if (cases.length < 4) log('R lighting reference cases found', false, { cases: cases.length });
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
  const check = (name, fn, want, pattern) => {
    const x = attempt(fn);
    const ok = want === 'throw' ? x.threw && (!pattern || pattern.test(x.message)) : !x.threw;
    log(`I ${name}`, ok, x);
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
  // H: forged handles. The wasm boundary converts numbers with ToInt32, so the wrapper must refuse
  // a handle plus 2^32, a fraction or a string before they could alias a live object (the third
  // critic's probe H).
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
    log('H forged handles (plus 2^32, fractional, a string, out of range) are refused, and the object stays drawn', refused && others.every((t) => t.threw) && still[0] > 200, { tries, others, pixel: still });
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
  log('K loss: frame() returns false and every create throws "context is lost", a new renderer too', k.lostEvent === 'lost' && k.lostFlag && k.frame.value === false && createsRefused && !k.setTransformWhileLost.threw && /context is lost/.test(k.createRendererWhileLost), k);
  log('K restore: the old renderer stays finished; a new one draws', k.restoredEvent === 'restored' && k.oldStillLost && k.pixelBefore[1] > 200 && k.pixelAfter[1] > 200, k);
  log('K disposing the old renderer sends nothing into the restored context (getError stays 0)', k.errorBeforeOldDispose === 0 && k.errorAfterOldDispose === 0 && k.errorAfterFrame === 0, k);
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
    d.swapped.threw && /is a material handle, not a mesh handle/.test(d.swapped.message) &&
      d.destroyMaterialWithMesh.threw && /is a mesh handle, not a material handle/.test(d.destroyMaterialWithMesh.message) &&
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
  // Forty more failures, then forty creates: no salt or canvas is left held.
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
    'Y a failed create (before or after the context exists) leaves no listener, salt or claim behind; a canvas refuses other context settings',
    /whole number/.test(d.early) && /not loadngo-web3d/.test(d.late) && created === 40 &&
      /keeps its first context's settings/.test(d.mismatch) && d.goodAttributes.antialias === false && d.goodAttributes.samples === 0 &&
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
  boxed[13] = { valueOf: () => (r.frame(), 0) };
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
    'U a getter, valueOf or descriptor that calls the renderer is refused; numbers are never converted; the renderer stays live and drawn',
    d.valueOfCalls === 0 && d.keyLight.threw && /must be a number/.test(d.keyLight.message) && d.clearColor.threw &&
      reentrant(d.getterDisposes) && reentrant(d.valueOfFrames) && reentrant(d.descriptorGetter) && reentrant(d.materialGetter) &&
      d.stillLive.value === true && d.pixel[1] > 200 && d.glError === 0,
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
