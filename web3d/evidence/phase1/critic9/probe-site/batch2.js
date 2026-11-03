// Ninth critic, batch 2: controls and follow-ups to probe9.js.
import { createRenderer } from '/web3d/pkg/web3d.js';

const out = { probes: [] };
window.__probe9 = { done: null, out };
const log = (id, claim, pass, detail) => out.probes.push({ id, claim, pass: Boolean(pass), detail });
const IDENT = new Float32Array([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1]);
const ORTHO = new Float32Array([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, -2 / 9.9, 0, 0, 0, -10.1 / 9.9, 1]);
const quad = { positions: new Float32Array([-1, -1, -5, 1, -1, -5, 1, 1, -5, -1, 1, -5]), normals: new Float32Array([0, 0, 1, 0, 0, 1, 0, 0, 1, 0, 0, 1]), indices: new Uint32Array([0, 1, 2, 0, 2, 3]) };
const event = (t, type) => new Promise((res) => t.addEventListener(type, () => setTimeout(() => res(type), 0), { once: true }));
const timeout = (ms) => new Promise((res) => setTimeout(() => res('timeout'), ms));
const settle = (p) => p.then((r) => ({ ok: true, r }), (e) => ({ ok: false, message: String(e?.message ?? e) }));
const attempt = (fn) => {
  try {
    return { threw: false, value: fn() };
  } catch (e) {
    return { threw: true, message: String(e?.message ?? e) };
  }
};
const OPTS = { width: 16, height: 16, antialias: false };
const draw = (r) => {
  r.setCamera(IDENT, ORTHO);
  r.setKeyLight(0, 0, 1, 0, 0, 0, 0);
  r.setAmbient(1, 1, 1, 1, 1, 1, Math.PI);
  r.setClearColor(0, 0, 0);
  r.createObject(r.createMesh(quad), r.createMaterial({ baseColor: [0, 1, 0, 1] }));
  r.frame();
  const px = new Uint8Array(4);
  r.gl.readPixels(8, 8, 1, 1, r.gl.RGBA, r.gl.UNSIGNED_BYTE, px);
  return [...px.slice(0, 3)];
};
const green = (p) => Array.isArray(p) && p[1] > 200 && p[0] < 60 && p[2] < 60;
let n = 0;
const canvas = () => {
  const c = document.createElement('canvas');
  c.id = `b2-${++n}`;
  c.width = 16;
  c.height = 16;
  document.body.append(c);
  return c;
};
const frame = async () => {
  const iframe = document.createElement('iframe');
  iframe.src = '/c/frame.html';
  document.body.append(iframe);
  await new Promise((res) => iframe.addEventListener('load', res, { once: true }));
  for (let i = 0; i < 100 && !iframe.contentWindow.__frame; i++) await timeout(20);
  return iframe;
};

/** N10c: N10's control: the same, with the frame kept. */
async function n10c() {
  const d = {};
  const c = canvas();
  const iframe = await frame();
  const theirs = await iframe.contentWindow.__frame.createRenderer(c, OPTS);
  d.theirs = draw(theirs);
  theirs.dispose();
  const calls = { lost: 0, restored: 0 };
  const ours = await createRenderer(c, { ...OPTS, onContextLost: () => calls.lost++, onContextRestored: () => calls.restored++ });
  const lc = ours.gl.getExtension('WEBGL_lose_context');
  const lost = event(c, 'webglcontextlost');
  lc.loseContext();
  await lost;
  d.afterLoss = { ...calls };
  const restored = event(c, 'webglcontextrestored');
  lc.restoreContext();
  d.restored = await Promise.race([restored, timeout(1000)]);
  await timeout(20);
  d.calls = calls;
  ours.dispose();
  iframe.remove();
  log('N10c', "N10's control: the frame that first took the canvas's context is kept; the top window's renderer's loss is cancelled and its hooks run", d.afterLoss.lost === 1 && d.restored === 'webglcontextrestored' && calls.restored === 1, d);
}

/** N10e: after N10's state, does the canvas ever recover? A later create, after the frame is gone, on a fresh page-free loss. */
async function n10e() {
  const d = {};
  const c = canvas();
  const iframe = await frame();
  const theirs = await iframe.contentWindow.__frame.createRenderer(c, OPTS);
  theirs.dispose();
  iframe.remove();
  await timeout(50);
  // A renderer made by the top window after the frame is gone: are new listeners added? Is a loss cancelled?
  const r = await createRenderer(c, OPTS);
  const lc = r.gl.getExtension('WEBGL_lose_context');
  r.dispose();
  const lost = event(c, 'webglcontextlost');
  lc.loseContext();
  await lost;
  const restored = event(c, 'webglcontextrestored');
  lc.restoreContext();
  d.restored = await Promise.race([restored, timeout(1000)]);
  d.create = (await settle(createRenderer(c, OPTS))).message ?? 'created';
  log('N10e', 'after the frame that first used the canvas is gone, a canvas web3d disposed from is still restorable after a loss', d.restored === 'webglcontextrestored', d);
}

/** N7b': lost (event not yet dispatched) when the create starts: what the create says, and what happens. */
async function n7b() {
  const d = {};
  const c = canvas();
  const gl = c.getContext('webgl2', { antialias: false });
  const lc = gl.getExtension('WEBGL_lose_context');
  let calls = 0;
  lc.loseContext();
  d.create = (await settle(createRenderer(c, { ...OPTS, onContextRestored: () => calls++ }))).message;
  await timeout(30); // the lost event has been dispatched by now
  const restored = event(c, 'webglcontextrestored');
  lc.restoreContext();
  d.restored = await Promise.race([restored, timeout(1000)]);
  await timeout(20);
  d.calls = calls;
  log(
    "N7b'",
    "a page-made context lost in the same task as the create (its event not yet dispatched): the create's message says it 'can be restored only if the page cancelled the lost event'; the page cancelled nothing",
    !(d.restored === 'webglcontextrestored' && /only if the page cancelled/.test(d.create)),
    d,
  );
}

/** N13: a worker's own copy of the module, on an OffscreenCanvas transferred to it: draw, lose, dispose, restore, draw. */
async function n13() {
  const c = canvas();
  const off = c.transferControlToOffscreen();
  const worker = new Worker('/c/worker.js', { type: 'module' });
  const d = await Promise.race([
    new Promise((res) => {
      worker.onmessage = (e) => res(e.data);
      worker.onerror = (e) => res({ error: String(e.message) });
      worker.postMessage({ canvas: off }, [off]);
    }),
    timeout(10000),
  ]);
  worker.terminate();
  log('N13', "a worker's copy: an OffscreenCanvas draws, is lost, its renderer disposed while lost gets onContextRestored once, a new renderer draws", !d?.error && green(d.first) && d.restored === 'webglcontextrestored' && d.calls?.restored === 1 && d.calls?.lost === 1 && green(d.next), d);
}

/** N21: canvases that cannot be read, as texture sources. */
async function n21() {
  const d = {};
  const host = canvas();
  const r = await createRenderer(host, OPTS);
  const placeholder = document.createElement('canvas');
  placeholder.transferControlToOffscreen();
  d.placeholder = attempt(() => r.createTexture(placeholder));
  const sent = new OffscreenCanvas(4, 4);
  structuredClone(sent, { transfer: [sent] }); // detached: sent elsewhere
  d.detachedOffscreen = attempt(() => r.createTexture(sent));
  r.dispose();
  log('N21', "texture sources that cannot be read are refused with a message that names their state", d.placeholder.threw && d.detachedOffscreen.threw && !/transferred to an OffscreenCanvas/.test(d.detachedOffscreen.message), d);
}

async function main() {
  for (const p of [n10c, n10e, n7b, n13, n21]) {
    try {
      await p();
    } catch (e) {
      log(p.name.toUpperCase(), 'the probe ran to its end', false, String(e?.stack ?? e));
    }
  }
  out.passed = out.probes.filter((p) => p.pass).length;
}
window.__probe9.done = main().then(() => out);
