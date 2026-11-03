// Types for loadngo-web3d's JS API (web3d.js). BSD-3-Clause.

/** 16 numbers, column-major: three.js's `Matrix4.elements` (an Array or a typed array of exactly 16). */
export type Matrix4 = ArrayLike<number>;
export type Vector3 = ArrayLike<number>;

declare const brand: unique symbol;
/**
 * Handles are numbers that name one kind of thing in one renderer: the Rust side refuses a handle
 * of another kind or from another renderer. The brands let TypeScript catch a swapped argument.
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
  /** Where the wasm is: a URL, a Response, bytes or a compiled module (default: beside web3d.js). */
  wasm?: string | URL | Response | BufferSource | WebAssembly.Module;
  onContextLost?: () => void;
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

export type TextureSource =
  | ImageBitmap
  | HTMLImageElement
  | HTMLCanvasElement
  | OffscreenCanvas
  | HTMLVideoElement
  | ImageData
  | VideoFrame;

export interface TextureOptions {
  /** Colour data, decoded from sRGB when sampled (default true). */
  srgb?: boolean;
  mipmaps?: boolean;
  wrapS?: 'repeat' | 'clamp' | 'mirror';
  wrapT?: 'repeat' | 'clamp' | 'mirror';
}

export interface MaterialOptions {
  /** Linear RGBA (default white). */
  baseColor?: ArrayLike<number>;
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
 * context's antialias setting; a later renderer on it must ask for the same.
 */
export function createRenderer(canvas: HTMLCanvasElement | OffscreenCanvas, options?: RendererOptions): Promise<Renderer>;

/**
 * After dispose() every method throws "web3d: renderer disposed". Once the WebGL context is lost,
 * frame() returns false and every create* throws; dispose the renderer, then create a new one after
 * onContextRestored and upload everything again.
 */
export class Renderer {
  readonly canvas: HTMLCanvasElement | OffscreenCanvas;
  readonly info: RendererInfo;
  /** True once the context has been lost; it stays true after a restore (create a new renderer). */
  readonly lost: boolean;
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
  /** Draws a frame; false while the context is lost. */
  frame(): boolean;
  /** [cpuMs, gpuMs or NaN, GPU frame, draws, triangles, culled, program binds, material binds]; `out` holds at least 8. */
  readStats(out: Float64Array): Float64Array;
  dispose(): void;
}
