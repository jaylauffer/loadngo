// Critic 11: attacks on canvasKind's realm check (one realm drives a canvas).
import * as web3d from '/pkg/web3d.js';
import { sleep, until, frame, loadInto, err, tryCreate, drawQuad } from './common.js';
const out = {};
const T = async (name, f) => { try { out[name] = await f(); } catch (e) { out[name] = { threw: err(e) }; } };
const protoOf = (c, w) => Object.getPrototypeOf(c) === w.HTMLCanvasElement.prototype;
const ADD = EventTarget.prototype.addEventListener;
const GETCTX = HTMLCanvasElement.prototype.getContext;

async function sameOriginFrame() {
  const f = await frame('frame.html');
  await until(() => f.contentWindow.ready);
  return f;
}

/** Loses the canvas's context, and says whether the lost event was cancelled and a restore came. */
async function loseAndRestore(canvas, gl, lose) {
  const seen = { lostEvents: 0, prevented: null, restored: false };
  ADD.call(canvas, 'webglcontextlost', (e) => { seen.lostEvents++; seen.prevented = e.defaultPrevented; });
  ADD.call(canvas, 'webglcontextrestored', () => { seen.restored = true; });
  lose.loseContext();
  await sleep(200);
  seen.isLost = gl.isContextLost();
  try { lose.restoreContext(); } catch (e) { seen.restoreThrew = err(e); }
  seen.errorAfterRestoreCall = WebGL2RenderingContext.prototype.getError.call(gl);
  await sleep(400);
  seen.lostAfter = gl.isContextLost();
  return seen;
}

// A1. Adoption, frame -> top: a canvas the frame's document made, appended to this document.
await T('A1 frame canvas adopted into the top document', async () => {
  const f = await sameOriginFrame();
  const fw = f.contentWindow;
  const c = fw.document.createElement('canvas');
  document.body.appendChild(c); // adopts it: its node document is now this one
  const r = { ownerIsTop: c.ownerDocument === document, protoIsFrames: protoOf(c, fw), protoIsTops: protoOf(c, window) };
  const top = await tryCreate(web3d, c);
  r.topCopy = top.ok ? 'accepted' : top.error;
  if (top.ok) top.r.dispose();
  const fr = await tryCreate(fw.web3d, c);
  r.frameCopy = fr.ok ? 'accepted' : fr.error;
  if (fr.ok) { const d = drawQuad(fr.r); r.frameCopyDraws = { drew: d.drew, centre: d.centre, error: d.error }; fr.r.dispose(); }
  f.remove();
  return r;
});

// A2. The F1/F3 failure, through adoption: the frame's copy drives a canvas now in the top
// document; the frame is removed; the canvas lives on here. Is its next loss still cancelled?
// Control: the same with the frame kept.
for (const remove of [false, true]) {
  await T(`A2 frame's renderer on an adopted canvas, frame ${remove ? 'removed' : 'kept (control)'}, then a loss`, async () => {
    const f = await sameOriginFrame();
    const fw = f.contentWindow;
    const c = fw.document.createElement('canvas');
    document.body.appendChild(c);
    let lostHook = 0, restoredHook = 0;
    const fr = await tryCreate(fw.web3d, c, { onContextLost: () => lostHook++, onContextRestored: () => restoredHook++ });
    if (!fr.ok) return { create: fr.error };
    const d = drawQuad(fr.r);
    const gl = fr.r.gl;
    const lose = WebGL2RenderingContext.prototype.getExtension.call(gl, 'WEBGL_lose_context');
    if (remove) { fr.r.dispose(); f.remove(); await sleep(100); }
    const seen = await loseAndRestore(c, gl, lose);
    const res = { drewFirst: d.drew, centre: d.centre, ...seen, lostHook, restoredHook, canvasStillInTop: c.isConnected && c.ownerDocument === document };
    // Afterwards: can this document's own copy take the canvas it now holds?
    const top = await tryCreate(web3d, c);
    res.topCopyAfter = top.ok ? 'accepted' : top.error;
    if (top.ok) top.r.dispose();
    if (!remove) { fr.r.dispose(); f.remove(); }
    return res;
  });
}

// A3. Adoption, top -> frame: a canvas this document made, appended to the frame's document.
await T('A3 top canvas adopted into a frame document', async () => {
  const f = await sameOriginFrame();
  const fw = f.contentWindow;
  const c = document.createElement('canvas');
  fw.document.body.appendChild(c);
  const r = { ownerIsFrame: c.ownerDocument === fw.document, protoIsTops: protoOf(c, window), protoIsFrames: protoOf(c, fw) };
  const top = await tryCreate(web3d, c);
  r.topCopy = top.ok ? 'accepted' : top.error;
  if (top.ok) top.r.dispose();
  const fr = await tryCreate(fw.web3d, c);
  r.frameCopy = fr.ok ? 'accepted' : fr.error;
  if (fr.ok) fr.r.dispose();
  f.remove();
  return r;
});

// A4. document.adoptNode and importNode, called each way.
await T('A4 adoptNode / importNode / createElement through another document', async () => {
  const f = await sameOriginFrame();
  const fw = f.contentWindow;
  const r = {};
  const a = document.adoptNode(fw.document.createElement('canvas'));
  r.topAdoptNode = { owner: a.ownerDocument === document ? 'top' : 'frame', proto: protoOf(a, window) ? 'top' : protoOf(a, fw) ? 'frame' : '?' };
  const i = document.importNode(fw.document.createElement('canvas'));
  r.topImportNode = { owner: i.ownerDocument === document ? 'top' : 'frame', proto: protoOf(i, window) ? 'top' : protoOf(i, fw) ? 'frame' : '?' };
  const i2 = fw.document.importNode(document.createElement('canvas'));
  r.frameDocImportNodeCalledFromTop = { owner: i2.ownerDocument === document ? 'top' : 'frame', proto: protoOf(i2, window) ? 'top' : protoOf(i2, fw) ? 'frame' : '?' };
  const e = fw.document.createElement('canvas');
  r.frameDocCreateElementCalledFromTop = { proto: protoOf(e, window) ? 'top' : protoOf(e, fw) ? 'frame' : '?' };
  const e2 = fw.eval("parent.document.createElement('canvas')");
  r.topDocCreateElementCalledFromFrame = { owner: e2.ownerDocument === document ? 'top' : 'frame', proto: protoOf(e2, window) ? 'top' : protoOf(e2, fw) ? 'frame' : '?' };
  fw.document.body.innerHTML = '<canvas id="p"></canvas>';
  const p = fw.document.getElementById('p');
  r.frameParsedCanvas = { proto: protoOf(p, window) ? 'top' : protoOf(p, fw) ? 'frame' : '?' };
  for (const [k, c] of [['adoptNode', a], ['importNode', i]]) {
    const t = await tryCreate(web3d, c);
    r[`${k}: top copy`] = t.ok ? 'accepted' : t.error;
    if (t.ok) t.r.dispose();
  }
  f.remove();
  return r;
});

// A5. Customised built-in canvases and subclasses.
await T('A5 customised built-in canvas (is=)', async () => {
  class XCanvas extends HTMLCanvasElement {}
  customElements.define('x-canvas', XCanvas, { extends: 'canvas' });
  const c = document.createElement('canvas', { is: 'x-canvas' });
  const t = await tryCreate(web3d, c);
  const r = { isX: c instanceof XCanvas, top: t.ok ? 'accepted' : t.error };
  if (t.ok) t.r.dispose();
  return r;
});
await T('A5b subclass made in a frame, extending the top HTMLCanvasElement, registered here', async () => {
  const f = await sameOriginFrame();
  const fw = f.contentWindow;
  const C = fw.eval('(class extends parent.HTMLCanvasElement {})');
  customElements.define('y-canvas', C, { extends: 'canvas' });
  const c = document.createElement('canvas', { is: 'y-canvas' });
  document.body.appendChild(c);
  const t = await tryCreate(web3d, c);
  const fr = await tryCreate(fw.web3d, c);
  const r = { top: t.ok ? 'accepted' : t.error, frame: fr.ok ? 'accepted' : fr.error };
  if (t.ok) t.r.dispose();
  if (fr.ok) fr.r.dispose();
  f.remove();
  return r;
});

// A6. A page that swaps a prototype (out of scope by the docs; recorded).
await T('A6 Object.setPrototypeOf on a frame canvas', async () => {
  const f = await sameOriginFrame();
  const fw = f.contentWindow;
  const c = fw.document.createElement('canvas');
  fw.document.body.appendChild(c);
  Object.setPrototypeOf(c, HTMLCanvasElement.prototype);
  const t = await tryCreate(web3d, c);
  const r = { topCopyOnFrameDocCanvas: t.ok ? 'accepted' : t.error };
  if (t.ok) t.r.dispose();
  f.remove();
  return r;
});

// A7. about:blank and srcdoc frames.
await T('A7 about:blank frame', async () => {
  const f = await frame(null);
  const fw = f.contentWindow;
  await loadInto(fw);
  const own = fw.document.createElement('canvas');
  fw.document.body.appendChild(own);
  const r = {};
  let t = await tryCreate(fw.web3d, own); r.ownCanvas = t.ok ? 'accepted' : t.error; if (t.ok) t.r.dispose();
  t = await tryCreate(fw.web3d, document.createElement('canvas')); r.topCanvas = t.ok ? 'accepted' : t.error; if (t.ok) t.r.dispose();
  t = await tryCreate(web3d, own); r.topCopyOnItsCanvas = t.ok ? 'accepted' : t.error; if (t.ok) t.r.dispose();
  r.sharesHome = fw[Symbol.for('loadngo-web3d')] === undefined && window[Symbol.for('loadngo-web3d')] !== undefined;
  f.remove();
  return r;
});
await T('A7b srcdoc frame', async () => {
  const f = await frame(null, { srcdoc: `<body><script type="module">import * as m from '/pkg/web3d.js'; window.web3d = m; window.ready = true;<\/script></body>` });
  const fw = f.contentWindow;
  await until(() => fw.ready);
  const own = fw.document.createElement('canvas');
  const r = {};
  let t = await tryCreate(fw.web3d, own); r.ownCanvas = t.ok ? 'accepted' : t.error; if (t.ok) t.r.dispose();
  t = await tryCreate(fw.web3d, document.createElement('canvas')); r.topCanvas = t.ok ? 'accepted' : t.error; if (t.ok) t.r.dispose();
  f.remove();
  return r;
});

// A8. A canvas moved between documents after its renderer was made (this window's copy).
await T('A8 top renderer, canvas then moved into a frame, frame removed, then a loss', async () => {
  const f = await sameOriginFrame();
  const fw = f.contentWindow;
  const c = document.createElement('canvas');
  document.body.appendChild(c);
  let lostHook = 0, restoredHook = 0;
  const t = await tryCreate(web3d, c, { onContextLost: () => lostHook++, onContextRestored: () => restoredHook++ });
  if (!t.ok) return { create: t.error };
  fw.document.body.appendChild(c);
  const d = drawQuad(t.r);
  const lose = t.r.gl.getExtension('WEBGL_lose_context');
  f.remove();
  await sleep(100);
  const seen = await loseAndRestore(c, t.r.gl, lose);
  t.r.dispose();
  return { drewInFrame: d.drew, centre: d.centre, ...seen, lostHook, restoredHook };
});

// A9. A same-origin popup.
await T('A9 window.open popup', async () => {
  const w = window.open('frame.html', '_blank');
  if (!w) return { popup: 'blocked' };
  await until(() => w.ready);
  const r = { popupTopIsItself: w.top === w };
  let t = await tryCreate(w.web3d, document.createElement('canvas')); r.popupCopyOnOpenerCanvas = t.ok ? 'accepted' : t.error; if (t.ok) t.r.dispose();
  const pc = w.document.createElement('canvas');
  t = await tryCreate(web3d, pc); r.openerCopyOnPopupCanvas = t.ok ? 'accepted' : t.error; if (t.ok) t.r.dispose();
  // Adoption into the popup's document.
  const oc = document.createElement('canvas');
  w.document.body.appendChild(oc);
  t = await tryCreate(web3d, oc); r.openerCopyOnItsCanvasNowInPopup = t.ok ? 'accepted' : t.error; if (t.ok) t.r.dispose();
  // Popup's canvas adopted here, driven by the popup's copy, popup closed, then a loss.
  const ac = w.document.createElement('canvas');
  document.body.appendChild(ac);
  t = await tryCreate(w.web3d, ac);
  r.popupCopyOnItsCanvasNowHere = t.ok ? 'accepted' : t.error;
  if (t.ok) {
    const d = drawQuad(t.r);
    const gl = t.r.gl;
    const lose = WebGL2RenderingContext.prototype.getExtension.call(gl, 'WEBGL_lose_context');
    t.r.dispose();
    w.close();
    await sleep(300);
    r.afterPopupClosed = { drewFirst: d.drew, ...(await loseAndRestore(ac, gl, lose)) };
    const again = await tryCreate(web3d, ac);
    r.afterPopupClosed.openerCopyOnIt = again.ok ? 'accepted' : again.error;
    if (again.ok) again.r.dispose();
  } else w.close();
  return r;
});

// A10. Workers.
await T('A10 worker: OffscreenCanvas made there, transferred there, and sent back', async () => {
  const worker = new Worker('worker.js', { type: 'module' });
  const ask = (msg, transfer = []) => new Promise((res) => { worker.onmessage = (e) => res(e.data); worker.postMessage(msg, transfer); });
  const r = {};
  r.own = await ask({ op: 'own' });
  const placeholder = document.createElement('canvas');
  const off = placeholder.transferControlToOffscreen();
  r.transferred = await ask({ op: 'take', canvas: off }, [off]);
  const back = await ask({ op: 'giveBack' });
  r.sentBack = back.error ?? 'received';
  if (back.canvas) {
    const t = await tryCreate(web3d, back.canvas);
    r.windowCopyOnCanvasFromWorker = t.ok ? 'accepted' : t.error;
    if (t.ok) t.r.dispose();
  }
  const local = new OffscreenCanvas(8, 8);
  r.windowOffscreenSentToWorkerNotTransferred = await ask({ op: 'probeClone' }).catch((e) => err(e));
  worker.terminate();
  return r;
});

window.__done = Promise.resolve(out);
