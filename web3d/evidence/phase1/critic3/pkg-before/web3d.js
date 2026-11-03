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
// `elements`), handles small positive integers. Nothing on the per-frame path (`setTransform`,
// `setCamera`, `setCameraLookAt`, `setKeyLight`, `frame`, `readStats`) allocates.
//
// After `dispose()` every call throws "web3d: renderer disposed". When the WebGL context is lost,
// `frame()` returns false, creating anything throws, and the renderer is finished: dispose it, then
// create a new one after the context is restored (`onContextRestored`) and upload everything again.
//
// BSD-3-Clause; see the repository's LICENSE.

import { createGl } from './gl.js';

const IO_FLOATS = 64;
const STATS_LEN = 8;
const ALPHA_MODES = new Map([
  ['opaque', 0],
  ['mask', 1],
  ['blend', 2],
]);
/** The largest drawing-buffer side accepted, as the Rust side's MAX_DRAWING_BUFFER. */
const MAX_DRAWING_BUFFER = 32768;
const DISPOSED = 'web3d: renderer disposed';
const LOST =
  'web3d: the WebGL context is lost; dispose this renderer, then create a new one once the context is restored (onContextRestored) and upload everything again';

const kind = (a) => a?.constructor?.name ?? typeof a;

/** A Float32Array, or null if absent. Integer data (quantised positions, normals) is refused. */
function floats(name, a) {
  if (a == null) return null;
  if (!(a instanceof Float32Array)) {
    throw new Error(`web3d: ${name} must be a Float32Array, not ${kind(a)}; dequantise integer data first`);
  }
  return a;
}

/** Skin weights as floats: glTF stores them as floats, or as normalised bytes or shorts. */
function weightFloats(a) {
  if (a == null) return null;
  if (a instanceof Float32Array) return a;
  if (a instanceof Uint8Array) return Float32Array.from(a, (v) => v / 255);
  if (a instanceof Uint16Array) return Float32Array.from(a, (v) => v / 65535);
  throw new Error(`web3d: weights must be a Float32Array, or a Uint8Array or Uint16Array of normalised values, not ${kind(a)}`);
}

function integers(name, a, kinds) {
  if (a == null) return null;
  if (!kinds.some((K) => a instanceof K)) {
    throw new Error(`web3d: ${name} must be a ${kinds.map((K) => K.name).join(' or ')}, not ${kind(a)}`);
  }
  return a;
}
// Maps, not object literals: a name such as 'toString' must not find Object.prototype.
const WRAPS = new Map([
  ['repeat', 0],
  ['clamp', 1],
  ['mirror', 2],
]);

let compiled = null;

/**
 * Compiles the wasm module once and caches it. `source` is a URL (default: next to this file), a
 * Response, bytes, or an already compiled WebAssembly.Module.
 */
export function compile(source) {
  compiled ??= compileFrom(source ?? new URL('./loadngo_web3d.wasm', import.meta.url));
  return compiled;
}

async function compileFrom(source) {
  if (source instanceof WebAssembly.Module) return source;
  if (source instanceof ArrayBuffer || ArrayBuffer.isView(source)) return WebAssembly.compile(source);
  const response = source instanceof Response ? source : await fetch(source);
  if (!response.ok) throw new Error(`web3d: fetching the wasm failed: ${response.status} ${response.url}`);
  const type = response.headers.get('Content-Type') ?? '';
  if (WebAssembly.compileStreaming && type.startsWith('application/wasm')) {
    return WebAssembly.compileStreaming(response);
  }
  return WebAssembly.compile(await response.arrayBuffer());
}

/**
 * Creates a renderer on a canvas. Each renderer gets its own module instance (the compiled
 * module is shared), so renderers are independent.
 *
 * options: width, height (drawing-buffer pixels; default the canvas's), antialias (default true,
 * as [redacted]), cpuTiming (default false; see setCpuTiming), wasm (see compile), onContextLost,
 * onContextRestored.
 */
export async function createRenderer(canvas, options = {}) {
  const module = await compile(options.wasm);
  const shim = createGl(
    canvas,
    {
      alpha: false,
      antialias: options.antialias ?? true,
      depth: true,
      stencil: false,
      premultipliedAlpha: true,
      preserveDrawingBuffer: false,
      powerPreference: 'high-performance',
    },
    { onContextLost: options.onContextLost, onContextRestored: options.onContextRestored },
  );
  if (shim.lost()) {
    // Between a loss and its restore: programs could not compile, and the error would mislead.
    shim.dispose();
    throw new Error(LOST);
  }
  const instance = await WebAssembly.instantiate(module, shim.imports);
  return new Renderer(canvas, shim, instance.exports, options);
}

export class Renderer {
  constructor(canvas, shim, exports, options) {
    this.canvas = canvas;
    this._shim = shim;
    this._x = exports;
    this._decoder = new TextDecoder();
    shim.attach(exports.memory);
    this._w = exports.web3d_create() >>> 0;
    const [width, height] = this._size(options.width ?? canvas.width, options.height ?? canvas.height);
    if (exports.web3d_init(this._w, width, height) !== 0) {
      const message = this._errorText();
      exports.web3d_destroy(this._w);
      this._w = 0;
      shim.dispose();
      throw new Error(`web3d: ${message}`);
    }
    this._ioPtr = exports.web3d_io(this._w) >>> 0;
    this._statsPtr = exports.web3d_stats(this._w) >>> 0;
    this._io = new Float32Array(exports.memory.buffer, this._ioPtr, IO_FLOATS);
    this._stats = new Float64Array(exports.memory.buffer, this._statsPtr, STATS_LEN);
    if (options.cpuTiming) exports.web3d_set_cpu_timing(this._w, 1);
    const caps = exports.web3d_caps(this._w) >>> 0;
    this.info = { ...shim.info(), maxTextureSize: caps >>> 1, timerQuery: (caps & 1) === 1 };
  }

  /** True once the WebGL context has been lost; frame() draws nothing then, and creates throw. */
  get lost() {
    return this._shim.lost();
  }

  _live() {
    if (!this._w) throw new Error(DISPOSED);
  }

  _creatable() {
    if (!this._w) throw new Error(DISPOSED);
    if (this._shim.lost()) throw new Error(LOST);
  }

  /** The WebGL2 context, for debugging (getError) only; the renderer assumes it owns the state. */
  get gl() {
    return this._shim.gl;
  }

  _errorText() {
    const x = this._x;
    const ptr = x.web3d_error_ptr(this._w) >>> 0;
    const len = x.web3d_error_len(this._w) >>> 0;
    const message = this._decoder.decode(new Uint8Array(x.memory.buffer, ptr, len));
    // Shader failures point at the console; carry the compiler's log along too.
    const log = this._shim.state.lastLog;
    return message.includes('console') && log ? `${message}\n${log}` : message;
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

  // Copies go through TypedArray.prototype.set where they can: an element-by-element copy of a
  // fractional number boxes it while V8 still runs the code unoptimised.
  _putMatrix(at, m) {
    const io = this._ioView();
    if (m.length === 16) io.set(m, at);
    else for (let k = 0; k < 16; k++) io[at + k] = m[k];
  }

  _putVector(at, v) {
    const io = this._ioView();
    if (v.length === 3) io.set(v, at);
    else for (let k = 0; k < 3; k++) io[at + k] = v[k];
  }

  /**
   * Sizes the canvas, then returns the drawing buffer's real size, which the browser may have
   * clamped. A size that is not a whole number from 0 to 32768 is refused before it can reach
   * WebGL (as a 32-bit integer it would turn negative past 2^31).
   */
  _size(width, height) {
    for (const v of [width, height]) {
      if (!Number.isInteger(v) || v < 0 || v > MAX_DRAWING_BUFFER) {
        throw new Error(`web3d: a drawing-buffer side must be a whole number from 0 to ${MAX_DRAWING_BUFFER}, not ${v}`);
      }
    }
    this.canvas.width = width;
    this.canvas.height = height;
    const gl = this._shim.gl;
    return [gl.drawingBufferWidth, gl.drawingBufferHeight];
  }

  /**
   * Sets the drawing-buffer size in pixels: the canvas's width and height, and the viewport, to
   * the drawing buffer the browser actually made.
   */
  resize(width, height) {
    this._live();
    const [w, h] = this._size(width, height);
    this._status(this._x.web3d_resize(this._w, w, h));
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
  createMesh({ positions, normals, uvs, indices, joints, weights, morphTargets }) {
    this._creatable();
    if (positions == null) throw new Error('web3d: a mesh needs positions');
    if (indices == null) throw new Error('web3d: a mesh needs indices');
    positions = floats('positions', positions);
    normals = floats('normals', normals);
    uvs = floats('uvs', uvs);
    indices = integers('indices', indices, [Uint32Array, Uint16Array, Uint8Array]);
    joints = integers('joints', joints, [Uint16Array, Uint8Array]);
    weights = weightFloats(weights);
    (morphTargets ?? []).forEach((t, k) => floats(`morphTargets[${k}]`, t));
    const x = this._x;
    const staged = [];
    const stage = (Kind, array) => {
      if (!array) return [0, 0];
      const bytes = array.length * Kind.BYTES_PER_ELEMENT;
      const ptr = x.web3d_alloc(bytes) >>> 0;
      if (ptr === 0) throw new Error('web3d: out of memory staging a mesh');
      staged.push(ptr, bytes);
      new Kind(x.memory.buffer, ptr, array.length).set(array);
      return [ptr, array.length];
    };
    try {
      const p = stage(Float32Array, positions);
      const n = stage(Float32Array, normals);
      const u = stage(Float32Array, uvs);
      const i = stage(Uint32Array, indices);
      const j = stage(Uint16Array, joints);
      const w = stage(Float32Array, weights);
      let m = [0, 0];
      const targets = morphTargets?.length ?? 0;
      if (targets > 0) {
        const each = morphTargets[0].length;
        const all = new Float32Array(each * targets);
        morphTargets.forEach((t, k) => {
          if (t.length !== each) throw new Error('web3d: morph targets must all be the same length');
          all.set(t, k * each);
        });
        m = stage(Float32Array, all);
      }
      return this._handle(
        x.web3d_mesh_create(this._w, p[0], p[1], n[0], n[1], u[0], u[1], i[0], i[1], j[0], j[1], w[0], w[1], m[0], m[1], targets),
      );
    } finally {
      for (let k = 0; k < staged.length; k += 2) x.web3d_free(staged[k], staged[k + 1]);
    }
  }

  destroyMesh(mesh) {
    this._live();
    this._status(this._x.web3d_mesh_destroy(this._w, mesh));
  }

  /**
   * Uploads a texture from an image source the browser has decoded: an ImageBitmap (preferred:
   * `createImageBitmap(blob, { premultiplyAlpha: 'none', colorSpaceConversion: 'none' })`), an
   * HTMLImageElement, a canvas or an OffscreenCanvas. WebP and JPEG work wherever the browser
   * decodes them. options: srgb (default true: colour data), mipmaps (default true), wrapS / wrapT
   * ('repeat', 'clamp' or 'mirror'). Returns a texture handle.
   */
  createTexture(source, { srgb = true, mipmaps = true, wrapS = 'repeat', wrapT = 'repeat' } = {}) {
    this._creatable();
    const width = source.naturalWidth || source.videoWidth || source.displayWidth || source.width;
    const height = source.naturalHeight || source.videoHeight || source.displayHeight || source.height;
    const s = WRAPS.get(wrapS);
    const t = WRAPS.get(wrapT);
    if (s === undefined || t === undefined) {
      throw new Error(`web3d: wrap modes are 'repeat', 'clamp' or 'mirror', not ${s === undefined ? wrapS : wrapT}`);
    }
    const flags = (srgb ? 1 : 0) | (mipmaps ? 2 : 0) | (s << 2) | (t << 4);
    this._shim.stage(source);
    try {
      return this._handle(this._x.web3d_texture_create(this._w, width, height, flags));
    } finally {
      this._shim.stage(null);
    }
  }

  destroyTexture(texture) {
    this._live();
    this._status(this._x.web3d_texture_destroy(this._w, texture));
  }

  /**
   * Creates a material. baseColor: linear [r, g, b, a] (default white); texture: a handle
   * (multiplies baseColor); alphaMode: 'opaque' | 'mask' | 'blend'; alphaCutoff (mask, default
   * 0.5); doubleSided; roughness (default 1) and metallic (default 0), as three.js's
   * MeshStandardMaterial. Returns a material handle.
   */
  createMaterial({ baseColor = [1, 1, 1, 1], texture = 0, alphaMode = 'opaque', alphaCutoff = 0.5, doubleSided = false, roughness = 1, metallic = 0 } = {}) {
    this._creatable();
    const mode = ALPHA_MODES.get(alphaMode);
    if (mode === undefined) throw new Error(`web3d: unknown alpha mode ${alphaMode}`);
    const io = this._ioView();
    io[0] = baseColor[0];
    io[1] = baseColor[1];
    io[2] = baseColor[2];
    io[3] = baseColor[3] ?? 1;
    io[4] = alphaCutoff;
    io[5] = roughness;
    io[6] = metallic;
    return this._handle(this._x.web3d_material_create(this._w, texture, mode, doubleSided ? 1 : 0));
  }

  destroyMaterial(material) {
    this._live();
    this._status(this._x.web3d_material_destroy(this._w, material));
  }

  /** Places a mesh with a material; matrix: 16 numbers, column-major (default identity). */
  createObject(mesh, material, matrix = IDENTITY) {
    this._creatable();
    this._putMatrix(0, matrix);
    return this._handle(this._x.web3d_object_create(this._w, mesh, material));
  }

  /** Moves an object; matrix: 16 numbers, column-major (three.js's matrixWorld.elements). */
  setTransform(object, matrix) {
    this._live();
    this._putMatrix(0, matrix);
    this._status(this._x.web3d_object_set_transform(this._w, object));
  }

  setVisible(object, visible) {
    this._live();
    this._status(this._x.web3d_object_set_visible(this._w, object, visible ? 1 : 0));
  }

  destroyObject(object) {
    this._live();
    this._status(this._x.web3d_object_destroy(this._w, object));
  }

  /** The camera as three.js holds it: camera.matrixWorldInverse.elements, camera.projectionMatrix.elements. */
  setCamera(view, projection) {
    this._live();
    this._putMatrix(0, view);
    this._putMatrix(16, projection);
    this._status(this._x.web3d_set_camera(this._w));
  }

  /**
   * A perspective camera at `eye` looking at `target` (3 numbers each; pass reused arrays on the
   * per-frame path). The projection follows the drawing buffer's aspect.
   */
  setCameraLookAt(eye, target, fovY, near, far, up = UP) {
    this._live();
    this._putVector(0, eye);
    this._putVector(3, target);
    this._putVector(6, up);
    const io = this._io;
    io[9] = fovY;
    io[10] = near;
    io[11] = far;
    this._status(this._x.web3d_set_camera_look_at(this._w));
  }

  /**
   * The key light: (x, y, z) points from the scene towards the light (three.js: light.position minus
   * light.target.position); colour linear; intensity as three.js r180's DirectionalLight.intensity.
   */
  setKeyLight(x, y, z, r, g, b, intensity = 1) {
    this._live();
    this._status(this._x.web3d_set_key_light(this._w, x, y, z, r, g, b, intensity));
  }

  /**
   * Hemisphere ambient: sky colour from above, ground colour from below (linear), intensity as
   * three.js r180's HemisphereLight.intensity.
   */
  setAmbient(skyR, skyG, skyB, groundR, groundG, groundB, intensity = 1) {
    this._live();
    this._status(this._x.web3d_set_ambient(this._w, skyR, skyG, skyB, groundR, groundG, groundB, intensity));
  }

  /** The background colour, linear. */
  setClearColor(r, g, b) {
    this._live();
    this._x.web3d_set_clear_color(this._w, r, g, b);
  }

  /**
   * Draws a frame; returns false (and draws nothing) once the context is lost (the flag the
   * context-lost event sets). With CPU timing on, wasm reads the clock twice around its work;
   * each reading is a number V8 boxes (about 21 bytes a frame in all), which is why timing is off
   * unless asked for (setCpuTiming).
   */
  frame() {
    if (!this._w) throw new Error(DISPOSED);
    if (this._shim.state.lost) return false;
    this._x.web3d_frame(this._w);
    return true;
  }

  /**
   * Times each frame on the CPU, for a perf readout. Off by default: each of its two clock readings
   * a frame is a number V8 boxes (24 bytes a frame), the only allocation drawing otherwise makes.
   */
  setCpuTiming(on) {
    this._live();
    this._x.web3d_set_cpu_timing(this._w, on ? 1 : 0);
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
   * Copies the last frame's numbers into `out` (a Float64Array of 8, reused), without
   * allocating: [cpuMs (NaN unless CPU timing is on), gpuMs (NaN if none), frame the GPU time
   * measured, draws, triangles, culled, program binds, material binds].
   */
  readStats(out) {
    this._live();
    out.set(this._statsView());
    return out;
  }

  /**
   * Releases every GPU object and the module instance's renderer. Safe to call twice, and after a
   * context loss (nothing is then sent to the context, which may since have been restored for a new
   * renderer). Every other call throws afterwards.
   */
  dispose() {
    if (!this._w) return;
    this._x.web3d_destroy(this._w);
    this._w = 0;
    this._shim.dispose();
  }
}

const IDENTITY = new Float32Array([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1]);
const UP = new Float32Array([0, 1, 0]);
