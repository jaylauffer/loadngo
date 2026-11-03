// Types for loadngo-web3d's JS API (web3d.js). BSD-3-Clause.

/** Numbers as the wrapper takes them: an Array of numbers, or a typed array (not a BigInt one). */
export type Numbers =
  | readonly number[]
  | Float32Array
  | Float64Array
  | Int8Array
  | Uint8Array
  | Uint8ClampedArray
  | Int16Array
  | Uint16Array
  | Int32Array
  | Uint32Array;
/** 16 numbers, column-major: three.js's `Matrix4.elements`. Exactly 16. */
export type Matrix4 = Numbers;
/** Exactly 3 numbers. */
export type Vector3 = Numbers;

declare const brand: unique symbol;
/**
 * Handles are whole numbers below 2^50 that name one kind of thing in one renderer, and no handle is
 * issued twice in a window and its same-origin frames (a worker or a cross-origin frame counts its
 * own, from a random start): the Rust side refuses a handle of another kind or from another
 * renderer, live or disposed. The brands let TypeScript catch a swapped argument.
 */
export type MeshHandle = number & { readonly [brand]: 'mesh' };
export type TextureHandle = number & { readonly [brand]: 'texture' };
export type MaterialHandle = number & { readonly [brand]: 'material' };
export type ObjectHandle = number & { readonly [brand]: 'object' };

export interface RendererOptions {
  /** Drawing-buffer width in pixels (default: the canvas's). */
  width?: number;
  /** Drawing-buffer height in pixels (default: the canvas's). */
  height?: number;
  /** Multisampling (default true). */
  antialias?: boolean;
  /** Time each frame on the CPU (default false): costs two boxed numbers (20.8 sampled bytes) a frame. */
  cpuTiming?: boolean;
  /** Where the wasm is: a URL, a Response, bytes (a DataView too) or a compiled module (default: beside web3d.js). */
  wasm?: string | URL | Response | BufferSource | WebAssembly.Module;
  /**
   * Called when the canvas's context is lost while this renderer is the canvas's live one; not for a
   * create that fails because of a loss, and never again once the context has been restored.
   */
  onContextLost?: () => void;
  /**
   * Called once when the context is restored after a loss of this renderer's: whether it was still
   * live (disposed or not, at any time after the loss, even in a restored listener of the page's
   * that runs before web3d's), disposed while lost, or a create that failed because of the loss.
   * Create the next renderer then. web3d keeps one loss controller a canvas, which cancels every
   * lost event so the context can be restored, and runs each hook once in its own try. Its
   * listeners are the top window's copy of web3d's when one has loaded, else the latest create's
   * copy's; a create puts them back if the frame whose copy made them is gone. A loss while the only
   * listeners belong to a removed frame, before the next create, cannot be restored.
   */
  onContextRestored?: () => void;
}

export interface RendererInfo {
  renderer: string;
  vendor: string;
  version: string;
  maxTextureSize: number;
  timerQuery: boolean;
  antialias: boolean;
  samples: number;
}

/** Integer (quantised) positions, normals or uvs are refused: dequantise them first. */
export interface MeshData {
  positions: Float32Array;
  normals?: Float32Array;
  uvs?: Float32Array;
  indices: Uint32Array | Uint16Array | Uint8Array;
  /** Reserved for skinning (a later phase): four joint indices per vertex. */
  joints?: Uint16Array | Uint8Array;
  /**
   * Reserved for skinning: four weights per vertex, each in 0..1, summing to 1 (within 0.02).
   * Normalised Uint8Array or Uint16Array weights (glTF's other storage) are dequantised.
   */
  weights?: Float32Array | Uint8Array | Uint16Array;
  /** Reserved for morph targets: position deltas, x, y, z per vertex, one array per target (at most 256). */
  morphTargets?: Float32Array[];
}

/**
 * Sources sized through built-in getters (a VideoFrame by its visible rectangle, turned by its
 * rotation). An <img> or <video> is refused (its reported size can differ from the pixels WebGL
 * uploads): decode it first with createImageBitmap. A canvas with a 'bitmaprenderer' context is
 * refused too (its bitmap need not be its size): pass the ImageBitmap itself.
 *
 * A canvas must be drawn on (have its context) before it is passed. Telling a 'bitmaprenderer'
 * canvas apart asks the canvas for a '2d' context, which changes nothing on a canvas that has a
 * context; one with none gets a blank '2d' context for good, so it can never have a WebGL, WebGPU
 * or 'bitmaprenderer' context, an element can no longer be transferred to an OffscreenCanvas, and a
 * later getContext('2d', options) ignores the options. No browser call tells whether a canvas has a
 * context without making one.
 */
export type TextureSource = ImageBitmap | HTMLCanvasElement | OffscreenCanvas | ImageData | VideoFrame;

export interface TextureOptions {
  /** Colour data, decoded from sRGB when sampled (default true). */
  srgb?: boolean;
  mipmaps?: boolean;
  wrapS?: 'repeat' | 'clamp' | 'mirror';
  wrapT?: 'repeat' | 'clamp' | 'mirror';
}

export interface MaterialOptions {
  /** Linear RGBA (default white). */
  /** 3 or 4 numbers. */
  baseColor?: Numbers;
  texture?: TextureHandle;
  alphaMode?: 'opaque' | 'mask' | 'blend';
  alphaCutoff?: number;
  doubleSided?: boolean;
  roughness?: number;
  metallic?: number;
}

export function compile(source?: RendererOptions['wasm']): Promise<WebAssembly.Module>;
/**
 * One renderer per canvas while it is live. A canvas keeps its first WebGL context and that
 * context's antialias setting; a later renderer on it must ask for the same, and the context needs a
 * depth buffer. The renderer puts WebGL 2's default state back on the context, so what a page left
 * bound or switched on does not reach it, and owns that state from then on.
 */
export function createRenderer(canvas: HTMLCanvasElement | OffscreenCanvas, options?: RendererOptions): Promise<Renderer>;

/**
 * After dispose() every method throws "web3d: renderer disposed". Once the WebGL context is lost,
 * frame() returns false and every create* and resize throws; dispose the renderer, then create a new
 * one after onContextRestored (which is still called when the renderer was disposed while lost) and
 * upload everything again.
 */
export class Renderer {
  readonly canvas: HTMLCanvasElement | OffscreenCanvas;
  readonly info: RendererInfo;
  /** True once the context has been lost; it stays true after a restore (create a new renderer). */
  readonly lost: boolean;
  /** For debugging (getError, readPixels) only: the renderer owns the context's state. */
  readonly gl: WebGL2RenderingContext;
  /** CPU milliseconds of the last frame(), or null when CPU timing is off. */
  readonly cpuMs: number | null;
  /** The newest GPU milliseconds, or null without timer queries or a reading in the last 30 frames. */
  readonly gpuMs: number | null;

  /** Whole numbers from 0 to 32768; the viewport follows the drawing buffer the browser makes. */
  resize(width: number, height: number): void;
  createMesh(data: MeshData): MeshHandle;
  destroyMesh(mesh: MeshHandle): void;
  createTexture(source: TextureSource, options?: TextureOptions): TextureHandle;
  destroyTexture(texture: TextureHandle): void;
  createMaterial(options?: MaterialOptions): MaterialHandle;
  destroyMaterial(material: MaterialHandle): void;
  createObject(mesh: MeshHandle, material: MaterialHandle, matrix?: Matrix4): ObjectHandle;
  setTransform(object: ObjectHandle, matrix: Matrix4): void;
  setVisible(object: ObjectHandle, visible: boolean): void;
  destroyObject(object: ObjectHandle): void;
  setCamera(view: Matrix4, projection: Matrix4): void;
  setCameraLookAt(eye: Vector3, target: Vector3, fovY: number, near: number, far: number, up?: Vector3): void;
  /** Intensity in three.js r180 units (DirectionalLight.intensity). */
  setKeyLight(x: number, y: number, z: number, r: number, g: number, b: number, intensity?: number): void;
  /** Intensity in three.js r180 units (HemisphereLight.intensity). */
  setAmbient(skyR: number, skyG: number, skyB: number, groundR: number, groundG: number, groundB: number, intensity?: number): void;
  setClearColor(r: number, g: number, b: number): void;
  /** Times frames on the CPU; off by default (two boxed numbers a frame in V8). */
  setCpuTiming(on: boolean): void;
  /** Draws a frame; false from the context's loss on, for good: after a restore, create a new renderer. */
  frame(): boolean;
  /** [cpuMs, gpuMs or NaN, GPU frame, draws, triangles, culled, program binds, material binds]; `out` holds at least 8. */
  readStats(out: Float64Array): Float64Array;
  dispose(): void;
}
