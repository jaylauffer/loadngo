// loadngo-web3d: a lean WebGL2 renderer in Rust/WebAssembly. This module loads the wasm and
// wraps its exports in a `Renderer` class. See docs/WEB3D.md in loadngo.
//
//   import { createRenderer } from './web3d.js';
//   const r = await createRenderer(canvas, { width: 1024, height: 576 });
//   const mesh = r.createMesh({ positions, normals, uvs, indices });
//   const material = r.createMaterial({ baseColor: [0.8, 0.2, 0.2, 1] });
//   r.createObject(mesh, material, matrixWorld.elements);
//   r.setCamera(camera.matrixWorldInverse.elements, camera.projectionMatrix.elements);
//   requestAnimationFrame(function tick() { r.frame(); requestAnimationFrame(tick); });
//
// Colours are linear, light intensities in three.js r180's units, matrices column-major (three.js's
// `elements`). Handles are whole numbers below 2^50 that name one kind of thing in one renderer, and
// no handle is issued twice in a window and its same-origin frames (a worker or a cross-origin frame
// counts its own, from a random start). Nothing on the per-frame path (`setTransform`, `setCamera`,
// `setCameraLookAt`, `setKeyLight`, `frame`, `readStats`) allocates.
//
// The boundary. Every argument is checked before it reaches wasm:
// - typed arrays are recognised by their internal slots, never by `instanceof` or their own
//   properties, so a Proxy or a subclass that lies about `length` cannot change what is staged;
// - numbers must be numbers, in arguments and in the elements of a matrix, vector or colour given
//   as an Array: nothing is converted, so no `valueOf` runs;
// - a caller's object (a descriptor, an options bag, an Array's length and elements) is read once,
//   and a renderer method called from inside that read (a getter or a Proxy trap) throws instead of
//   running;
// - handles carry their kind and their renderer's serial, and the Rust side refuses a handle of
//   another kind or from another renderer.
// Page code that patches built-ins, or reaches into a renderer's private fields, is out of scope.
//
// After `dispose()` every call throws "web3d: renderer disposed". When the WebGL context is lost,
// `frame()` returns false for good, creating anything throws, and the renderer is finished: dispose
// it, then create a new one when `onContextRestored` is called and upload everything again. One loss
// controller a canvas (`controllerFor`) cancels every lost event and calls the hooks; its listeners
// belong to the top window's copy of this module when there is one, else to the copy of the last
// create, and a create puts them back if the window that made them is gone (a removed frame).
//
// A renderer puts WebGL 2's default state back on the context it takes (src/renderer.rs,
// `reset_context_state`), so a context the page made and used draws as a new one would.
//
// BSD-3-Clause; see the repository's LICENSE.

import { createGl } from './gl.js';

const IO_FLOATS = 64;
const STATS_LEN = 8;
/** The largest drawing-buffer side accepted, as the Rust side's MAX_DRAWING_BUFFER. */
const MAX_DRAWING_BUFFER = 32768;
/** Renderer serials run from 1 to below 2^20 (src/slots.rs SERIALS), one per renderer a page makes. */
const SERIALS = 2 ** 20;
/** Every handle is below this (src/slots.rs HANDLE_LIMIT): 2^50, where a number still holds quarters. */
const HANDLE_LIMIT = 2 ** 50;

const DISPOSED = 'web3d: renderer disposed';
const LOST =
  'web3d: the WebGL context is lost; dispose this renderer, then create a new one once the context is restored (onContextRestored is called then, also after dispose) and upload everything again';
const LOST_AT_CREATE =
  "web3d: the canvas's WebGL context is lost; create the renderer again when this call's onContextRestored is called, on the restore";
// A loss no listener of web3d's saw: the create cannot tell whether its lost event is still to come
// (web3d's listener, added now, cancels it) or was dispatched already (only the page could have
// cancelled it), so the message says both.
const LOST_BEFORE_WEB3D =
  "web3d: the canvas's WebGL context was lost before web3d first used this canvas; it is restored if its lost event is cancelled, which web3d now does if that event is still to come (one already dispatched only the page could have cancelled), and then this call's onContextRestored is called";
const LOST_UNWATCHED =
  "web3d: the canvas's WebGL context was lost after the window whose copy of web3d watched this canvas went away; it is restored if its lost event is cancelled, which web3d now does if that event is still to come, and then this call's onContextRestored is called";
const SHARED =
  'web3d: this canvas already has a live renderer; dispose it first (two renderers would share one WebGL context)';
const REENTRANT =
  'web3d: a renderer method was called while the renderer was reading another call\'s arguments (from a getter, valueOf or a Proxy); call it afterwards';

// Maps, not object literals: a name such as 'toString' must not find Object.prototype.
const ALPHA_MODES = new Map([
  ['opaque', 0],
  ['mask', 1],
  ['blend', 2],
]);
const WRAPS = new Map([
  ['repeat', 0],
  ['clamp', 1],
  ['mirror', 2],
]);

// Built-in getters and methods, taken when this module loads, so that what a caller's object says
// about itself (an own `length`, a Proxy trap, a subclass getter) is never what the checks read.
const TYPED_ARRAY = Object.getPrototypeOf(Uint8Array.prototype);
const getter = (proto, name) => Object.getOwnPropertyDescriptor(proto, name)?.get;
const setter = (proto, name) => Object.getOwnPropertyDescriptor(proto, name)?.set;
const TA_TAG = getter(TYPED_ARRAY, Symbol.toStringTag);
const TA_LENGTH = getter(TYPED_ARRAY, 'length');
const TA_SET = TYPED_ARRAY.set;

/**
 * The typed-array kind of `a` ('Float32Array', 'Uint16Array', ...), read from its internal slot:
 * undefined for anything that is not a real typed array, a Proxy over one included.
 */
const typedKind = (a) => (typeof a === 'object' && a !== null ? TA_TAG.call(a) : undefined);

const isArray = (a) => {
  try {
    return Array.isArray(a);
  } catch {
    return false; // a revoked Proxy
  }
};

/** A short description of a value for an error message, without running any of its code. */
function describe(v) {
  switch (typeof v) {
    case 'string':
      return JSON.stringify(v.length > 40 ? `${v.slice(0, 40)}...` : v);
    case 'number':
      return Object.is(v, -0) ? '-0' : String(v);
    case 'bigint':
      return `${v}n`;
    case 'undefined':
    case 'boolean':
      return String(v);
    case 'symbol':
      return 'a symbol';
    case 'function':
      return 'a function';
    default:
      return v === null ? 'null' : (typedKind(v) ?? (isArray(v) ? 'an Array' : 'an object'));
  }
}

/**
 * Checks a handle before it crosses into wasm: a whole number from 1 to 2^50 - 1, passed to wasm as
 * an exact double. A fraction, a numeric string or a Number object is refused here; the Rust side
 * then refuses a number that is not a live handle of this kind in this renderer. Allocates nothing
 * unless it throws.
 */
function handle(what, h) {
  if (!Number.isSafeInteger(h) || h <= 0 || h >= HANDLE_LIMIT) {
    throw new Error(`web3d: ${describe(h)} is not a valid ${what} handle (a whole number from 1 to 2^50 - 1)`);
  }
  return h;
}

/** A number, as given: nothing is converted, so no valueOf runs. */
function num(what, v) {
  if (typeof v !== 'number') throw new Error(`web3d: ${what} must be a number, not ${describe(v)}`);
  return v;
}

/** A Float32Array (a real one), or null if absent. Integer data (quantised positions, normals) is refused. */
function floats(name, a) {
  if (a == null) return null;
  if (typedKind(a) !== 'Float32Array') {
    throw new Error(`web3d: ${name} must be a Float32Array, not ${describe(a)}; dequantise integer data first`);
  }
  return a;
}

function integers(name, a, kinds) {
  if (a == null) return null;
  if (!kinds.includes(typedKind(a))) {
    throw new Error(`web3d: ${name} must be a ${kinds.join(' or ')}, not ${describe(a)}`);
  }
  return a;
}

/** Skin weights as floats: glTF stores them as floats, or as normalised bytes or shorts. */
function weightFloats(a) {
  if (a == null) return null;
  const k = typedKind(a);
  if (k === 'Float32Array') return a;
  if (k !== 'Uint8Array' && k !== 'Uint16Array') {
    throw new Error(`web3d: weights must be a Float32Array, or a Uint8Array or Uint16Array of normalised values, not ${describe(a)}`);
  }
  const n = TA_LENGTH.call(a);
  const scale = k === 'Uint8Array' ? 1 / 255 : 1 / 65535;
  const out = new Float32Array(n);
  // Indexed reads of a real typed array go to its buffer; no getter can run.
  for (let i = 0; i < n; i++) out[i] = a[i] * scale;
  return out;
}

/**
 * Morph targets as a plain list of real Float32Arrays, read once: `targets` may be any array-like,
 * but each element is taken exactly once, so what is checked is what is staged.
 */
function morphList(targets) {
  if (targets == null) return [];
  if (!isArray(targets)) {
    throw new Error(`web3d: morphTargets must be an Array of Float32Array, not ${describe(targets)}`);
  }
  const n = targets.length;
  if (!Number.isInteger(n) || n < 0 || n > 256) {
    throw new Error('web3d: a mesh may have 1 to 256 morph targets');
  }
  const list = [];
  for (let k = 0; k < n; k++) list.push(floats(`morphTargets[${k}]`, targets[k]));
  return list;
}

// The image sources taken, each sized through built-in getters, so a source's own properties are
// never read, and recognised by whichever getter accepts it. A VideoFrame uploads its visible
// rectangle, turned by its rotation, not its display size. A canvas with a 'bitmaprenderer' context
// shows a bitmap of any size at its own, so WebGL would crop or fail on it: it is refused, and the
// ImageBitmap itself is the source to pass. It is found by asking for a '2d' context, then a
// 'bitmaprenderer' one. Neither call changes a canvas that has a context. A canvas with none gets a
// blank '2d' context, for good: it uploads the transparent pixels it would have, but from then on it
// can never have a WebGL, WebGPU or 'bitmaprenderer' context, an element can no longer be
// transferred to an OffscreenCanvas, and a later getContext('2d', options) returns that context
// with its options ignored. No browser call tells whether a canvas has a context without making
// one, so a canvas must be drawn on (have its context) before it is passed.
const sizeBy = (name, w, h) => {
  const proto = globalThis[name]?.prototype;
  const gw = proto && getter(proto, w);
  const gh = proto && getter(proto, h);
  return gw && gh ? (source) => [gw.call(source), gh.call(source)] : null;
};
const RECT = globalThis.DOMRectReadOnly?.prototype;
const RECT_W = RECT && getter(RECT, 'width');
const RECT_H = RECT && getter(RECT, 'height');
const VISIBLE_RECT = globalThis.VideoFrame && getter(globalThis.VideoFrame.prototype, 'visibleRect');
const ROTATION = globalThis.VideoFrame && getter(globalThis.VideoFrame.prototype, 'rotation');
const BITMAPRENDERER =
  "web3d: a canvas with a 'bitmaprenderer' context is not accepted as a texture source, because the bitmap it shows need not be its size; pass the ImageBitmap itself";
/** A canvas kind: sized as the others, but refused when its context is a 'bitmaprenderer'. */
const canvasBy = (name, unreadable) => {
  const size = sizeBy(name, 'width', 'height');
  const getContext = globalThis[name]?.prototype.getContext;
  if (!size || !getContext) return null;
  return (source) => {
    const wh = size(source); // throws for another kind
    try {
      if (getContext.call(source, '2d') === null && getContext.call(source, 'bitmaprenderer') !== null) return BITMAPRENDERER;
    } catch {
      return unreadable; // nothing here to upload
    }
    return wh;
  };
};
const IMAGE_KINDS = [
  sizeBy('ImageBitmap', 'width', 'height'),
  canvasBy('HTMLCanvasElement', 'web3d: this canvas cannot be read here (its control was transferred to an OffscreenCanvas)'),
  canvasBy('OffscreenCanvas', 'web3d: this OffscreenCanvas cannot be read (it was transferred elsewhere, so it is detached)'),
  sizeBy('ImageData', 'width', 'height'),
  VISIBLE_RECT && RECT_W
    ? (frame) => {
        const rect = VISIBLE_RECT.call(frame); // null once the frame is closed
        if (!rect) return [0, 0];
        const w = RECT_W.call(rect);
        const h = RECT_H.call(rect);
        const turned = ROTATION ? ROTATION.call(frame) % 180 !== 0 : false; // 90 or 270 degrees
        return turned ? [h, w] : [w, h];
      }
    : null,
].filter(Boolean);
// Sources whose reported size can differ from the pixels WebGL uploads: an image's naturalWidth
// follows srcset densities and image metadata, a video's videoWidth its display aspect.
const INEXACT_KINDS = [
  ['HTMLImageElement', 'an <img>'],
  ['HTMLVideoElement', 'a <video>'],
]
  .map(([name, what]) => [getter(globalThis[name]?.prototype ?? {}, 'width') ?? null, what])
  .filter(([g]) => g);

function imageSize(source) {
  if (typeof source === 'object' && source !== null) {
    for (const size of IMAGE_KINDS) {
      let wh;
      try {
        wh = size(source);
      } catch {
        continue; // not this kind
      }
      if (typeof wh === 'string') throw new Error(wh);
      return wh;
    }
    for (const [g, what] of INEXACT_KINDS) {
      try {
        g.call(source);
      } catch {
        continue;
      }
      throw new Error(
        `web3d: ${what} is not accepted as a texture source, because its reported size can differ from the pixels WebGL uploads; decode it first with createImageBitmap(source, { premultiplyAlpha: 'none', colorSpaceConversion: 'none' })`,
      );
    }
  }
  throw new Error(`web3d: a texture source must be an ImageBitmap, canvas, OffscreenCanvas, ImageData or VideoFrame, not ${describe(source)}`);
}

// Canvases, recognised and sized through their built-in accessors.
const CANVAS_KINDS = ['HTMLCanvasElement', 'OffscreenCanvas']
  .filter((name) => typeof globalThis[name] === 'function')
  .map((name) => {
    const p = globalThis[name].prototype;
    return { width: getter(p, 'width'), setWidth: setter(p, 'width'), height: getter(p, 'height'), setHeight: setter(p, 'height'), getContext: p.getContext };
  });

function canvasKind(canvas) {
  if (typeof canvas === 'object' && canvas !== null) {
    for (const kind of CANVAS_KINDS) {
      try {
        kind.width.call(canvas);
        return kind;
      } catch {
        // not this kind
      }
    }
  }
  throw new Error(`web3d: the canvas must be an HTMLCanvasElement or OffscreenCanvas, not ${describe(canvas)}`);
}

const WEBGL2 = globalThis.WebGL2RenderingContext?.prototype;
const BUFFER_WIDTH = WEBGL2 && getter(WEBGL2, 'drawingBufferWidth');
const BUFFER_HEIGHT = WEBGL2 && getter(WEBGL2, 'drawingBufferHeight');

function checkSize(width, height) {
  for (const v of [width, height]) {
    if (!Number.isInteger(v) || v < 0 || v > MAX_DRAWING_BUFFER) {
      throw new Error(`web3d: a drawing-buffer side must be a whole number from 0 to ${MAX_DRAWING_BUFFER}, not ${describe(v)}`);
    }
  }
}

/**
 * State shared by every copy of this module (a second import under another URL is another copy):
 * which renderer owns each canvas, the antialias setting each canvas's context has, and the renderer
 * serials. It lives on the top window when this realm can reach it, so a window and its same-origin
 * frames share one; a worker or a cross-origin frame has its own.
 */
const SHARED_KEY = Symbol.for('loadngo-web3d');
const HOME = (() => {
  try {
    const top = globalThis.top;
    if (top && top !== globalThis) {
      void top[SHARED_KEY]; // throws for a cross-origin top
      return top;
    }
  } catch {
    // a cross-origin top: this realm keeps its own
  }
  return globalThis;
})();
const SHARED_STATE = (HOME[SHARED_KEY] ??= {});
SHARED_STATE.owners ??= new WeakMap();
SHARED_STATE.contexts ??= new WeakMap();
SHARED_STATE.controllers ??= new WeakMap();
// Each home (a top window with its same-origin frames, a worker, a cross-origin frame) starts at a
// random serial, so two homes collide only by chance; within one, serials never repeat.
SHARED_STATE.serials ??= { next: 1 + (crypto.getRandomValues(new Uint32Array(1))[0] % (SERIALS - 1)), made: 0 };
const OWNERS = SHARED_STATE.owners;
const CONTEXTS = SHARED_STATE.contexts;
const PENDING = { lost: false };

/**
 * The next renderer serial, from 1 to 2^20 - 1, going round from a random start. Serials are never
 * reused in one home (a window and its same-origin frames, or a worker, or a cross-origin frame), so
 * no two of its renderers, live or disposed, can issue the same handle. A home may make 2^20 - 1
 * renderers.
 */
function takeSerial() {
  const pool = SHARED_STATE.serials;
  if (pool.made >= SERIALS - 1) throw new Error(`web3d: this page has made ${SERIALS - 1} renderers, the most it may`);
  const serial = pool.next;
  pool.next = (serial % (SERIALS - 1)) + 1;
  pool.made++;
  return serial;
}


// Compiled modules, by URL. A failed compile is forgotten, so a later create tries again.
const COMPILED = new Map();
const DEFAULT_WASM = new URL('./loadngo_web3d.wasm', import.meta.url).href;

/**
 * Compiles the wasm module. `source` is a URL (default: next to this file), a Response, bytes, or
 * an already compiled WebAssembly.Module. A URL's module is cached; a failure is not, so the next
 * call fetches again.
 */
export function compile(source) {
  if (isModule(source)) return Promise.resolve(source);
  const url = source == null ? DEFAULT_WASM : typeof source === 'string' ? source : urlHref(source);
  if (url !== undefined) {
    const base = globalThis.document?.baseURI ?? globalThis.location?.href;
    const href = new URL(url, base).href;
    let promise = COMPILED.get(href);
    if (!promise) {
      promise = compileFrom(href);
      COMPILED.set(href, promise);
      promise.catch(() => {
        if (COMPILED.get(href) === promise) COMPILED.delete(href);
      });
    }
    return promise;
  }
  if (isResponse(source) || ArrayBuffer.isView(source) || isArrayBuffer(source)) return compileFrom(asBytes(source));
  throw new Error(`web3d: the wasm option must be a URL (a string or URL), a Response, bytes or a WebAssembly.Module, not ${describe(source)}`);
}

// A DataView's bytes as a Uint8Array, which WebAssembly.compile takes and a DataView it does not.
const DV = DataView.prototype;
const DV_BUFFER = getter(DV, 'buffer');
const DV_OFFSET = getter(DV, 'byteOffset');
const DV_LENGTH = getter(DV, 'byteLength');
function asBytes(source) {
  if (!ArrayBuffer.isView(source) || typedKind(source) !== undefined) return source;
  try {
    return new Uint8Array(DV_BUFFER.call(source), DV_OFFSET.call(source), DV_LENGTH.call(source));
  } catch {
    throw new Error("web3d: the wasm option's DataView has no bytes to compile (its buffer is detached)");
  }
}

// Brand checks through built-ins, which also hold for objects from another realm (an iframe's).
const URL_HREF = getter(URL.prototype, 'href');
const RESPONSE_OK = globalThis.Response && getter(Response.prototype, 'ok');
const AB_BYTES = getter(ArrayBuffer.prototype, 'byteLength');
const branded = (g) => (v) => {
  if (typeof v !== 'object' || v === null || !g) return false;
  try {
    g.call(v);
    return true;
  } catch {
    return false;
  }
};
const isResponse = branded(RESPONSE_OK);
const isArrayBuffer = branded(AB_BYTES);
function isModule(v) {
  if (typeof v !== 'object' || v === null) return false;
  try {
    WebAssembly.Module.exports(v);
    return true;
  } catch {
    return false;
  }
}
function urlHref(v) {
  if (typeof v !== 'object' || v === null) return undefined;
  try {
    return URL_HREF.call(v);
  } catch {
    return undefined;
  }
}

async function compileFrom(source) {
  if (isArrayBuffer(source) || ArrayBuffer.isView(source)) return WebAssembly.compile(source);
  const response = isResponse(source) ? source : await fetch(source);
  if (!response.ok) throw new Error(`web3d: fetching the wasm failed: ${response.status} ${response.url}`);
  const type = response.headers.get('Content-Type') ?? '';
  if (WebAssembly.compileStreaming && type.startsWith('application/wasm')) {
    return WebAssembly.compileStreaming(response);
  }
  return WebAssembly.compile(await response.arrayBuffer());
}

/** Reads every option once, and checks it, before anything is created. */
function readOptions(canvas, options) {
  const kind = canvasKind(canvas);
  const o = options ?? {};
  if (typeof o !== 'object') throw new Error(`web3d: options must be an object, not ${describe(o)}`);
  const width = o.width ?? kind.width.call(canvas);
  const height = o.height ?? kind.height.call(canvas);
  const antialias = o.antialias ?? true;
  const cpuTiming = o.cpuTiming ?? false;
  const wasm = o.wasm;
  const onContextLost = o.onContextLost;
  const onContextRestored = o.onContextRestored;
  checkSize(width, height);
  for (const [name, f] of [
    ['onContextLost', onContextLost],
    ['onContextRestored', onContextRestored],
  ]) {
    if (f != null && typeof f !== 'function') throw new Error(`web3d: ${name} must be a function, not ${describe(f)}`);
  }
  return { kind, width, height, antialias: Boolean(antialias), cpuTiming: Boolean(cpuTiming), wasm, onContextLost, onContextRestored };
}

/**
 * Creates a renderer on a canvas. Each renderer gets its own module instance (the compiled
 * module is shared) and its own serial, so renderers are independent, refuse each other's handles,
 * and never issue the same handle.
 *
 * options: width, height (drawing-buffer pixels, whole numbers from 0 to 32768; default the
 * canvas's), antialias (default true, as three's WebGLRenderer is usually created), cpuTiming
 * (default false; see setCpuTiming), wasm (see compile), onContextLost and onContextRestored (see
 * controllerFor for when each is called).
 *
 * A canvas keeps the WebGL context it first had, with that context's settings, whoever made it: a
 * renderer must ask for the antialias the context has, or it is refused, as is a context without a
 * depth buffer. A context that suits is taken with WebGL 2's default state put back (what the page
 * left bound or switched on is unbound or switched off; its objects are kept), and from then on the
 * renderer owns that state. Everything is checked before the context is made, and a create that
 * fails afterwards removes what it set up and puts the canvas's size back.
 */
export async function createRenderer(canvas, options) {
  const o = readOptions(canvas, options);
  const owner = OWNERS.get(canvas);
  if (owner && !owner.lost) throw new Error(SHARED);
  const first = CONTEXTS.get(canvas);
  if (first !== undefined && first !== o.antialias) throw new Error(contextMismatch(first));
  // Claimed before the first await, so two calls in a row cannot both pass the check.
  OWNERS.set(canvas, PENDING);
  try {
    const renderer = await build(canvas, o);
    OWNERS.set(canvas, renderer);
    return renderer;
  } catch (error) {
    if (owner) OWNERS.set(canvas, owner);
    else OWNERS.delete(canvas);
    throw error;
  }
}

async function build(canvas, o) {
  const module = await compile(o.wasm);
  const attributes = {
    alpha: false,
    antialias: o.antialias,
    depth: true,
    stencil: false,
    premultipliedAlpha: true,
    preserveDrawingBuffer: false,
    powerPreference: 'high-performance',
  };
  const gl = WEBGL2 ? o.kind.getContext.call(canvas, 'webgl2', attributes) : null;
  if (!gl) throw new Error('web3d: WebGL2 is not available here, or this canvas already has another kind of context');
  // From the first use of a canvas on, its loss controller cancels every lost event, so the context
  // can be restored, and calls the hooks; `watched` says whether its listeners were already there,
  // and running, before this create.
  const used = SHARED_STATE.controllers.has(canvas);
  const { controller, watched } = controllerFor(canvas);
  const losses = controller.losses;
  // A lost context (between a loss and its restore, or one the browser dropped) reports no
  // attributes, so nothing is read or recorded from it.
  const attrs = gl.isContextLost() ? null : gl.getContextAttributes();
  if (!attrs) {
    waitForRestore(controller, o.onContextRestored);
    throw new Error(watched ? LOST_AT_CREATE : used ? LOST_UNWATCHED : LOST_BEFORE_WEB3D);
  }
  // The context the canvas now has, which the page may have made first with other settings. Every
  // setting that does not suit is named at once.
  const actual = Boolean(attrs.antialias);
  CONTEXTS.set(canvas, actual);
  const unsuited = [];
  if (actual !== o.antialias) unsuited.push(`antialias ${actual} (ask for antialias: ${actual})`);
  if (!attrs.depth) unsuited.push('no depth buffer (web3d needs one)');
  if (unsuited.length) {
    throw new Error(
      `web3d: this canvas's WebGL context has ${unsuited.join(' and ')}; a canvas keeps its first context and that context's settings, so use a new canvas where the context cannot suit`,
    );
  }
  // The shim is finished by any loss from now on, even one restored before it next looks.
  const shim = createGl(gl, () => controller.losses !== losses);
  const size = [o.kind.width.call(canvas), o.kind.height.call(canvas)];
  let x = null;
  let w = 0;
  try {
    if (shim.lost()) throw new Error(LOST_AT_CREATE); // between a loss and its restore, programs cannot compile
    x = (await WebAssembly.instantiate(module, shim.imports)).exports;
    if (shim.lost()) throw new Error(LOST_AT_CREATE); // lost while the module was instantiated
    if (!(x.memory instanceof WebAssembly.Memory) || typeof x.web3d_create !== 'function') {
      throw new Error('web3d: the wasm module is not loadngo-web3d (its exports are missing)');
    }
    shim.attach(x.memory);
    const serial = takeSerial();
    w = x.web3d_create(serial) >>> 0;
    if (w === 0) throw new Error('web3d: the module could not create a renderer');
    o.kind.setWidth.call(canvas, o.width);
    o.kind.setHeight.call(canvas, o.height);
    if (x.web3d_init(w, BUFFER_WIDTH.call(gl), BUFFER_HEIGHT.call(gl)) !== 0) {
      // Programs fail to compile on a context lost meanwhile; say so, not that a shader failed.
      throw new Error(shim.lost() ? LOST_AT_CREATE : `web3d: ${errorText(x, w, shim)}`);
    }
    if (o.cpuTiming) x.web3d_set_cpu_timing(w, 1);
    const renderer = new Renderer(canvas, o.kind, gl, shim, x, w, serial);
    // Only now do the page's hooks belong to the canvas: a loss during the create makes it fail with
    // "lost", and does not call onContextLost.
    controller.current = { renderer, onLost: o.onContextLost, onRestored: o.onContextRestored };
    return renderer;
  } catch (error) {
    // Leave nothing behind: the renderer's GL objects, the canvas's size. A serial taken is spent,
    // never reused. A create that failed because the context was lost still hears of the restore,
    // through this call's onContextRestored; if the restore came while it failed, at once.
    if (w !== 0) x.web3d_destroy(w);
    const lost = shim.lost();
    shim.dispose();
    if (lost && gl.isContextLost()) waitForRestore(controller, o.onContextRestored);
    else if (lost && typeof o.onContextRestored === 'function') setTimeout(() => callHook(o.onContextRestored), 0);
    if (o.kind.width.call(canvas) !== size[0]) o.kind.setWidth.call(canvas, size[0]);
    if (o.kind.height.call(canvas) !== size[1]) o.kind.setHeight.call(canvas, size[1]);
    throw error;
  }
}

function contextMismatch(antialias) {
  return `web3d: this canvas's WebGL context has antialias ${antialias} (ask for antialias: ${antialias}); a canvas keeps its first context and that context's settings, so use a new canvas where the context cannot suit`;
}

/**
 * The loss controller of a canvas, one a canvas, made the first time web3d takes a context from it,
 * and kept for the canvas's life, shared by every copy of this module that can reach the canvas. Its
 * two listeners are the only ones web3d puts on a canvas:
 * - on `webglcontextlost` it cancels the event, so the context can be restored, counts the loss
 *   (which finishes every renderer made before it), and calls the live renderer's `onContextLost`;
 * - on `webglcontextrestored` it calls, once each (one function once, however often it was given),
 *   the `onContextRestored` of the renderer that was live at the loss (whenever the page disposes
 *   it, even in a restored listener of its own that runs first), of every renderer disposed while
 *   lost, and of every create that failed because of the loss. Then no renderer is live until a new
 *   one is created, so a lost renderer left undisposed hears no later loss or restore.
 * Each hook runs in its own `try`; what it throws is reported (`reportError`) and the rest still run.
 *
 * A window's listeners stop running once its document is gone: a same-origin frame removed, or
 * navigated away. So the listeners are made by the top window's copy of this module once one has
 * loaded (it outlives every frame), else by the copy of the latest create. Each create puts them back
 * from that copy when they are another copy's or the window that made them is gone, keeping the loss
 * count, the live renderer and the waiting hooks. A loss that falls while the only listeners belong
 * to a window that is gone (no top-window copy loaded, the frame whose copy made them removed, and no
 * create since) is not cancelled, and that context cannot be restored.
 *
 * Returns the controller and whether its listeners were there and running before this call.
 */
function controllerFor(canvas) {
  let c = SHARED_STATE.controllers.get(canvas);
  if (!c) {
    c = { losses: 0, current: null, waiting: new Set(), listeners: null };
    SHARED_STATE.controllers.set(canvas, c);
  }
  const install = SHARED_STATE.install ?? installListeners;
  const l = c.listeners;
  const watched = l !== null && listening(l);
  if (!watched || l.install !== install) install(canvas, c);
  return { controller: c, watched };
}

/** Whether listeners made in a realm still run: a window's stop once its document is gone. */
function listening(l) {
  try {
    return !l.realm.closed && l.realm.document === l.document;
  } catch {
    return false; // navigated to another origin
  }
}

/** Puts this copy's two listeners on the canvas in place of any it had, keeping the controller's state. */
function installListeners(canvas, c) {
  if (c.listeners) {
    REMOVE_LISTENER.call(canvas, 'webglcontextlost', c.listeners.lost);
    REMOVE_LISTENER.call(canvas, 'webglcontextrestored', c.listeners.restored);
  }
  const lost = (event) => {
    event.preventDefault();
    c.losses++;
    const hook = c.current?.onLost;
    if (typeof hook === 'function') callHook(hook);
  };
  const restored = () => {
    const hooks = new Set(c.waiting);
    c.waiting.clear();
    const hook = c.current?.onRestored;
    if (typeof hook === 'function') hooks.add(hook);
    c.current = null;
    for (const h of hooks) callHook(h);
  };
  ADD_LISTENER.call(canvas, 'webglcontextlost', lost);
  ADD_LISTENER.call(canvas, 'webglcontextrestored', restored);
  // The waiting hooks, in a set of this realm's (the old one may be a removed frame's).
  c.waiting = new Set(c.waiting);
  c.listeners = { lost, restored, install: installListeners, realm: globalThis, document: globalThis.document };
}

const ADD_LISTENER = EventTarget.prototype.addEventListener;
const REMOVE_LISTENER = EventTarget.prototype.removeEventListener;
// The top window's copy (or a worker's, or a cross-origin frame's: whichever copy runs where the
// shared state lives) makes the listeners for every copy that shares that state.
if (HOME === globalThis) SHARED_STATE.install ??= installListeners;

function waitForRestore(controller, hook) {
  if (typeof hook === 'function') controller.waiting.add(hook);
}

/** Calls a page hook; what it throws is reported, not passed on, so one hook cannot stop another. */
function callHook(hook) {
  try {
    hook();
  } catch (error) {
    if (typeof reportError === 'function') reportError(error);
    else
      setTimeout(() => {
        throw error;
      });
  }
}

function errorText(x, w, shim) {
  const ptr = x.web3d_error_ptr(w) >>> 0;
  const len = x.web3d_error_len(w) >>> 0;
  const message = new TextDecoder().decode(new Uint8Array(x.memory.buffer, ptr, len));
  // Shader failures point at the console; carry the compiler's log along too.
  const log = shim.state.lastLog;
  return message.includes('console') && log ? `${message}\n${log}` : message;
}

/** Made only by createRenderer. */
export class Renderer {
  constructor(canvas, kind, gl, shim, exports, w, serial) {
    this.canvas = canvas;
    this._kind = kind;
    this._gl = gl;
    this._shim = shim;
    this._x = exports;
    this._w = w;
    this._serial = serial;
    this._busy = false;
    this._ioPtr = exports.web3d_io(w) >>> 0;
    this._statsPtr = exports.web3d_stats(w) >>> 0;
    this._io = new Float32Array(exports.memory.buffer, this._ioPtr, IO_FLOATS);
    this._stats = new Float64Array(exports.memory.buffer, this._statsPtr, STATS_LEN);
    const caps = exports.web3d_caps(w) >>> 0;
    this.info = { ...shim.info(), maxTextureSize: caps >>> 1, timerQuery: (caps & 1) === 1 };
  }

  /** True once the WebGL context has been lost; frame() draws nothing then, and creates throw. */
  get lost() {
    return this._shim.lost();
  }

  /**
   * The WebGL2 context, for debugging (getError, readPixels) only: the renderer owns its state, which
   * it set to WebGL 2's defaults when it was made, and a change made here is not seen by it.
   */
  get gl() {
    return this._gl;
  }

  /**
   * Every public method starts here: it refuses a disposed renderer, a call made from inside
   * another call's argument reading, and (for creates) a lost context, then marks the renderer
   * busy until `_leave`. Allocates nothing unless it throws.
   */
  _enter(creates) {
    if (!this._w) throw new Error(DISPOSED);
    if (this._busy) throw new Error(REENTRANT);
    if (creates && this._shim.lost()) throw new Error(LOST);
    this._busy = true;
  }

  _leave() {
    this._busy = false;
  }

  _errorText() {
    return errorText(this._x, this._w, this._shim);
  }

  _handle(h) {
    if (h === 0) throw new Error(`web3d: ${this._errorText()}`);
    return h;
  }

  _status(code) {
    if (code !== 0) throw new Error(`web3d: ${this._errorText()}`);
  }

  /** The exchange area; remade only after the module's memory grows. */
  _ioView() {
    if (this._io.buffer !== this._x.memory.buffer) {
      this._io = new Float32Array(this._x.memory.buffer, this._ioPtr, IO_FLOATS);
    }
    return this._io;
  }

  _statsView() {
    if (this._stats.buffer !== this._x.memory.buffer) {
      this._stats = new Float64Array(this._x.memory.buffer, this._statsPtr, STATS_LEN);
    }
    return this._stats;
  }

  /**
   * Copies `n` numbers into the exchange area at `at`. A typed array (by its internal slot) must
   * have a real length of `n` and is copied by the built-in TypedArray set, which reads its storage
   * and so converts nothing. An Array's `length` is read once and must be `n`; each element is read
   * once and must be a number, so no `valueOf` runs. The Rust side refuses non-finite values.
   */
  _put(what, at, v, n) {
    const io = this._ioView();
    const k = typedKind(v);
    if (k !== undefined) {
      const length = TA_LENGTH.call(v);
      if (length !== n || k === 'BigInt64Array' || k === 'BigUint64Array') {
        throw new Error(`web3d: ${what} must hold ${n} numbers, not ${describe(v)} of ${length}`);
      }
      TA_SET.call(io, v, at);
      return;
    }
    if (!isArray(v)) throw new Error(`web3d: ${what} must be an Array or typed array of ${n} numbers, not ${describe(v)}`);
    const length = v.length;
    if (length !== n) throw new Error(`web3d: ${what} must hold ${n} numbers, not ${describe(length)}`);
    for (let i = 0; i < n; i++) {
      const x = v[i];
      if (typeof x !== 'number') throw new Error(`web3d: ${what}[${i}] must be a number, not ${describe(x)}`);
      io[at + i] = x;
    }
  }

  /** Sizes the canvas, then returns the drawing buffer's real size, which the browser may have clamped. */
  _size(width, height) {
    checkSize(width, height);
    this._kind.setWidth.call(this.canvas, width);
    this._kind.setHeight.call(this.canvas, height);
    return [BUFFER_WIDTH.call(this._gl), BUFFER_HEIGHT.call(this._gl)];
  }

  /**
   * Sets the drawing-buffer size in pixels (whole numbers from 0 to 32768): the canvas's width and
   * height, and the viewport, to the drawing buffer the browser actually made.
   */
  resize(width, height) {
    this._enter(false);
    try {
      // A lost renderer no longer owns its canvas: a new one may already draw there.
      if (this._shim.lost()) throw new Error(LOST);
      if (OWNERS.get(this.canvas) !== this) throw new Error('web3d: this renderer no longer owns its canvas');
      const [w, h] = this._size(width, height);
      this._status(this._x.web3d_resize(this._w, w, h));
    } finally {
      this._leave();
    }
  }

  /**
   * Uploads one mesh (a glTF primitive). Typed arrays, as three.js's BufferGeometry holds them:
   * positions and normals Float32Array (x, y, z), uvs Float32Array (u, v), indices Uint32Array,
   * Uint16Array or Uint8Array. Integer (quantised) positions, normals or uvs are refused: dequantise
   * them first. Reserved for skinning and morphing (checked now, drawn in a later phase): joints
   * (Uint8Array or Uint16Array, 4 per vertex), weights (4 per vertex, in 0..1 and summing to 1:
   * Float32Array, or normalised Uint8Array or Uint16Array, which are dequantised here),
   * morphTargets (an array of Float32Array position deltas, x, y, z per vertex). Returns a mesh
   * handle.
   */
  createMesh(data) {
    this._enter(true);
    const x = this._x;
    const staged = [];
    try {
      if (typeof data !== 'object' || data === null) throw new Error(`web3d: createMesh takes an object, not ${describe(data)}`);
      // Each field is read once; everything after works on these locals.
      const positions = floats('positions', data.positions);
      const normals = floats('normals', data.normals);
      const uvs = floats('uvs', data.uvs);
      const indices = integers('indices', data.indices, ['Uint32Array', 'Uint16Array', 'Uint8Array']);
      const joints = integers('joints', data.joints, ['Uint16Array', 'Uint8Array']);
      const weights = weightFloats(data.weights);
      const targets = morphList(data.morphTargets);
      if (positions == null) throw new Error('web3d: a mesh needs positions');
      if (indices == null) throw new Error('web3d: a mesh needs indices');
      // Lengths come from the arrays' internal slots, never from a `length` property.
      const stage = (Kind, array) => {
        if (!array) return [0, 0];
        const length = TA_LENGTH.call(array);
        const bytes = length * Kind.BYTES_PER_ELEMENT;
        const ptr = x.web3d_alloc(bytes) >>> 0;
        if (ptr === 0) throw new Error('web3d: out of memory staging a mesh');
        staged.push(ptr, bytes);
        TA_SET.call(new Kind(x.memory.buffer, ptr, length), array);
        return [ptr, length];
      };
      const p = stage(Float32Array, positions);
      const n = stage(Float32Array, normals);
      const u = stage(Float32Array, uvs);
      const i = stage(Uint32Array, indices);
      const j = stage(Uint16Array, joints);
      const w = stage(Float32Array, weights);
      let m = [0, 0];
      if (targets.length > 0) {
        const each = TA_LENGTH.call(targets[0]);
        const all = new Float32Array(each * targets.length);
        targets.forEach((t, k) => {
          if (TA_LENGTH.call(t) !== each) throw new Error('web3d: morph targets must all be the same length');
          TA_SET.call(all, t, k * each);
        });
        m = stage(Float32Array, all);
      }
      return this._handle(
        x.web3d_mesh_create(this._w, p[0], p[1], n[0], n[1], u[0], u[1], i[0], i[1], j[0], j[1], w[0], w[1], m[0], m[1], targets.length),
      );
    } finally {
      for (let k = 0; k < staged.length; k += 2) x.web3d_free(staged[k], staged[k + 1]);
      this._leave();
    }
  }

  destroyMesh(mesh) {
    this._enter(false);
    try {
      this._status(this._x.web3d_mesh_destroy(this._w, handle('mesh', mesh)));
    } finally {
      this._leave();
    }
  }

  /**
   * Uploads a texture from an image source the browser has decoded: an ImageBitmap (preferred:
   * `createImageBitmap(blob, { premultiplyAlpha: 'none', colorSpaceConversion: 'none' })`, which
   * also decodes WebP and JPEG), a canvas, an OffscreenCanvas, ImageData or a VideoFrame. An <img>
   * or <video> is refused: decode it with createImageBitmap first. options: srgb (default true: colour data), mipmaps (default
   * true), wrapS / wrapT ('repeat', 'clamp' or 'mirror'). Returns a texture handle.
   */
  createTexture(source, options) {
    this._enter(true);
    try {
      // The options first: they are the caller's object, and its getters run here. The source's
      // size is read after them, through built-in getters, so no caller code runs between it and
      // the upload; the shim checks the size again just before uploading all the same.
      const opts = options ?? {};
      const srgb = opts.srgb ?? true;
      const mipmaps = opts.mipmaps ?? true;
      const wrapS = opts.wrapS ?? 'repeat';
      const wrapT = opts.wrapT ?? 'repeat';
      const s = WRAPS.get(wrapS);
      const t = WRAPS.get(wrapT);
      if (s === undefined || t === undefined) {
        throw new Error(`web3d: wrap modes are 'repeat', 'clamp' or 'mirror', not ${describe(s === undefined ? wrapS : wrapT)}`);
      }
      const flags = (srgb ? 1 : 0) | (mipmaps ? 2 : 0) | (s << 2) | (t << 4);
      const [width, height] = imageSize(source);
      this._shim.stage(source, () => imageSize(source));
      try {
        return this._handle(this._x.web3d_texture_create(this._w, width, height, flags));
      } finally {
        this._shim.stage(null);
      }
    } finally {
      this._leave();
    }
  }

  destroyTexture(texture) {
    this._enter(false);
    try {
      this._status(this._x.web3d_texture_destroy(this._w, handle('texture', texture)));
    } finally {
      this._leave();
    }
  }

  /**
   * Creates a material. baseColor: linear [r, g, b] or [r, g, b, a] (default white); texture: a
   * handle (multiplies baseColor); alphaMode: 'opaque' | 'mask' | 'blend'; alphaCutoff (mask,
   * default 0.5); doubleSided; roughness (default 1) and metallic (default 0), as three.js's
   * MeshStandardMaterial. Returns a material handle.
   */
  createMaterial(options) {
    this._enter(true);
    try {
      const o = options ?? {};
      if (typeof o !== 'object') throw new Error(`web3d: createMaterial takes an object, not ${describe(o)}`);
      const baseColor = o.baseColor ?? WHITE;
      const texture = o.texture ?? 0;
      const alphaMode = o.alphaMode ?? 'opaque';
      const alphaCutoff = num('alphaCutoff', o.alphaCutoff ?? 0.5);
      const doubleSided = Boolean(o.doubleSided);
      const roughness = num('roughness', o.roughness ?? 1);
      const metallic = num('metallic', o.metallic ?? 0);
      const mode = ALPHA_MODES.get(alphaMode);
      if (mode === undefined) throw new Error(`web3d: unknown alpha mode ${describe(alphaMode)}`);
      const tex = texture === 0 ? 0 : handle('texture', texture);
      // The colour: 3 or 4 numbers, each read once into locals before the exchange area is written.
      const isTyped = typedKind(baseColor) !== undefined;
      if (!isTyped && !isArray(baseColor)) throw new Error(`web3d: baseColor must be 3 or 4 numbers, not ${describe(baseColor)}`);
      const count = isTyped ? TA_LENGTH.call(baseColor) : baseColor.length;
      if (count !== 3 && count !== 4) throw new Error(`web3d: baseColor must be 3 or 4 numbers, not ${describe(count)}`);
      const r = num('baseColor[0]', baseColor[0]);
      const g = num('baseColor[1]', baseColor[1]);
      const b = num('baseColor[2]', baseColor[2]);
      const a = count === 4 ? num('baseColor[3]', baseColor[3]) : 1;
      const io = this._ioView();
      io[0] = r;
      io[1] = g;
      io[2] = b;
      io[3] = a;
      io[4] = alphaCutoff;
      io[5] = roughness;
      io[6] = metallic;
      return this._handle(this._x.web3d_material_create(this._w, tex, mode, doubleSided ? 1 : 0));
    } finally {
      this._leave();
    }
  }

  destroyMaterial(material) {
    this._enter(false);
    try {
      this._status(this._x.web3d_material_destroy(this._w, handle('material', material)));
    } finally {
      this._leave();
    }
  }

  /** Places a mesh with a material; matrix: 16 numbers, column-major (default identity). */
  createObject(mesh, material, matrix = IDENTITY) {
    this._enter(true);
    try {
      handle('mesh', mesh);
      handle('material', material);
      this._put('matrix', 0, matrix, 16);
      return this._handle(this._x.web3d_object_create(this._w, mesh, material));
    } finally {
      this._leave();
    }
  }

  /** Moves an object; matrix: 16 numbers, column-major (three.js's matrixWorld.elements). */
  setTransform(object, matrix) {
    this._enter(false);
    try {
      handle('object', object);
      this._put('matrix', 0, matrix, 16);
      this._status(this._x.web3d_object_set_transform(this._w, object));
    } finally {
      this._leave();
    }
  }

  setVisible(object, visible) {
    this._enter(false);
    try {
      this._status(this._x.web3d_object_set_visible(this._w, handle('object', object), visible ? 1 : 0));
    } finally {
      this._leave();
    }
  }

  destroyObject(object) {
    this._enter(false);
    try {
      this._status(this._x.web3d_object_destroy(this._w, handle('object', object)));
    } finally {
      this._leave();
    }
  }

  /** The camera as three.js holds it: camera.matrixWorldInverse.elements, camera.projectionMatrix.elements. */
  setCamera(view, projection) {
    this._enter(false);
    try {
      this._put('view', 0, view, 16);
      this._put('projection', 16, projection, 16);
      this._status(this._x.web3d_set_camera(this._w));
    } finally {
      this._leave();
    }
  }

  /**
   * A perspective camera at `eye` looking at `target` (3 numbers each; pass reused arrays on the
   * per-frame path). The projection follows the drawing buffer's aspect.
   */
  setCameraLookAt(eye, target, fovY, near, far, up = UP) {
    this._enter(false);
    try {
      num('fovY', fovY);
      num('near', near);
      num('far', far);
      this._put('eye', 0, eye, 3);
      this._put('target', 3, target, 3);
      this._put('up', 6, up, 3);
      const io = this._io;
      io[9] = fovY;
      io[10] = near;
      io[11] = far;
      this._status(this._x.web3d_set_camera_look_at(this._w));
    } finally {
      this._leave();
    }
  }

  /**
   * The key light: (x, y, z) points from the scene towards the light (three.js: light.position minus
   * light.target.position); colour linear; intensity as three.js r180's DirectionalLight.intensity.
   */
  setKeyLight(x, y, z, r, g, b, intensity = 1) {
    this._enter(false);
    try {
      num('x', x);
      num('y', y);
      num('z', z);
      num('r', r);
      num('g', g);
      num('b', b);
      num('intensity', intensity);
      this._status(this._x.web3d_set_key_light(this._w, x, y, z, r, g, b, intensity));
    } finally {
      this._leave();
    }
  }

  /**
   * Hemisphere ambient: sky colour from above, ground colour from below (linear), intensity as
   * three.js r180's HemisphereLight.intensity.
   */
  setAmbient(skyR, skyG, skyB, groundR, groundG, groundB, intensity = 1) {
    this._enter(false);
    try {
      num('skyR', skyR);
      num('skyG', skyG);
      num('skyB', skyB);
      num('groundR', groundR);
      num('groundG', groundG);
      num('groundB', groundB);
      num('intensity', intensity);
      this._status(this._x.web3d_set_ambient(this._w, skyR, skyG, skyB, groundR, groundG, groundB, intensity));
    } finally {
      this._leave();
    }
  }

  /** The background colour, linear; non-finite values are refused. */
  setClearColor(r, g, b) {
    this._enter(false);
    try {
      num('r', r);
      num('g', g);
      num('b', b);
      this._status(this._x.web3d_set_clear_color(this._w, r, g, b));
    } finally {
      this._leave();
    }
  }

  /**
   * Draws a frame; returns false (and draws nothing) once the context is lost (the flag the
   * context-lost event sets). With CPU timing on, wasm reads the clock twice around its work;
   * each reading is a number V8 boxes (20.8 sampled bytes a frame for the two, measured in
   * evidence/phase1/cpu-timing-allocation/), which is why timing is off unless asked for.
   */
  frame() {
    this._enter(false);
    try {
      // From the moment the context is lost, before its event arrives too.
      if (this._shim.lost()) return false;
      this._x.web3d_frame(this._w);
      return true;
    } finally {
      this._leave();
    }
  }

  /**
   * Times each frame on the CPU, for a perf readout. Off by default: its two clock readings a
   * frame are numbers V8 boxes (20.8 sampled bytes a frame), the only allocation drawing otherwise
   * makes.
   */
  setCpuTiming(on) {
    this._enter(false);
    try {
      this._x.web3d_set_cpu_timing(this._w, on ? 1 : 0);
    } finally {
      this._leave();
    }
  }

  /**
   * CPU milliseconds the last frame() took inside wasm (the renderer's work and its WebGL calls),
   * or null when CPU timing is off.
   */
  get cpuMs() {
    const ms = this._w ? this._statsView()[0] : NaN;
    return Number.isNaN(ms) ? null : ms;
  }

  /** The newest GPU time in milliseconds, or null without timer queries or a recent reading. */
  get gpuMs() {
    if (!this._w) return null;
    const ms = this._statsView()[1];
    return Number.isNaN(ms) ? null : ms;
  }

  /**
   * Copies the last frame's numbers into `out` (a Float64Array of at least 8, reused), without
   * allocating: [cpuMs (NaN unless CPU timing is on), gpuMs (NaN if none), frame the GPU time
   * measured, draws, triangles, culled, program binds, material binds].
   */
  readStats(out) {
    this._enter(false);
    try {
      if (typedKind(out) !== 'Float64Array' || TA_LENGTH.call(out) < STATS_LEN) {
        throw new Error(`web3d: readStats takes a Float64Array of at least ${STATS_LEN}, not ${describe(out)}`);
      }
      TA_SET.call(out, this._statsView());
      return out;
    } finally {
      this._leave();
    }
  }

  /**
   * Releases every GPU object and the module instance's renderer. Safe to call twice, and after a
   * context loss (nothing is then sent to the context, which may since have been restored for a new
   * renderer). Every other call throws afterwards, and the canvas is free for a new renderer.
   */
  dispose() {
    if (!this._w) return;
    if (this._busy) throw new Error(REENTRANT);
    this._x.web3d_destroy(this._w);
    this._w = 0;
    this._shim.dispose();
    // If this is the canvas's live renderer, its hooks leave it now. Disposed after a loss, as the
    // LOST message says to, its onContextRestored still comes on the restore, so the page knows when
    // to create the next renderer: the controller clears `current` when it handles a restore, so a
    // finished renderer that is still current has not had its hook yet, even if the context is
    // restored already (a restored listener of the page's that runs before web3d's disposes it).
    const controller = SHARED_STATE.controllers.get(this.canvas);
    if (controller?.current?.renderer === this) {
      if (this._shim.lost()) waitForRestore(controller, controller.current.onRestored);
      controller.current = null;
    }
    if (OWNERS.get(this.canvas) === this) OWNERS.delete(this.canvas);
  }
}

const IDENTITY = new Float32Array([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1]);
const UP = new Float32Array([0, 1, 0]);
const WHITE = new Float32Array([1, 1, 1, 1]);
