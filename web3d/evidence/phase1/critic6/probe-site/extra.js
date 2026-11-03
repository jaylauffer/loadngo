// Sixth critic's second page (a fresh page, since G14 spends the page's serials).
//   G9a  a page-made context without a depth buffer, drawn so that only the depth test can order it
//   G16  a renderer on an OffscreenCanvas
//   G14  the last serial: its handles are exact, it draws; the next create is refused cleanly
// BSD-3-Clause, as loadngo.

import { createRenderer } from '/web3d/pkg/web3d.js';

const out = { probes: [] };
window.__limit = { done: null, out };
const log = (id, claim, pass, detail) => out.probes.push({ id, claim, pass: Boolean(pass), detail });
const N = 0.1;
const F = 10;
const ORTHO = new Float32Array([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, -2 / (F - N), 0, 0, 0, -(F + N) / (F - N), 1]);
const IDENT = new Float32Array([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1]);
const quad = (z) => ({
  positions: new Float32Array([-1, -1, z, 1, -1, z, 1, 1, z, -1, 1, z]),
  normals: new Float32Array([0, 0, 1, 0, 0, 1, 0, 0, 1, 0, 0, 1]),
  indices: new Uint32Array([0, 1, 2, 0, 2, 3]),
});
const settle = (p) => p.then((r) => ({ ok: true, r }), (e) => ({ ok: false, message: String(e?.message ?? e) }));
const flat = (r) => {
  r.setCamera(IDENT, ORTHO);
  r.setKeyLight(0, 0, 1, 0, 0, 0, 0);
  r.setAmbient(1, 1, 1, 1, 1, 1, Math.PI);
  r.setClearColor(0, 0, 0);
};
const pixel = (r) => {
  const px = new Uint8Array(4);
  r.gl.readPixels(Math.floor(r.canvas.width / 2), Math.floor(r.canvas.height / 2), 1, 1, r.gl.RGBA, r.gl.UNSIGNED_BYTE, px);
  return [...px];
};
const canvas = (w = 16, h = 16) => {
  const c = document.createElement('canvas');
  c.width = w;
  c.height = h;
  document.body.appendChild(c);
  return c;
};
const decode = (h) => {
  const b = BigInt(h);
  return { serial: Number(b >> 33n), kind: Number((b >> 30n) & 7n), generation: Number((b >> 16n) & 0x3fffn), index: Number(b & 0xffffn) };
};

/** Near green (its material made first, so drawn first), far red after it: only depth keeps green. */
function scene(r) {
  flat(r);
  const near = r.createMaterial({ baseColor: [0, 1, 0, 1] });
  const far = r.createMaterial({ baseColor: [1, 0, 0, 1] });
  r.createObject(r.createMesh(quad(-2)), near);
  r.createObject(r.createMesh(quad(-5)), far);
  r.frame();
  return pixel(r);
}

async function g9aDepth() {
  const d = {};
  const control = await createRenderer(canvas(), { width: 16, height: 16, antialias: false });
  d.controlPixel = scene(control);
  control.dispose();
  const c = canvas();
  c.getContext('webgl2', { antialias: false, depth: false });
  const res = await settle(createRenderer(c, { width: 16, height: 16, antialias: false }));
  d.created = res.ok ? 'created' : res.message;
  if (res.ok) {
    d.contextDepth = res.r.gl.getContextAttributes().depth;
    d.pixel = scene(res.r);
    d.glError = res.r.gl.getError();
    res.r.dispose();
  }
  const isGreen = (p) => p && p[1] > 200 && p[0] < 60;
  log(
    'G9a',
    'opaque draws rely on the depth test (WEB3D.md "A frame"; probe E "depth, whatever the creation order"): a page-made context without a depth buffer should be refused, as one with the wrong antialias is, not draw far over near',
    isGreen(d.controlPixel) && (d.created !== 'created' || isGreen(d.pixel)),
    d,
  );
}

async function g16Offscreen() {
  const oc = new OffscreenCanvas(16, 16);
  const res = await settle(createRenderer(oc, { width: 16, height: 16, antialias: false }));
  const d = { created: res.ok ? 'created' : res.message };
  if (res.ok) {
    d.pixel = scene(res.r);
    d.glError = res.r.gl.getError();
    res.r.dispose();
  }
  log('G16', 'WEB3D.md createRenderer takes an OffscreenCanvas: it draws there like a canvas', d.created === 'created' && d.pixel[1] > 200 && d.glError === 0, d);
}

async function g14LastSerial() {
  // A test hook on the shared state, to reach the end of the serials without making a million renderers.
  const shared = globalThis[Symbol.for('loadngo-web3d')];
  shared.serials.next = 2 ** 20 - 2;
  const c1 = canvas();
  const c2 = canvas();
  const a = await createRenderer(c1, { width: 16, height: 16, antialias: false });
  const b = await createRenderer(c2, { width: 16, height: 16, antialias: false });
  flat(b);
  const d = { serials: [a._serial, b._serial] };
  const am = a.createMesh(quad(-5));
  const bm = b.createMesh(quad(-5));
  const bmat = b.createMaterial({ baseColor: [0, 1, 0, 1] });
  const bo = b.createObject(bm, bmat);
  for (let i = 0; i < 3; i++) b.destroyObject(b.createObject(bm, bmat));
  d.handles = { am, bm, bmat, bo, boDecoded: decode(bo) };
  d.exact = [am, bm, bmat, bo].every((h) => Number.isSafeInteger(h) && h < 2 ** 53);
  b.frame();
  d.pixel = pixel(b);
  d.crossRefused = (() => { try { b.destroyMesh(am); return false; } catch (e) { return /another renderer/.test(e.message); } })();
  const c3 = canvas(20, 10);
  const next = await settle(createRenderer(c3, { width: 48, height: 24, antialias: false }));
  d.next = next.ok ? 'created' : next.message;
  const again = await settle(createRenderer(c3, { width: 48, height: 24, antialias: false }));
  d.again = again.ok ? 'created' : again.message;
  d.size = [c3.width, c3.height];
  a.dispose();
  b.dispose();
  log(
    'G14',
    'WEB3D.md "a page may make 2^20-1 renderers": the last serial issues exact handles below 2^53 and draws; the next create is refused, leaves the canvas unclaimed and its size as it was',
    d.serials[1] === 2 ** 20 - 1 && d.exact && d.pixel[1] > 200 && d.crossRefused && /1048575 renderers/.test(d.next) && /1048575 renderers/.test(d.again) && d.size[0] === 20 && d.size[1] === 10,
    d,
  );
}

async function main() {
  for (const p of [g9aDepth, g16Offscreen, g14LastSerial]) {
    try {
      await p();
    } catch (e) {
      log(p.name, 'the probe ran to the end', false, { error: String(e?.stack ?? e) });
    }
  }
  out.passed = out.probes.filter((t) => t.pass).length;
  out.failed = out.probes.filter((t) => !t.pass).map((t) => t.id);
}
window.__limit.done = main().then(() => out);
