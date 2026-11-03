// Critic round 2: independent GPU probes for loadngo-web3d phase 1.
import { createRenderer } from '/web3d/pkg/web3d.js';
import * as THREE from '/three/three.module.js';

const out = { tests: [], notes: {} };
window.__critic = { done: null, out };
const log = (name, pass, detail) => out.tests.push({ name, pass: Boolean(pass), detail });
const PI = Math.PI;
const N = 0.1;
const F = 10;
const ORTHO = new Float32Array([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, -2 / (F - N), 0, 0, 0, -(F + N) / (F - N), 1]);
const IDENT = [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1];
const srgb = (c) => {
  c = Math.min(1, Math.max(0, c));
  return c <= 0.0031308 ? c * 12.92 : 1.055 * c ** (1 / 2.4) - 0.055;
};
const byte = (c) => Math.round(srgb(c) * 255);
const lin = (s8) => {
  const c = s8 / 255;
  return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
};
const centre = (gl) => {
  const px = new Uint8Array(4);
  gl.readPixels(Math.floor(gl.drawingBufferWidth / 2), Math.floor(gl.drawingBufferHeight / 2), 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, px);
  return [px[0], px[1], px[2]];
};
const at = (gl, nx, ny) => {
  const px = new Uint8Array(4);
  gl.readPixels(Math.floor(((nx + 1) / 2) * gl.drawingBufferWidth), Math.floor(((ny + 1) / 2) * gl.drawingBufferHeight), 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, px);
  return [px[0], px[1], px[2]];
};
const attempt = (fn) => {
  try {
    return { threw: false, value: fn() };
  } catch (e) {
    return { threw: true, message: String(e?.message ?? e) };
  }
};
const timeout = (ms) => new Promise((res) => setTimeout(() => res('timeout'), ms));
const quad = (z, n = [0, 0, 1], s = 5) => ({
  positions: new Float32Array([-s, -s, z, s, -s, z, s, s, z, -s, s, z]),
  normals: new Float32Array([...n, ...n, ...n, ...n]),
  indices: new Uint16Array([0, 1, 2, 0, 2, 3]),
});

// 1. WebGL2 primitive restart: a u16 index of 0xFFFF is always a restart index in WebGL 2.
async function restart() {
  const r = await createRenderer(document.getElementById('a'), { width: 64, height: 64, antialias: false });
  r.setCamera(IDENT, ORTHO);
  r.setKeyLight(0, 0, 1, 0, 0, 0, 0);
  r.setAmbient(1, 1, 1, 1, 1, 1, PI);
  r.setClearColor(0, 0, 0);
  const white = r.createMaterial({ baseColor: [1, 1, 1, 1] });
  const objs = [];
  const cases = [];
  for (const [n, special] of [
    [65535, 65534],
    [65536, 65535],
    [65537, 65536],
  ]) {
    const pos = new Float32Array(n * 3);
    for (let i = 0; i < n; i++) pos[i * 3 + 2] = -5;
    pos.set([3, -1, -5], 0);
    pos.set([-1, 3, -5], 3);
    pos.set([-1, -1, -5], special * 3);
    const nor = new Float32Array(n * 3);
    for (let i = 0; i < n; i++) nor[i * 3 + 2] = 1;
    const mesh = r.createMesh({ positions: pos, normals: nor, indices: new Uint32Array([special, 0, 1]) });
    const o = r.createObject(mesh, white, IDENT);
    objs.push(o);
    cases.push({ n, special, o });
  }
  for (const c of cases) {
    for (const o of objs) r.setVisible(o, o === c.o);
    r.frame();
    const stats = r.readStats(new Float64Array(8));
    c.pixel = centre(r.gl);
    c.drawn = c.pixel[0] > 200;
    c.statsTriangles = stats[4];
    delete c.o;
  }
  out.notes.restart = cases;
  log('P1 a 65,535-vertex mesh (u16, max index 65534) draws its triangle', cases[0].drawn, cases[0]);
  log('P1 a 65,536-vertex mesh (u16, index 65535) draws its triangle', cases[1].drawn, cases[1]);
  log('P1 a 65,537-vertex mesh (u32) draws its triangle', cases[2].drawn, cases[2]);
  out.notes.restartGetError = r.gl.getError();
  r.dispose();
}

// 2. Lighting against three.js r180 itself: the same quad, MeshPhongMaterial with web3d's mapping.
const shininessOf = (roughness) => {
  const rr = Math.max(Math.min(Math.max(roughness, 0), 1), 0.03);
  return Math.min(Math.max(2 / (rr * rr * rr * rr) - 2, 1), 2048);
};
async function lighting() {
  const S = 32;
  const r = await createRenderer(document.getElementById('b'), { width: S, height: S, antialias: false });
  const tcanvas = document.getElementById('c');
  tcanvas.width = S;
  tcanvas.height = S;
  const tr = new THREE.WebGLRenderer({ canvas: tcanvas, antialias: false });
  tr.setPixelRatio(1);
  tr.setSize(S, S, false);
  out.notes.threeRevision = THREE.REVISION;
  out.notes.threeToneMapping = tr.toneMapping;
  out.notes.threeOutput = tr.outputColorSpace;
  const cam = new THREE.PerspectiveCamera(40, 1, 0.1, 100);
  cam.updateMatrixWorld();
  cam.updateProjectionMatrix();
  r.setCamera(Array.from(cam.matrixWorldInverse.elements), Array.from(cam.projectionMatrix.elements));
  r.setClearColor(0, 0, 0);
  const norm = (v) => {
    const l = Math.hypot(...v);
    return v.map((x) => x / l);
  };
  const CASES = [
    { name: 'key only, grey dielectric, 45 deg', base: [0.6, 0.6, 0.6], m: 0, rough: 0.8, n: [0, 0, 1], to: [0, 1, 1], key: 2.0, sky: [0, 0, 0], ground: [0, 0, 0] },
    { name: 'oblique normal, glossy, hemisphere', base: [0.5, 0.25, 0.1], m: 0, rough: 0.35, n: [0.3, 0.5, 0.81], to: [0.6, 0.5, 0.8], key: 1.4, sky: [0.5, 0.55, 0.6], ground: [0.12, 0.1, 0.08] },
    { name: 'half metal, downward normal, hemisphere', base: [0.2, 0.5, 0.3], m: 0.5, rough: 0.5, n: [-0.2, -0.6, 0.77], to: [-0.3, -0.4, 1], key: 1.1, sky: [0.8, 0.8, 0.9], ground: [0.3, 0.25, 0.2] },
    { name: 'full metal, on-peak highlight', base: [0.9, 0.6, 0.3], m: 1, rough: 0.3, n: [0.1435, 0.0718, 0.9871], to: [0.3, 0.15, 1], key: 0.02, sky: [0.1, 0.1, 0.1], ground: [0.05, 0.05, 0.05] },
    { name: 'dielectric, on-peak highlight, roughness 0.2', base: [0.1, 0.1, 0.1], m: 0, rough: 0.2, n: [0.1435, 0.0718, 0.9871], to: [0.3, 0.15, 1], key: 0.08, sky: [0, 0, 0], ground: [0, 0, 0] },
    { name: 'skin-like rough red, side key', base: [0.55, 0.32, 0.25], m: 0, rough: 0.68, n: [0.5, 0.1, 0.86], to: [1, 0.3, 0.4], key: 1.6, sky: [0.25, 0.27, 0.3], ground: [0.1, 0.09, 0.08] },
    { name: 'grazing view (65 deg), fresnel', base: [0.3, 0.3, 0.3], m: 0, rough: 0.25, n: [0, 0.9, 0.436], to: [0, 1, -0.2], key: 0.15, sky: [0.3, 0.3, 0.35], ground: [0.1, 0.1, 0.1] },
  ];
  const results = [];
  for (const c of CASES) {
    const n = norm(c.n);
    // web3d
    const mesh = r.createMesh(quad(-5, n));
    const mat = r.createMaterial({ baseColor: [...c.base, 1], roughness: c.rough, metallic: c.m });
    const obj = r.createObject(mesh, mat, IDENT);
    r.setKeyLight(...c.to, 1, 1, 1, c.key);
    r.setAmbient(...c.sky, ...c.ground, 1);
    r.frame();
    const got = centre(r.gl);
    r.destroyObject(obj);
    r.destroyMaterial(mat);
    r.destroyMesh(mesh);
    // three.js r180
    const scene = new THREE.Scene();
    const geo = new THREE.BufferGeometry();
    const q = quad(-5, n);
    geo.setAttribute('position', new THREE.BufferAttribute(q.positions, 3));
    geo.setAttribute('normal', new THREE.BufferAttribute(q.normals, 3));
    geo.setIndex(new THREE.BufferAttribute(q.indices, 1));
    const f0 = c.base.map((b) => 0.04 * (1 - c.m) + b * c.m);
    const material = new THREE.MeshPhongMaterial({
      color: new THREE.Color().setRGB(...c.base.map((b) => b * (1 - c.m))),
      specular: new THREE.Color().setRGB(...f0),
      shininess: shininessOf(c.rough),
    });
    scene.add(new THREE.Mesh(geo, material));
    const dl = new THREE.DirectionalLight(new THREE.Color().setRGB(1, 1, 1), c.key);
    dl.position.set(...c.to.map((x) => x * 10));
    scene.add(dl);
    scene.add(new THREE.HemisphereLight(new THREE.Color().setRGB(...c.sky), new THREE.Color().setRGB(...c.ground), 1));
    tr.setClearColor(0x000000, 1);
    tr.render(scene, cam);
    const three = centre(tr.getContext());
    // web3d's environment stand-in, added in linear: f0 * hemisphere(reflect(-v, n).y) / pi, v = +Z.
    const v = [0, 0, 1];
    const ndv = n[0] * v[0] + n[1] * v[1] + n[2] * v[2];
    const ry = 2 * ndv * n[1] - v[1];
    const t = 0.5 * ry + 0.5;
    const env = [0, 1, 2].map((k) => (f0[k] * (c.ground[k] * (1 - t) + c.sky[k] * t)) / PI);
    const expected = [0, 1, 2].map((k) => byte(lin(three[k]) + env[k]));
    const diff = Math.max(...expected.map((e, k) => Math.abs(e - got[k])));
    results.push({ name: c.name, web3d: got, three, envStandIn: env.map((x) => +x.toFixed(4)), expected, maxDiff: diff });
    log(`L ${c.name}: web3d within 2/255 of three.js r180 (plus the stand-in)`, diff <= 2, { web3d: got, three, expected, envStandIn: env });
    material.dispose();
    geo.dispose();
  }
  out.notes.lighting = results;
  out.notes.lightingGetError = r.gl.getError();
  tr.dispose();
  r.dispose();
}

// 3. Blend order: two blended quads, the near one created first, both over the clear colour.
async function blendOrder() {
  const r = await createRenderer(document.getElementById('a2'), { width: 64, height: 64, antialias: false });
  r.setCamera(IDENT, ORTHO);
  r.setKeyLight(0, 0, 1, 0, 0, 0, 0);
  r.setAmbient(1, 1, 1, 1, 1, 1, PI / 1.04);
  r.setClearColor(0, 0, 0);
  const red = r.createMaterial({ baseColor: [1, 0, 0, 0.5], alphaMode: 'blend' });
  const blue = r.createMaterial({ baseColor: [0, 0, 1, 0.5], alphaMode: 'blend' });
  r.createObject(r.createMesh(quad(-3, [0, 0, 1], 1)), red, IDENT);
  r.createObject(r.createMesh(quad(-4, [0, 0, 1], 1)), blue, IDENT);
  r.frame();
  const got = centre(r.gl);
  // Colours: base * (E/pi) * (1 + 0.04/base?) -> with E = pi/1.04 the lit colour is (base + 0.04) / 1.04.
  const lit = (c) => c.map((x) => srgb((x + 0.04) / 1.04) * 255);
  const R = lit([1, 0, 0]);
  const B = lit([0, 0, 1]);
  const backToFront = R.map((x, k) => Math.round(0.5 * x + 0.5 * (0.5 * B[k])));
  const frontToBack = B.map((x, k) => Math.round(0.5 * x + 0.5 * (0.5 * R[k])));
  log('S blended draws composite back to front whatever the creation order', got.every((v, k) => Math.abs(v - backToFront[k]) <= 3), { got, backToFront, frontToBack });
  r.dispose();
}

// 4. Every method after dispose().
async function disposed() {
  const r = await createRenderer(document.getElementById('a3'), { width: 16, height: 16, antialias: false });
  const mesh = r.createMesh(quad(-5));
  const mat = r.createMaterial({});
  const obj = r.createObject(mesh, mat, IDENT);
  r.dispose();
  const calls = {
    resize: () => r.resize(8, 8),
    createMesh: () => r.createMesh(quad(-5)),
    destroyMesh: () => r.destroyMesh(mesh),
    createTexture: () => r.createTexture(new OffscreenCanvas(2, 2)),
    destroyTexture: () => r.destroyTexture(1),
    createMaterial: () => r.createMaterial({}),
    destroyMaterial: () => r.destroyMaterial(mat),
    createObject: () => r.createObject(mesh, mat, IDENT),
    setTransform: () => r.setTransform(obj, IDENT),
    setVisible: () => r.setVisible(obj, false),
    destroyObject: () => r.destroyObject(obj),
    setCamera: () => r.setCamera(IDENT, ORTHO),
    setCameraLookAt: () => r.setCameraLookAt([0, 0, 5], [0, 0, 0], 1, 0.1, 10),
    setKeyLight: () => r.setKeyLight(0, 1, 0, 1, 1, 1, 1),
    setAmbient: () => r.setAmbient(1, 1, 1, 1, 1, 1, 1),
    setClearColor: () => r.setClearColor(0, 0, 0),
    frame: () => r.frame(),
    setCpuTiming: () => r.setCpuTiming(true),
    readStats: () => r.readStats(new Float64Array(8)),
  };
  const res = Object.fromEntries(Object.entries(calls).map(([k, f]) => [k, attempt(f)]));
  const notThrowing = Object.entries(res).filter(([, v]) => !(v.threw && /renderer disposed/.test(v.message))).map(([k, v]) => `${k}: ${JSON.stringify(v)}`);
  const getters = { cpuMs: r.cpuMs, gpuMs: r.gpuMs, lost: attempt(() => r.lost) };
  log('D every method throws "renderer disposed" after dispose(); getters are null', notThrowing.length === 0 && getters.cpuMs === null && getters.gpuMs === null, { notThrowing, getters, disposeAgain: attempt(() => r.dispose()) });
}

// 5. Context loss, the documented order (dispose old, then create new), with the old renderer
// exercised after the restore first; then a second loss and restore of the new one.
async function lossCycle(canvas, label, before) {
  let lostRes;
  let restoredRes;
  const lost = new Promise((res) => (lostRes = res));
  const restored = new Promise((res) => (restoredRes = res));
  const r = await createRenderer(canvas, { width: 64, height: 64, antialias: false, onContextLost: () => lostRes('lost'), onContextRestored: () => restoredRes('restored') });
  r.setCamera(IDENT, ORTHO);
  r.setKeyLight(0, 0, 1, 0, 0, 0, 0);
  r.setAmbient(1, 1, 1, 1, 1, 1, PI);
  const tex = r.createTexture(new OffscreenCanvas(4, 4));
  const tex2 = r.createTexture(new OffscreenCanvas(8, 8));
  const mesh = r.createMesh(quad(-5));
  const mesh2 = r.createMesh(quad(-6));
  const matT = r.createMaterial({ texture: tex });
  const mat2 = r.createMaterial({ baseColor: [0, 1, 0, 1] });
  const o1 = r.createObject(mesh, mat2, IDENT);
  const o2 = r.createObject(mesh2, matT, IDENT);
  // A running loop, as an application has, through the loss and the restore.
  let running = true;
  let frames = 0;
  let falseFrames = 0;
  const m = IDENT.slice();
  const tick = () => {
    if (!running) return;
    m[12] = Math.sin(frames * 0.1) * 0.01;
    try {
      r.setTransform(o1, m);
      if (r.frame()) frames++;
      else falseFrames++;
    } catch (e) {
      out.notes[`${label}-tickError`] = String(e.message);
    }
    requestAnimationFrame(tick);
  };
  requestAnimationFrame(tick);
  await timeout(200);
  const k = { framesBefore: frames, getErrorBefore: r.gl.getError() };
  const lc = r.gl.getExtension('WEBGL_lose_context');
  lc.loseContext();
  k.lostEvent = await Promise.race([lost, timeout(2000)]);
  await timeout(50);
  k.whileLost = {
    createMesh: attempt(() => r.createMesh(quad(-5))).threw,
    destroyObject: attempt(() => r.destroyObject(o2)),
    destroyMaterial: attempt(() => r.destroyMaterial(matT)),
  };
  lc.restoreContext();
  k.restoredEvent = await Promise.race([restored, timeout(3000)]);
  await timeout(200); // the old loop keeps calling frame() and setTransform() after the restore
  k.falseFrames = falseFrames;
  k.firstGetErrorAfterRestore = r.gl.getError();
  // The old renderer, after the restore, before it is disposed: destroys that would reach GL.
  k.afterRestore = {
    destroyTexture: attempt(() => r.destroyTexture(tex)),
    destroyObject: attempt(() => r.destroyObject(o1)),
    destroyMaterial2: attempt(() => r.destroyMaterial(mat2)),
    destroyMesh: attempt(() => r.destroyMesh(mesh)),
    destroyTexture2: attempt(() => r.destroyTexture(tex2)),
    setVisible: attempt(() => r.setVisible(o2, false)),
    createTexture: attempt(() => r.createTexture(new OffscreenCanvas(2, 2))).threw,
  };
  k.getErrorAfterOldCalls = r.gl.getError();
  running = false;
  r.dispose(); // the documented order: dispose first
  k.getErrorAfterDispose = r.gl.getError();
  const fresh = await createRenderer(canvas, { width: 64, height: 64, antialias: false });
  fresh.setCamera(IDENT, ORTHO);
  fresh.setKeyLight(0, 0, 1, 0, 0, 0, 0);
  fresh.setAmbient(1, 1, 1, 1, 1, 1, PI);
  const t = fresh.createTexture(new OffscreenCanvas(4, 4));
  fresh.createObject(fresh.createMesh(quad(-5)), fresh.createMaterial({ baseColor: [0, 1, 0, 1] }), IDENT);
  fresh.createObject(fresh.createMesh(quad(-7)), fresh.createMaterial({ texture: t }), IDENT);
  for (let i = 0; i < 5; i++) {
    fresh.frame();
    await new Promise((res) => requestAnimationFrame(res));
  }
  fresh.frame();
  k.freshPixel = centre(fresh.gl);
  k.freshGetError = fresh.gl.getError();
  k.freshGpuMs = fresh.gpuMs;
  out.notes[label] = k;
  log(`${label}: old renderer finished (frame false, creates throw), destroys after restore harmless, getError 0, new renderer draws`, k.lostEvent === 'lost' && k.restoredEvent === 'restored' && k.falseFrames > 0 && k.whileLost.createMesh && k.afterRestore.createTexture && k.getErrorAfterOldCalls === 0 && k.getErrorAfterDispose === 0 && k.freshGetError === 0 && k.freshPixel[1] > 200, k);
  return fresh;
}

async function main() {
  const probe = document.createElement('canvas').getContext('webgl2');
  const dbg = probe.getExtension('WEBGL_debug_renderer_info');
  out.renderer = probe.getParameter(dbg.UNMASKED_RENDERER_WEBGL);
  await restart();
  await lighting();
  await blendOrder();
  await disposed();
  const canvas = document.getElementById('d');
  const second = await lossCycle(canvas, 'K1 first loss');
  second.dispose();
  const third = await lossCycle(canvas, 'K2 second loss on the same canvas');
  third.dispose();
  out.passed = out.tests.filter((t) => t.pass).length;
  out.failed = out.tests.filter((t) => !t.pass).map((t) => t.name);
}

window.__critic.done = main().then(
  () => out,
  (e) => {
    out.error = String(e?.stack ?? e);
    return out;
  },
);
