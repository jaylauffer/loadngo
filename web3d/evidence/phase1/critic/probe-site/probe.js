// Critic's probe for loadngo-web3d phase 1: pixel-exact rendering checks and input refusals.
//
// Adapted by the builder after the phase-1 fixes, to run as a regression check: lights now take
// three.js r180 units (diffuse is albedo / pi times irradiance), so every renderer here goes through
// threeUnits(), which scales intensities by pi to keep this probe's original expectations; and the
// defects it recorded as "ACCEPTED silently" now expect a refusal (Uint8 weights are dequantised
// and accepted).
import { createRenderer as create } from '/web3d/pkg/web3d.js';

const threeUnits = (r) => {
  const key = r.setKeyLight.bind(r);
  const ambient = r.setAmbient.bind(r);
  r.setKeyLight = (x, y, z, R, G, B, i = 1) => key(x, y, z, R, G, B, i * Math.PI);
  r.setAmbient = (sr, sg, sb, gr, gg, gb, i = 1) => ambient(sr, sg, sb, gr, gg, gb, i * Math.PI);
  return r;
};
const createRenderer = async (...args) => threeUnits(await create(...args));

const out = { tests: [], notes: [] };
window.__probe = { done: null, out };
const log = (name, pass, detail) => out.tests.push({ name, pass, detail });

const N = 0.1, F = 10;
const ORTHO = new Float32Array([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, -2 / (F - N), 0, 0, 0, -(F + N) / (F - N), 1]);
const IDENT = new Float32Array([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1]);
const S = 256;

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

function srgb(c) {
  c = Math.min(1, Math.max(0, c));
  return c <= 0.0031308 ? c * 12.92 : 1.055 * Math.pow(c, 1 / 2.4) - 0.055;
}
function lin(s8) {
  const c = s8 / 255;
  return c <= 0.04045 ? c / 12.92 : Math.pow((c + 0.055) / 1.055, 2.4);
}
const close = (a, b, tol = 3) => a.every((v, i) => Math.abs(v - b[i]) <= tol);

async function bitmap(w, h, rgba) {
  return createImageBitmap(new ImageData(new Uint8ClampedArray(rgba), w, h), { premultiplyAlpha: 'none', colorSpaceConversion: 'none' });
}

function expectThrow(fn) {
  try { const v = fn(); return { threw: false, value: v }; } catch (e) { return { threw: true, message: String(e.message ?? e) }; }
}

async function main() {
  const canvas = document.getElementById('c');
  const r = await createRenderer(canvas, { width: S, height: S, antialias: false });
  const gl = r.gl;
  out.info = r.info;
  const px = new Uint8Array(4);
  const read = (nx, ny) => { // NDC point -> pixel (read in the same task as frame())
    const x = Math.floor((nx + 1) / 2 * S), y = Math.floor((ny + 1) / 2 * S);
    gl.readPixels(x, y, 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, px);
    return [px[0], px[1], px[2]];
  };
  const stats = new Float64Array(8);
  r.setCamera(IDENT, ORTHO);
  r.setClearColor(0, 0, 0);
  const all = [];
  const show = (...objs) => { for (const o of all) r.setVisible(o, objs.includes(o)); };
  const obj = (mesh, mat, m = IDENT) => { const o = r.createObject(mesh, mat, m); all.push(o); return o; };

  // A: sRGB output and the flat-ambient lighting maths. key 0, sky = ground = s: colour = s * (base + 0.04).
  r.setKeyLight(0, 0, 1, 0, 0, 0, 0);
  r.setAmbient(0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 1);
  const full = r.createMesh(quad(-1, -1, 1, 1, -5));
  const white = r.createMaterial({ baseColor: [1, 1, 1, 1] });
  const oA = obj(full, white);
  show(oA); r.frame();
  const a = read(0, 0), ea = Math.round(srgb(0.5 * 1.04) * 255);
  log('A sRGB-encoded output (white, ambient 0.5)', close(a, [ea, ea, ea], 2), { got: a, expected: ea, ifLinearOutput: Math.round(0.52 * 255) });
  // Clear colour is linear too.
  r.setClearColor(0.5, 0.5, 0.5); show(); r.frame();
  const cc = read(0, 0), ec = Math.round(srgb(0.5) * 255);
  log('A2 clear colour linear 0.5 encoded', close(cc, [ec, ec, ec], 1), { got: cc, expected: ec });
  r.setClearColor(0, 0, 0);

  // B: texture orientation (glTF: first image row is v = 0) and sRGB decoding.
  const s = 1 / 1.04;
  r.setAmbient(s, s, s, s, s, s, 1); // colour = (texel + 0.04) / 1.04
  const texB = r.createTexture(await bitmap(2, 2, [255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 128, 128, 128, 255]), { mipmaps: false });
  const matB = r.createMaterial({ texture: texB });
  const oB = obj(full, matB);
  show(oB); r.frame();
  const tl = read(-0.5, 0.5), tr = read(0.5, 0.5), bl = read(-0.5, -0.5), br = read(0.5, -0.5);
  const lo = Math.round(srgb(0.04 / 1.04) * 255);
  const mid = Math.round(srgb((lin(128) + 0.04) / 1.04) * 255);
  log('B texture upright: top-left red, top-right green, bottom-left blue', close(tl, [255, lo, lo]) && close(tr, [lo, 255, lo]) && close(bl, [lo, lo, 255]), { tl, tr, bl, lo });
  log('B sRGB texture decoded (128 -> linear 0.216)', close(br, [mid, mid, mid], 3), { got: br, expected: mid, ifNotDecoded: Math.round(srgb((128 / 255 + 0.04) / 1.04) * 255) });

  // C: alpha mask over a red quad behind; left half alpha 0 is discarded.
  r.setAmbient(1, 1, 1, 1, 1, 1, 1);
  const red = r.createMaterial({ baseColor: [1, 0, 0, 1] });
  const behind = r.createMesh(quad(-1, -1, 1, 1, -8));
  const texC = r.createTexture(await bitmap(2, 1, [0, 255, 0, 0, 0, 255, 0, 255]), { mipmaps: false, wrapS: 'clamp', wrapT: 'clamp' });
  const matC = r.createMaterial({ texture: texC, alphaMode: 'mask', alphaCutoff: 0.5 });
  const oCb = obj(behind, red), oC = obj(full, matC);
  show(oCb, oC); r.frame();
  const cl = read(-0.75, 0), cr = read(0.75, 0);
  log('C alpha mask: left discarded (red behind), right drawn (green)', cl[0] > 200 && cl[1] < 60 && cr[1] > 200 && cr[0] < 60, { left: cl, right: cr });

  // D: blending, created before the opaque quad behind it; must still draw after it.
  const blue = r.createMaterial({ baseColor: [0, 0, 1, 0.5], alphaMode: 'blend' });
  const front = r.createMesh(quad(-1, -1, 1, 1, -2));
  const oD = obj(front, blue), oDb = obj(behind, white);
  show(oD, oDb); r.frame();
  const d = read(0, 0);
  // Fixed-function blend of encoded values: 0.5 * encode(src) + 0.5 * encode(white*(1+0.04)).
  const src = [srgb(0.04), srgb(0.04), srgb(1.04)].map((v) => v * 255);
  const dst = 255;
  const ed = src.map((v) => Math.round(0.5 * v + 0.5 * dst));
  log('D blend over opaque (blended drawn last, blended in encoded space)', close(d, ed, 3), { got: d, expected: ed });

  // E: depth test independent of creation order: a near green quad created before a far red one.
  const green = r.createMaterial({ baseColor: [0, 1, 0, 1] });
  const near = r.createMesh(quad(-0.5, -0.5, 0.5, 0.5, -3));
  const oEn = obj(near, green), oEf = obj(full, red);
  show(oEn, oEf); r.frame();
  const e = read(0, 0), e2 = read(0.9, 0.9);
  log('E depth: near quad wins, far visible around it', e[1] > 200 && e[0] < 60 && e2[0] > 200, { centre: e, corner: e2 });

  // F: back-face culling; a double-sided material turns its normal round.
  r.setAmbient(0, 0, 0, 0, 0, 0, 0); r.setKeyLight(0, 0, 1, 1, 1, 1, 1);
  const away = r.createMesh(quad(-1, -1, 1, 1, -5, true));
  const oF = obj(away, white);
  show(oF); r.frame();
  const f1 = read(0, 0);
  const ds = r.createMaterial({ baseColor: [1, 1, 1, 1], doubleSided: true });
  const oF2 = obj(away, ds);
  show(oF2); r.frame();
  const f2 = read(0, 0);
  log('F single-sided back face culled; double-sided drawn and lit from the front', f1[0] < 10 && f2[0] > 200, { culled: f1, doubleSided: f2 });

  // G: a mirroring transform keeps a single-sided quad visible (front face flips) and lit.
  const oG = obj(full, white, scale3(-1, 1, 1));
  show(oG); r.frame();
  const g = read(0, 0);
  log('G mirrored single-sided quad still drawn and lit', g[0] > 200, { got: g });

  // H: frustum culling.
  const oH = obj(full, white, translate(5, 0, 0));
  show(oH); r.frame(); r.readStats(stats);
  log('H object outside the view is culled', stats[3] === 0 && stats[5] === 1, { draws: stats[3], culled: stats[5] });

  // N: u32 indices (65,537 vertices), a triangle using the last vertex.
  {
    const n = 65537;
    const pos = new Float32Array(n * 3).fill(0);
    const nor = new Float32Array(n * 3); for (let i = 0; i < n; i++) nor[i * 3 + 2] = 1;
    pos.set([-1, -1, -5, 1, -1, -5], 0);
    pos.set([0, 1, -5], (n - 1) * 3);
    const big = r.createMesh({ positions: pos, normals: nor, indices: new Uint32Array([0, 1, n - 1]) });
    const info = big;
    const oN = obj(big, white);
    show(oN); r.frame();
    const nn = read(0, 0);
    log('N 65,537-vertex mesh with u32 indices draws', nn[0] > 200, { got: nn });
  }

  // L: memory growth while views are cached (createMesh of 1.5M vertices), then per-frame calls still work.
  {
    const before = r._x.memory.buffer.byteLength;
    const n = 1_500_000;
    const pos = new Float32Array(n * 3);
    pos.set([-1, -1, -5, 1, -1, -5, 0, 1, -5], 0);
    const nor = new Float32Array(n * 3); for (let i = 0; i < n; i++) nor[i * 3 + 2] = 1;
    const big = r.createMesh({ positions: pos, normals: nor, indices: new Uint32Array([0, 1, 2]) });
    const after = r._x.memory.buffer.byteLength;
    const oL = obj(big, white, IDENT);
    r.setTransform(oL, translate(0, 0, 0));
    r.setCamera(IDENT, ORTHO);
    show(oL); r.frame(); r.readStats(stats);
    const l = read(0, -0.2);
    log('L after memory growth: setTransform, setCamera, frame and readStats still work', after > before && l[0] > 200 && stats[3] === 1, { memoryBefore: before, memoryAfter: after, pixel: l, draws: stats[3] });
  }

  // M: GPU timer.
  for (let i = 0; i < 10; i++) { r.frame(); await new Promise((res) => requestAnimationFrame(res)); }
  log('M GPU timer reading present', r.gpuMs !== null && r.info.timerQuery, { gpuMs: r.gpuMs });

  // I: refusals.
  const t = (name, fn, want) => { const x = expectThrow(fn); out.notes.push({ name, ...x }); log(`I ${name}`, want === 'throw' ? x.threw : !x.threw, x); };
  t('index past the end is refused', () => r.createMesh({ positions: new Float32Array(9), indices: new Uint32Array([0, 1, 3]) }), 'throw');
  t('positions not a multiple of 3 are refused', () => r.createMesh({ positions: new Float32Array(8), indices: new Uint32Array([0, 1, 2]) }), 'throw');
  t('texture over MAX_TEXTURE_SIZE is refused', () => r.createTexture(new OffscreenCanvas(r.info.maxTextureSize + 1, 1)), 'throw');
  t('destroying a texture in use is refused', () => r.destroyTexture(texB), 'throw');
  t('destroying a mesh in use is refused', () => r.destroyMesh(full), 'throw');
  t('unknown alpha mode is refused', () => r.createMaterial({ alphaMode: 'add' }), 'throw');
  t('non-finite transform is refused', () => r.setTransform(oA, new Float32Array(16).fill(NaN)), 'throw');
  const gone = r.createMesh(quad(-1, -1, 1, 1, -5));
  r.destroyMesh(gone);
  t('stale mesh handle is refused', () => r.createObject(gone, white), 'throw');
  t('NaN position refused', () => r.createMesh({ positions: new Float32Array([0, 0, 0, NaN, 0, 0, 0, 1, 0]), indices: new Uint32Array([0, 1, 2]) }), 'throw');
  // Inputs the phase 2 handover could pass by mistake: quantised arrays, normalised weights.
  t('Int16 (quantised) positions are refused', () => r.createMesh({ positions: new Int16Array([0, 0, 0, 32767, 0, 0, 0, 32767, 0]), indices: new Uint16Array([0, 1, 2]) }), 'throw');
  t('Uint8 normalised weights (255 = 1.0) are dequantised and accepted', () => r.createMesh({ positions: new Float32Array([0, 0, 0, 1, 0, 0, 0, 1, 0]), indices: new Uint16Array([0, 1, 2]), joints: new Uint8Array(12), weights: new Uint8Array([255, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0]) }), 'accept');
  t('weights summing to 7 per vertex are refused', () => r.createMesh({ positions: new Float32Array([0, 0, 0, 1, 0, 0, 0, 1, 0]), indices: new Uint16Array([0, 1, 2]), joints: new Uint16Array(12), weights: new Float32Array(12).fill(1.75) }), 'throw');
  t('joint index 65535 with no skeleton is accepted until phase 3 has skeletons', () => r.createMesh({ positions: new Float32Array([0, 0, 0, 1, 0, 0, 0, 1, 0]), indices: new Uint16Array([0, 1, 2]), joints: new Uint16Array(12).fill(65535), weights: new Float32Array(12).fill(0.25) }), 'accept');
  t('morph deltas of the wrong length are refused', () => r.createMesh({ positions: new Float32Array(9), indices: new Uint16Array([0, 1, 2]), morphTargets: [new Float32Array(6)] }), 'throw');

  out.final = { glError: gl.getError() };

  // J: use after dispose, on a second canvas.
  {
    const c2 = document.getElementById('c2');
    const r2 = await createRenderer(c2, { width: 64, height: 64, antialias: false });
    const m2 = r2.createMesh(quad(-1, -1, 1, 1, -5));
    r2.dispose();
    const after = {};
    after.frame = expectThrow(() => r2.frame());
    after.createMaterial = expectThrow(() => r2.createMaterial({}));
    after.createMesh = expectThrow(() => r2.createMesh(quad(-1, -1, 1, 1, -5)));
    after.setTransform = expectThrow(() => r2.setTransform(1, IDENT));
    after.destroyMesh = expectThrow(() => r2.destroyMesh(m2));
    after.readStats = expectThrow(() => Array.from(r2.readStats(new Float64Array(8))));
    after.mem0 = Array.from(new Uint8Array(r2._x.memory.buffer, 0, 64)).some((b) => b !== 0);
    out.useAfterDispose = after;
    const clean = ['frame', 'createMaterial', 'createMesh', 'setTransform', 'destroyMesh'].every((k) => after[k].threw && /dispos|destroy|not live|releas/i.test(after[k].message));
    log('J calls after dispose are refused with a clear error', clean, after);
  }

  // K: context loss and restore, on a third canvas.
  {
    const c3 = document.getElementById('c3');
    let restored = null, lostCalled = false;
    const restoredP = new Promise((res) => { restored = res; });
    const r3 = await createRenderer(c3, { width: 64, height: 64, antialias: false, onContextLost: () => { lostCalled = true; }, onContextRestored: () => restored() });
    const lc = r3.gl.getExtension('WEBGL_lose_context');
    const m3 = r3.createMesh(quad(-1, -1, 1, 1, -5));
    const mat3 = r3.createMaterial({});
    r3.createObject(m3, mat3);
    lc.loseContext();
    await new Promise((res) => setTimeout(res, 50));
    const k = { lostCalled, lost: r3.lost, frame: r3.frame() };
    k.createMesh = expectThrow(() => r3.createMesh(quad(-1, -1, 1, 1, -5)));
    k.createTexture = expectThrow(() => r3.createTexture(new OffscreenCanvas(4, 4)));
    k.setTransform = expectThrow(() => r3.setTransform(1, IDENT));
    lc.restoreContext();
    await Promise.race([restoredP, new Promise((res) => setTimeout(res, 2000))]);
    const r4 = await createRenderer(c3, { width: 64, height: 64, antialias: false });
    r4.setCamera(IDENT, ORTHO); r4.setKeyLight(0, 0, 1, 0, 0, 0, 0); r4.setAmbient(1, 1, 1, 1, 1, 1, 1);
    r4.createObject(r4.createMesh(quad(-1, -1, 1, 1, -5)), r4.createMaterial({ baseColor: [0, 1, 0, 1] }));
    r4.frame();
    const p = new Uint8Array(4); r4.gl.readPixels(32, 32, 1, 1, r4.gl.RGBA, r4.gl.UNSIGNED_BYTE, p);
    k.newRendererPixel = Array.from(p);
    k.newLostFlag = r4.lost;
    k.errorBeforeOldDispose = r4.gl.getError();
    r3.dispose();
    k.errorAfterOldDispose = r4.gl.getError();
    out.contextLoss = k;
    log('K context loss: frame stops, creates refused; a new renderer on the restored context draws', k.lostCalled && k.lost && k.frame === false && k.createMesh.threw && p[1] > 200, k);
    r4.dispose();
  }
  r.dispose();
}

window.__probe.done = main().then(() => out, (e) => { out.error = String(e?.stack ?? e); return out; });
