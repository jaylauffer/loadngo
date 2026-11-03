// Eighth critic's probes, batch 2: 'bitmaprenderer' canvases against the post-upload check, the
// restoreContext timing WEB3D.md states, and the documented recovery over two cycles.
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
const event = (target, type) => new Promise((res) => target.addEventListener(type, () => setTimeout(() => res(type), 0), { once: true }));
const made = [];
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
const OPTS = { width: 16, height: 16, antialias: false };
const greenBitmap = (w, h) => createImageBitmap(new ImageData(new Uint8ClampedArray(w * h * 4).map((_, i) => (i % 4 === 1 || i % 4 === 3 ? 255 : 0)), w, h));

/** E13: bitmaprenderer canvases (element and Offscreen) of sizes other than their bitmap's. */
async function bitmapRenderers() {
  const r = await createRenderer(canvas(16, 16), OPTS);
  flat(r);
  const d = {};
  let silent = false;
  const cases = [
    ['element 16x16, bitmap 8x8', 16, 16, 8, 8, false],
    ['element 4x4, bitmap 8x8', 4, 4, 8, 8, false],
    ['element 300x150 default, bitmap 8x8', 300, 150, 8, 8, false],
    ['offscreen 16x16, bitmap 8x8', 16, 16, 8, 8, true],
    ['offscreen 4x4, bitmap 8x8', 4, 4, 8, 8, true],
  ];
  for (const [name, cw, ch, bw, bh, offscreen] of cases) {
    const c = offscreen ? new OffscreenCanvas(cw, ch) : Object.assign(document.createElement('canvas'), { width: cw, height: ch });
    c.getContext('bitmaprenderer').transferFromImageBitmap(await greenBitmap(bw, bh));
    const reported = [c.width, c.height];
    while (r.gl.getError());
    const t = attempt(() => r.createTexture(c, { srgb: false, mipmaps: false, wrapS: 'clamp', wrapT: 'clamp' }));
    const e = { reported, texture: t.threw ? t.message.slice(0, 120) : 'created', getErrorAfter: r.gl.getError() };
    if (!t.threw) {
      const m = r.createMaterial({ texture: t.value });
      const o = r.createObject(r.createMesh(quad()), m);
      r.frame();
      e.quadrants = [pixel(r, -0.5, 0.5), pixel(r, 0.5, 0.5), pixel(r, -0.5, -0.5), pixel(r, 0.5, -0.5)];
      if (!e.quadrants.every(green)) silent = true;
      r.destroyObject(o);
    }
    d[name] = e;
  }
  r.dispose();
  log('E13', "a 'bitmaprenderer' canvas whose bitmap does not fit its reported size is refused (WEB3D.md: \"refused by the check after the upload\"), never made and partly filled", !silent, d);
}

/** E14: WEB3D.md: "Chrome allows restoreContext() only after the lost event's dispatch has returned". */
async function restoreInsideDispatch() {
  const c = canvas();
  const gl = c.getContext('webgl2');
  const lc = gl.getExtension('WEBGL_lose_context');
  const d = {};
  const restored = event(c, 'webglcontextrestored');
  c.addEventListener('webglcontextlost', (e) => {
    e.preventDefault();
    lc.restoreContext();
    d.errorInside = gl.getError();
  }, { once: true });
  lc.loseContext();
  d.restoredFromInside = await Promise.race([restored, timeout(1500)]);
  if (d.restoredFromInside !== 'webglcontextrestored') {
    const again = event(c, 'webglcontextrestored');
    lc.restoreContext();
    d.restoredAfter = await Promise.race([again, timeout(1500)]);
  }
  log('E14', 'restoreContext() inside the lost event does not restore; after the dispatch it does', d.restoredFromInside !== 'webglcontextrestored' && d.restoredAfter === 'webglcontextrestored', d);
}

/** E15: the documented recovery (dispose in onContextLost, create on onContextRestored), two cycles in a row. */
async function twoCycles() {
  const c = canvas();
  const d = { cycles: [] };
  let r = null;
  let resolveRestored;
  const make = () =>
    createRenderer(c, {
      ...OPTS,
      onContextLost: () => r?.dispose(),
      onContextRestored: () => resolveRestored?.(),
    });
  r = await make();
  for (let i = 0; i < 2; i++) {
    const restored = new Promise((res) => (resolveRestored = res));
    const lc = r.gl.getExtension('WEBGL_lose_context');
    const lost = event(c, 'webglcontextlost');
    lc.loseContext();
    await lost;
    lc.restoreContext();
    const got = await Promise.race([restored.then(() => 'hook'), timeout(3000)]);
    const next = await settle(make());
    const cycle = { got, next: next.ok ? 'created' : next.message };
    if (next.ok) {
      r = next.r;
      flat(r);
      r.createObject(r.createMesh(quad()), r.createMaterial({ baseColor: [0, 1, 0, 1] }));
      r.frame();
      cycle.pixel = pixel(r, 0, 0);
      cycle.getError = r.gl.getError();
    }
    d.cycles.push(cycle);
  }
  r.dispose();
  log('E15', 'the documented recovery works twice in a row on one canvas', d.cycles.every((x) => x.got === 'hook' && green(x.pixel) && x.getError === 0), d);
}

async function main() {
  for (const p of [bitmapRenderers, restoreInsideDispatch, twoCycles]) {
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
