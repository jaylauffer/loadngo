// GENERATED from probe9.js: the same probes against fix/web3d.js, a copy of pkg/web3d.js with two
// proposed fixes (dispose queues on shim.lost(); resetState before init). Ninth critic's probes for web3d phase 1 (round nine: one loss controller a canvas, bitmaprenderer
// canvases refused). window.__probe9.done resolves to { probes: [{ id, claim, pass, detail }], passed }.
// `pass` means the behaviour is right, or the documented claim holds; a FAIL is a finding.
// window.__listeners(id), when the driver provides it, counts a top-document element's event
// listeners through the DevTools protocol (DOMDebugger.getEventListeners).
import { createRenderer } from '/c/fix/web3d.js';
// A second copy of the module: another URL is another module record, with its own PENDING, COMPILED
// and functions, sharing only the state kept on the top window.
import { createRenderer as createRendererB } from '/c/fix/web3d.js?copy=b';

const out = { probes: [] };
window.__probe9 = { done: null, out };
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
let serial = 0;
const made = [];
/** A fresh canvas in the document, with an id; old probes' contexts are released so Chrome's context limit drops none of ours. */
const canvas = (w = 16, h = 16) => {
  while (made.length > 8) {
    const old = made.shift();
    try {
      old.getContext('webgl2')?.getExtension('WEBGL_lose_context')?.loseContext();
    } catch {
      // a 2d or bitmaprenderer canvas
    }
  }
  const c = document.createElement('canvas');
  c.id = `c9-${++serial}`;
  made.push(c);
  c.width = w;
  c.height = h;
  document.body.append(c);
  return c;
};
const pixelOf = (gl, x, y) => {
  const px = new Uint8Array(4);
  gl.readPixels(x, y, 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, px);
  return [px[0], px[1], px[2]];
};
const centre = (r) => pixelOf(r.gl, Math.floor(r.canvas.width / 2), Math.floor(r.canvas.height / 2));
const green = (p) => Array.isArray(p) && p[1] > 200 && p[0] < 60 && p[2] < 60;
const event = (target, type) => new Promise((res) => target.addEventListener(type, () => setTimeout(() => res(type), 0), { once: true }));
const OPTS = { width: 16, height: 16, antialias: false, wasm: '/web3d/pkg/loadngo_web3d.wasm' };
/** Draws a green quad and returns the centre pixel, read in the same task: proof a renderer works. */
const drawsGreen = (r) => {
  flat(r);
  r.createObject(r.createMesh(quad()), r.createMaterial({ baseColor: [0, 1, 0, 1] }));
  r.frame();
  return centre(r);
};
const listeners = async (c) => (typeof window.__listeners === 'function' ? window.__listeners(c.id) : 'no driver');
const loseAndRestore = async (c, lc, between) => {
  const lost = event(c, 'webglcontextlost');
  lc.loseContext();
  await lost;
  if (between) await between();
  const restored = event(c, 'webglcontextrestored');
  lc.restoreContext();
  return Promise.race([restored, timeout(3000)]);
};
const isLost = (m) => /context is lost/.test(m ?? '');

// Errors that hooks throw on purpose reach window's error event through reportError; count them.
const reported = [];
window.addEventListener('error', (e) => {
  if (String(e.error?.message ?? '').startsWith('c9:')) {
    reported.push(e.error.message);
    e.preventDefault();
  }
});

/** N1: the page's own restored listener (added before web3d first used the canvas) disposes the renderer live at the loss. */
async function n1() {
  const c = canvas();
  const calls = { restored: 0, lost: 0 };
  let r1 = null;
  let disposedIn = null;
  c.addEventListener('webglcontextrestored', () => {
    disposedIn = r1.gl.isContextLost() ? 'lost' : 'restored';
    r1.dispose();
  });
  r1 = await createRenderer(c, { ...OPTS, onContextLost: () => calls.lost++, onContextRestored: () => calls.restored++ });
  const lc = r1.gl.getExtension('WEBGL_lose_context');
  const got = await loseAndRestore(c, lc);
  await timeout(30);
  const next = await settle(createRenderer(c, OPTS));
  const pixel = next.ok ? drawsGreen(next.r) : next.message;
  if (next.ok) next.r.dispose();
  log(
    'N1',
    "WEB3D.md 'Context loss': on webglcontextrestored the controller calls onContextRestored for the renderer that was live at the loss. Here the page's own restored listener, added before web3d's, disposes that renderer first.",
    got === 'webglcontextrestored' && calls.lost === 1 && calls.restored === 1,
    { got, calls, disposedIn, nextPixel: pixel },
  );
}

/** N1b: the same, in the page's own lost listener (before web3d's): disposed while lost, so the hook is queued. */
async function n1b() {
  const c = canvas();
  const calls = { restored: 0, lost: 0 };
  let r1 = null;
  c.addEventListener('webglcontextlost', () => r1.dispose());
  r1 = await createRenderer(c, { ...OPTS, onContextLost: () => calls.lost++, onContextRestored: () => calls.restored++ });
  const lc = r1.gl.getExtension('WEBGL_lose_context');
  const got = await loseAndRestore(c, lc);
  await timeout(30);
  log('N1b', 'disposed in the page\'s own lost listener (before web3d\'s): onContextRestored still comes once (onContextLost does not: it was no longer live)', got === 'webglcontextrestored' && calls.restored === 1 && calls.lost === 0, { got, calls });
}

/** N2: several failed creates, one disposal and a repeated function on one canvas; each hook once; two listeners. */
async function n2() {
  const c = canvas();
  const calls = { live: 0, a: 0, b: 0, shared: 0 };
  const shared = () => calls.shared++;
  const r1 = await createRenderer(c, { ...OPTS, onContextRestored: () => calls.live++ });
  const before = await listeners(c);
  const lc = r1.gl.getExtension('WEBGL_lose_context');
  const d = {};
  const got = await loseAndRestore(c, lc, async () => {
    d.a = (await settle(createRenderer(c, { ...OPTS, onContextRestored: () => calls.a++ }))).message;
    d.b = (await settle(createRenderer(c, { ...OPTS, onContextRestored: () => calls.b++ }))).message;
    d.s1 = (await settle(createRenderer(c, { ...OPTS, onContextRestored: shared }))).message;
    d.s2 = (await settle(createRendererB(c, { ...OPTS, onContextRestored: shared }))).message; // the other copy
    d.shared = (await settle(createRenderer(c, OPTS))).message;
    r1.dispose();
  });
  await timeout(30);
  const next = await settle(createRenderer(c, OPTS));
  const pixel = next.ok ? drawsGreen(next.r) : next.message;
  const glError = next.ok ? next.r.gl.getError() : -1;
  if (next.ok) next.r.dispose();
  const after = await listeners(c);
  log(
    'N2',
    'three failed creates (one function given twice, once through a second module copy) and the live renderer disposed while lost: each hook runs once on the restore; a new renderer then draws; the canvas holds exactly one lost and one restored listener',
    got === 'webglcontextrestored' && [d.a, d.b, d.s1, d.s2].every(isLost) && calls.live === 1 && calls.a === 1 && calls.b === 1 && calls.shared === 1 &&
      green(pixel) && glError === 0 && (after === 'no driver' || (after.webglcontextlost === 1 && after.webglcontextrestored === 1)),
    { got, d, calls, pixel, glError, listenersBefore: before, listenersAfter: after },
  );
}

/** N3: a loss and its restore both between two checks: the old renderer is finished; a new one may take the canvas undisposed; the old one's dispose later does not touch it. */
async function n3() {
  const c = canvas();
  const calls = { lost: 0, restored: 0 };
  const old = await createRenderer(c, { ...OPTS, onContextLost: () => calls.lost++, onContextRestored: () => calls.restored++ });
  flat(old);
  old.frame();
  const lc = old.gl.getExtension('WEBGL_lose_context');
  const got = await loseAndRestore(c, lc);
  const d = { got, calls: { ...calls } };
  d.frame = attempt(() => old.frame());
  d.lostFlag = old.lost;
  d.mesh = attempt(() => old.createMesh(quad()));
  d.resize = attempt(() => old.resize(8, 8));
  const fresh = await settle(createRenderer(c, OPTS)); // the old one is not disposed
  d.fresh = fresh.ok ? 'created' : fresh.message;
  if (fresh.ok) {
    flat(fresh.r);
    fresh.r.createObject(fresh.r.createMesh(quad()), fresh.r.createMaterial({ baseColor: [0, 1, 0, 1] }));
    old.dispose(); // must send nothing into the restored context, which the new renderer owns
    fresh.r.frame();
    d.pixelAfterOldDispose = centre(fresh.r);
    d.glError = fresh.r.gl.getError();
    d.freshStillOwns = attempt(() => fresh.r.resize(16, 16));
    fresh.r.dispose();
  }
  log(
    'N3',
    'a loss and restore both between two checks finish the old renderer (frame() false, creates and resize throw); a new renderer may take the canvas while the old is undisposed; disposing the old one then sends nothing into the new one\'s context',
    got === 'webglcontextrestored' && d.frame.value === false && d.lostFlag === true && d.mesh.threw && isLost(d.mesh.message) && d.resize.threw &&
      d.fresh === 'created' && green(d.pixelAfterOldDispose) && d.glError === 0 && !d.freshStillOwns.threw && calls.lost === 1 && calls.restored === 1,
    d,
  );
}

/** N4: disposed after loseContext() but before the lost event: no onContextLost, and onContextRestored once. */
async function n4() {
  const c = canvas();
  const calls = { lost: 0, restored: 0 };
  const r = await createRenderer(c, { ...OPTS, onContextLost: () => calls.lost++, onContextRestored: () => calls.restored++ });
  const lc = r.gl.getExtension('WEBGL_lose_context');
  const lost = event(c, 'webglcontextlost');
  lc.loseContext();
  const before = { contextLost: r.gl.isContextLost(), frame: attempt(() => r.frame()).value };
  r.dispose();
  await lost;
  const restored = event(c, 'webglcontextrestored');
  lc.restoreContext();
  const got = await Promise.race([restored, timeout(3000)]);
  await timeout(20);
  log('N4', 'disposed between loseContext() and the lost event: onContextLost is not called (no live renderer), onContextRestored is called once', got === 'webglcontextrestored' && calls.lost === 0 && calls.restored === 1 && before.frame === false, { before, got, calls });
}

/** N5: hooks that throw and hooks that create. */
async function n5() {
  const c = canvas();
  const d = { order: [] };
  let inLost = null;
  let inRestored = null;
  const r1 = await createRenderer(c, {
    ...OPTS,
    onContextLost: () => {
      d.order.push('lost');
      // A create from inside onContextLost: it fails as lost; its hook comes on the restore.
      inLost = settle(
        createRenderer(c, {
          ...OPTS,
          onContextRestored: () => {
            d.order.push('restored(inner)');
            inRestored = settle(createRenderer(c, OPTS));
          },
        }),
      );
      throw new Error('c9: onContextLost throws on purpose');
    },
    onContextRestored: () => {
      d.order.push('restored(live)');
      throw new Error('c9: onContextRestored throws on purpose');
    },
  });
  const lc = r1.gl.getExtension('WEBGL_lose_context');
  const got = await loseAndRestore(c, lc, async () => {
    d.inLost = (await inLost).message;
  });
  await timeout(30);
  const second = inRestored ? await inRestored : { ok: false, message: 'no create in the restored hook' };
  d.second = second.ok ? 'created' : second.message;
  d.pixel = second.ok ? drawsGreen(second.r) : null;
  d.reported = [...reported];
  if (second.ok) second.r.dispose();
  r1.dispose();
  log(
    'N5',
    'a throwing onContextLost and onContextRestored are reported and stop nothing; a create made inside onContextLost fails as lost and its hook runs once on the restore; a create made inside that hook draws',
    got === 'webglcontextrestored' && isLost(d.inLost) && d.order.filter((o) => o === 'restored(inner)').length === 1 &&
      d.order.includes('restored(live)') && d.second === 'created' && green(d.pixel) &&
      d.reported.includes('c9: onContextLost throws on purpose') && d.reported.includes('c9: onContextRestored throws on purpose'),
    d,
  );
}

/** N6: hooks that dispose other renderers, on another canvas and on the same one. */
async function n6() {
  const d = {};
  // Another canvas: A's onContextLost disposes B; B's canvas then takes a new renderer.
  const ca = canvas();
  const cb = canvas();
  let rb = null;
  const ra = await createRenderer(ca, { ...OPTS, onContextLost: () => (rb.dispose(), ra.dispose()) });
  rb = await createRenderer(cb, OPTS);
  const lca = ra.gl.getExtension('WEBGL_lose_context');
  d.a = await loseAndRestore(ca, lca);
  const nb = await settle(createRenderer(cb, OPTS));
  d.bAfter = nb.ok ? drawsGreen(nb.r) : nb.message;
  if (nb.ok) nb.r.dispose();
  // The same canvas: old renderer lost and left undisposed; after the restore a new one; on the
  // next loss the new one's onContextLost disposes both; its onContextRestored creates a third.
  const c = canvas();
  const calls = { oldLost: 0, oldRestored: 0, newLost: 0, newRestored: 0 };
  const old = await createRenderer(c, { ...OPTS, onContextLost: () => calls.oldLost++, onContextRestored: () => calls.oldRestored++ });
  const lc = old.gl.getExtension('WEBGL_lose_context');
  await loseAndRestore(c, lc);
  let third = null;
  const now = await createRenderer(c, {
    ...OPTS,
    onContextLost: () => {
      calls.newLost++;
      old.dispose();
      now.dispose();
    },
    onContextRestored: () => {
      calls.newRestored++;
      third = settle(createRenderer(c, OPTS));
    },
  });
  d.same = await loseAndRestore(c, lc);
  await timeout(30);
  const t = third ? await third : { ok: false, message: 'none' };
  d.third = t.ok ? drawsGreen(t.r) : t.message;
  d.calls = calls;
  if (t.ok) t.r.dispose();
  log(
    'N6',
    "hooks that dispose: A's onContextLost disposes B on another canvas (B's canvas takes a new renderer); on one canvas the new renderer's onContextLost disposes the old one and itself, its onContextRestored creates a third that draws; the old one hears nothing more",
    d.a === 'webglcontextrestored' && green(d.bAfter) && d.same === 'webglcontextrestored' && green(d.third) &&
      calls.oldLost === 1 && calls.oldRestored === 1 && calls.newLost === 1 && calls.newRestored === 1,
    d,
  );
}

/** N7: a canvas lost before web3d first used it. */
async function n7() {
  const d = {};
  // a) A page-made context lost, its event dispatched and not cancelled: never restorable.
  {
    const c = canvas();
    const gl = c.getContext('webgl2', { antialias: false });
    const lc = gl.getExtension('WEBGL_lose_context');
    const lost = event(c, 'webglcontextlost');
    lc.loseContext();
    await lost;
    let calls = 0;
    d.aCreate = (await settle(createRenderer(c, { ...OPTS, onContextRestored: () => calls++ }))).message;
    const restored = event(c, 'webglcontextrestored');
    lc.restoreContext();
    d.aRestoreError = gl.getError();
    d.aRestored = await Promise.race([restored, timeout(500)]);
    d.aCalls = calls;
  }
  // b) Lost, but its event not yet dispatched when the create starts: web3d's listener comes first
  // and cancels it, so the canvas is restored and the hook comes, although the message says the page
  // had to cancel it.
  {
    const c = canvas();
    const gl = c.getContext('webgl2', { antialias: false });
    const lc = gl.getExtension('WEBGL_lose_context');
    let calls = 0;
    let prevented = null;
    lc.loseContext();
    const create = settle(createRenderer(c, { ...OPTS, onContextRestored: () => calls++ }));
    await new Promise((res) => c.addEventListener('webglcontextlost', (e) => ((prevented = e.defaultPrevented), setTimeout(res, 0)), { once: true }));
    d.bCreate = (await create).message;
    d.bPrevented = prevented;
    const restored = event(c, 'webglcontextrestored');
    lc.restoreContext();
    d.bRestored = await Promise.race([restored, timeout(1000)]);
    await timeout(20);
    d.bCalls = calls;
    const again = await settle(createRenderer(c, OPTS));
    d.bAgain = again.ok ? drawsGreen(again.r) : again.message;
    if (again.ok) again.r.dispose();
  }
  // c) Lost and cancelled by the page itself: the create says so, the hook comes on the restore.
  {
    const c = canvas();
    const gl = c.getContext('webgl2', { antialias: false });
    c.addEventListener('webglcontextlost', (e) => e.preventDefault(), { once: true });
    const lc = gl.getExtension('WEBGL_lose_context');
    const lost = event(c, 'webglcontextlost');
    lc.loseContext();
    await lost;
    let calls = 0;
    d.cCreate = (await settle(createRenderer(c, { ...OPTS, onContextRestored: () => calls++ }))).message;
    const restored = event(c, 'webglcontextrestored');
    lc.restoreContext();
    d.cRestored = await Promise.race([restored, timeout(1000)]);
    await timeout(20);
    d.cCalls = calls;
    const again = await settle(createRenderer(c, OPTS));
    d.cAgain = again.ok ? drawsGreen(again.r) : again.message;
    if (again.ok) again.r.dispose();
  }
  const before = (m) => /lost before web3d first used this canvas/.test(m ?? '');
  log('N7a', 'a page-made context lost (event not cancelled) before web3d used the canvas: the create says it was lost before web3d; no restore and no hook (as WEB3D.md says)', before(d.aCreate) && d.aRestored === 'timeout' && d.aCalls === 0, d);
  log('N7b', 'lost but its event not yet dispatched when the create starts: the canvas is restored and the hook comes once, and the message is right about it', d.bPrevented === true && d.bRestored === 'webglcontextrestored' && d.bCalls === 1 && green(d.bAgain) && !before(d.bCreate), { bCreate: d.bCreate, bPrevented: d.bPrevented, bRestored: d.bRestored, bCalls: d.bCalls, bAgain: d.bAgain });
  log('N7c', 'lost and cancelled by the page before web3d used the canvas: the hook comes once on the restore and a new renderer draws', before(d.cCreate) && d.cRestored === 'webglcontextrestored' && d.cCalls === 1 && green(d.cAgain), { cCreate: d.cCreate, cRestored: d.cRestored, cCalls: d.cCalls, cAgain: d.cAgain });
}

/** N8: a page-made context that suits (depth, antialias as asked), handed over with state the page left. */
async function n8() {
  const cases = [
    ['scissor test on, a 1x1 box at the corner', (gl) => (gl.enable(gl.SCISSOR_TEST), gl.scissor(0, 0, 1, 1))],
    ['colour mask all false', (gl) => gl.colorMask(false, false, false, false)],
    ['depth func NEVER', (gl) => gl.depthFunc(gl.NEVER)],
    ['cull face FRONT', (gl) => gl.cullFace(gl.FRONT)],
    ['clear depth 0', (gl) => gl.clearDepth(0)],
    ['depth range 1..1', (gl) => gl.depthRange(1, 1)],
    ['rasterizer discard on', (gl) => gl.enable(gl.RASTERIZER_DISCARD)],
    ['draw buffers NONE on the default framebuffer', (gl) => gl.drawBuffers([gl.NONE])],
    [
      'a framebuffer of its own left bound',
      (gl) => {
        const fb = gl.createFramebuffer();
        const tex = gl.createTexture();
        gl.bindTexture(gl.TEXTURE_2D, tex);
        gl.texStorage2D(gl.TEXTURE_2D, 1, gl.RGBA8, 16, 16);
        gl.bindFramebuffer(gl.FRAMEBUFFER, fb);
        gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.TEXTURE_2D, tex, 0);
        gl.bindTexture(gl.TEXTURE_2D, null);
      },
    ],
    ['nothing changed (control)', () => {}],
  ];
  const results = [];
  for (const [name, mutate] of cases) {
    const c = canvas();
    const gl = c.getContext('webgl2', { antialias: false });
    mutate(gl);
    const pageError = gl.getError();
    const r = await settle(createRenderer(c, OPTS));
    if (!r.ok) {
      results.push({ name, created: r.message });
      continue;
    }
    flat(r.r);
    r.r.createObject(r.r.createMesh(quad()), r.r.createMaterial({ baseColor: [0, 1, 0, 1] }));
    r.r.frame();
    const glError = gl.getError();
    // Read the canvas's own drawing buffer, whatever framebuffer the page left bound.
    const fb = gl.getParameter(gl.READ_FRAMEBUFFER_BINDING);
    gl.bindFramebuffer(gl.READ_FRAMEBUFFER, null);
    const pixel = pixelOf(gl, 8, 8);
    gl.bindFramebuffer(gl.READ_FRAMEBUFFER, fb);
    results.push({ name, pageError, glError, pixel, drawn: green(pixel) });
    r.r.dispose();
  }
  // Texture uploads after page-set unpack state.
  const uploads = [
    ['UNPACK_SKIP_PIXELS 1', (gl) => gl.pixelStorei(gl.UNPACK_SKIP_PIXELS, 1)],
    ['UNPACK_SKIP_ROWS 1', (gl) => gl.pixelStorei(gl.UNPACK_SKIP_ROWS, 1)],
    ['a PIXEL_UNPACK_BUFFER left bound', (gl) => gl.bindBuffer(gl.PIXEL_UNPACK_BUFFER, gl.createBuffer())],
    ['nothing changed (control)', () => {}],
  ];
  const bitmap = await createImageBitmap(new ImageData(new Uint8ClampedArray(4 * 4 * 4).fill(255), 4, 4), { premultiplyAlpha: 'none', colorSpaceConversion: 'none' });
  for (const [name, mutate] of uploads) {
    const c = canvas();
    const gl = c.getContext('webgl2', { antialias: false });
    mutate(gl);
    const r = await settle(createRenderer(c, OPTS));
    if (!r.ok) {
      results.push({ name, created: r.message });
      continue;
    }
    const t = attempt(() => r.r.createTexture(bitmap));
    results.push({ name, texture: t.threw ? t.message.slice(0, 160) : 'created', drawn: !t.threw });
    r.r.dispose();
  }
  log(
    'N8',
    "WEB3D.md: a page-made context is refused only without depth or with another antialias; one that suits is used. Does web3d draw (and upload) right with the state a page left on it?",
    results.every((x) => x.drawn === true),
    results,
  );
}

/** N9: two module copies share the controller: one pair of listeners, hooks across copies, losses counted for both. */
async function n9() {
  const c = canvas();
  const d = {};
  const calls = { a: 0, b: 0, bLost: 0, b2Restored: 0 };
  const ra = await createRenderer(c, { ...OPTS, onContextRestored: () => calls.a++ });
  d.bWhileALive = (await settle(createRendererB(c, OPTS))).message;
  const lc = ra.gl.getExtension('WEBGL_lose_context');
  d.first = await loseAndRestore(c, lc, async () => {
    d.bWhileLost = (await settle(createRendererB(c, { ...OPTS, onContextRestored: () => calls.b++ }))).message;
    ra.dispose();
  });
  await timeout(20);
  const rb = await settle(createRendererB(c, { ...OPTS, onContextLost: () => calls.bLost++, onContextRestored: () => calls.b2Restored++ }));
  d.rb = rb.ok ? drawsGreen(rb.r) : rb.message;
  if (rb.ok) {
    d.second = await loseAndRestore(c, lc);
    await timeout(20);
    d.rbFrame = rb.r.frame();
    rb.r.dispose();
  }
  d.listeners = await listeners(c);
  d.calls = calls;
  log(
    'N9',
    'two copies of the module share one controller a canvas: one copy\'s live renderer refuses the other\'s create; hooks from both copies run once; the second copy\'s renderer hears the next loss and restore; the canvas has one listener of each kind',
    /already has a live renderer/.test(d.bWhileALive) && isLost(d.bWhileLost) && d.first === 'webglcontextrestored' && calls.a === 1 && calls.b === 1 &&
      green(d.rb) && d.second === 'webglcontextrestored' && calls.bLost === 1 && calls.b2Restored === 1 && d.rbFrame === false &&
      (d.listeners === 'no driver' || (d.listeners.webglcontextlost === 1 && d.listeners.webglcontextrestored === 1)),
    d,
  );
}

const frame = async () => {
  const iframe = document.createElement('iframe');
  iframe.src = '/c/frame.html';
  document.body.append(iframe);
  await new Promise((res) => iframe.addEventListener('load', res, { once: true }));
  for (let i = 0; i < 100 && !iframe.contentWindow.__frame; i++) await timeout(20);
  return iframe;
};

/** N10: a same-origin frame's copy first takes a context from a top-document canvas; then the frame goes away. */
async function n10() {
  const d = {};
  const c = canvas();
  const iframe = await frame();
  const theirs = await iframe.contentWindow.__frame.createRenderer(c, OPTS);
  d.theirs = drawsGreen(theirs);
  theirs.dispose();
  iframe.remove();
  await timeout(50);
  const calls = { lost: 0, restored: 0 };
  const ours = await createRenderer(c, { ...OPTS, onContextLost: () => calls.lost++, onContextRestored: () => calls.restored++ });
  d.ours = drawsGreen(ours);
  d.listenersOnCanvas = await listeners(c);
  const lc = ours.gl.getExtension('WEBGL_lose_context');
  let prevented = null;
  c.addEventListener('webglcontextlost', (e) => (prevented = e.defaultPrevented), { once: true });
  const lost = event(c, 'webglcontextlost');
  lc.loseContext();
  await lost;
  d.prevented = prevented;
  d.callsAfterLoss = { ...calls };
  const restored = event(c, 'webglcontextrestored');
  lc.restoreContext();
  d.restoreError = ours.gl.getError();
  d.restored = await Promise.race([restored, timeout(1000)]);
  await timeout(20);
  d.calls = calls;
  ours.dispose();
  const next = await settle(createRenderer(c, OPTS));
  d.next = next.ok ? drawsGreen(next.r) : next.message;
  if (next.ok) next.r.dispose();
  log(
    'N10',
    "WEB3D.md: the controller's two listeners stay for the canvas's life, cancel every loss and call the hooks. A same-origin frame's copy of the module first takes the canvas's context; the frame is removed; the top window's renderer on that canvas then loses its context",
    green(d.theirs) && green(d.ours) && d.prevented === true && d.callsAfterLoss.lost === 1 && d.restored === 'webglcontextrestored' && calls.restored === 1 && green(d.next),
    d,
  );
}

/** N10b: a frame's own canvas, its own copy; the top window's renderer elsewhere is untouched by the frame's loss; listeners on the frame's canvas. */
async function n10b() {
  const d = {};
  const iframe = await frame();
  const doc = iframe.contentDocument;
  const fc = doc.createElement('canvas');
  fc.width = 16;
  fc.height = 16;
  doc.body.append(fc);
  const calls = { lost: 0, restored: 0 };
  const theirs = await iframe.contentWindow.__frame.createRenderer(fc, { ...OPTS, onContextLost: () => calls.lost++, onContextRestored: () => calls.restored++ });
  const oc = canvas();
  const ours = await createRenderer(oc, OPTS);
  const lc = theirs.gl.getExtension('WEBGL_lose_context');
  d.got = await loseAndRestore(fc, lc);
  await timeout(20);
  d.calls = calls;
  d.oursFrame = ours.frame();
  d.ours = drawsGreen(ours);
  theirs.dispose();
  ours.dispose();
  iframe.remove();
  log('N10b', "a frame's renderer on the frame's own canvas loses and restores; its hooks run once; the top window's renderer is untouched", d.got === 'webglcontextrestored' && calls.lost === 1 && calls.restored === 1 && d.oursFrame === true && green(d.ours), d);
}

/** N11: the bitmaprenderer refusal asks a context-less canvas for a '2d' context. What does that change? */
async function n11() {
  const d = {};
  const host = canvas();
  const r = await createRenderer(host, OPTS);
  // An element canvas with no context, passed as a texture source.
  const el = document.createElement('canvas');
  el.width = 4;
  el.height = 4;
  d.elTexture = attempt(() => r.createTexture(el)).threw ? 'refused' : 'created';
  d.elWebgl2After = el.getContext('webgl2') === null ? 'null: the canvas can no longer have a WebGL context' : 'a context';
  d.elBitmaprendererAfter = el.getContext('bitmaprenderer') === null ? 'null' : 'a context';
  d.elTransfer = attempt(() => el.transferControlToOffscreen()).threw ? 'throws: can no longer be transferred' : 'transferred';
  d.el2dOptions = el.getContext('2d', { alpha: false, willReadFrequently: true })?.getContextAttributes();
  // createRenderer on it afterwards.
  d.elRenderer = (await settle(createRenderer(el, OPTS))).message ?? 'created';
  // An OffscreenCanvas with no context.
  const off = new OffscreenCanvas(4, 4);
  d.offTexture = attempt(() => r.createTexture(off)).threw ? 'refused' : 'created';
  d.offWebgl2After = off.getContext('webgl2') === null ? 'null' : 'a context';
  // Control: the same calls without createTexture.
  const control = document.createElement('canvas');
  d.controlWebgl2 = control.getContext('webgl2') === null ? 'null' : 'a context';
  // A canvas with a webgl2 context is untouched (both getContext calls return null).
  const glc = document.createElement('canvas');
  glc.width = 4;
  glc.height = 4;
  const g2 = glc.getContext('webgl2');
  g2.clearColor(0, 1, 0, 1);
  g2.clear(g2.COLOR_BUFFER_BIT);
  d.webglSource = attempt(() => r.createTexture(glc)).threw ? 'refused' : 'created';
  // A bitmaprenderer canvas the same size as its bitmap is refused too (by design).
  const same = document.createElement('canvas');
  same.width = 4;
  same.height = 4;
  same.getContext('bitmaprenderer').transferFromImageBitmap(await createImageBitmap(new ImageData(4, 4)));
  d.sameSize = attempt(() => r.createTexture(same));
  r.dispose();
  log(
    'N11',
    "WEB3D.md createTexture: telling a bitmaprenderer canvas apart asks the canvas for a '2d' context; 'a canvas with none gets a blank 2d one, which uploads the same transparent pixels'. Does that leave the caller's canvas as it was in every way that matters?",
    d.elWebgl2After !== 'null: the canvas can no longer have a WebGL context' && d.elTransfer === 'transferred',
    d,
  );
}

async function main() {
  for (const p of [n1, n1b, n2, n3, n4, n5, n6, n8, n9]) {
    try {
      await p();
    } catch (e) {
      log(p.name.toUpperCase(), 'the probe ran to its end', false, String(e?.stack ?? e));
    }
  }
  out.passed = out.probes.filter((p) => p.pass).length;
}
window.__probe9.done = main().then(() => out);
