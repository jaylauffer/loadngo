// Tenth critic, second state page: the cases the first got wrong by its own reading (the quad's
// diagonal crosses the centre; a float drawing buffer cannot be read as bytes; the grey and canvas
// values depend on the lighting, so every case is compared with its own control drawn the same
// way), plus ImageBitmap and ImageData sources under a page's unpackColorSpace, and the polygon-mode
// warning counted per context. The driver screenshots the drawingBufferColorSpace canvases.
import { createRenderer } from '/web3d/pkg/web3d.js';

const out = { cases: {} };
const N = 0.1;
const F = 10;
const ORTHO = new Float32Array([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, -2 / (F - N), 0, 0, 0, -(F + N) / (F - N), 1]);
const IDENT = new Float32Array([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1]);
const quad = () => ({
  positions: new Float32Array([-1, -1, -5, 1, -1, -5, 1, 1, -5, -1, 1, -5]),
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
const timeout = (ms) => new Promise((res) => setTimeout(res, ms));
const readBytes = (gl, nx, ny) => {
  const px = new Uint8Array(4);
  gl.bindFramebuffer(gl.READ_FRAMEBUFFER, null);
  gl.readPixels(Math.floor(((nx + 1) / 2) * gl.drawingBufferWidth), Math.floor(((ny + 1) / 2) * gl.drawingBufferHeight), 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, px);
  return [px[0], px[1], px[2]];
};
const readFloats = (gl, nx, ny) => {
  const px = new Float32Array(4);
  gl.bindFramebuffer(gl.READ_FRAMEBUFFER, null);
  gl.readPixels(Math.floor(((nx + 1) / 2) * gl.drawingBufferWidth), Math.floor(((ny + 1) / 2) * gl.drawingBufferHeight), 1, 1, gl.RGBA, gl.FLOAT, px);
  return [px[0], px[1], px[2]].map((v) => Math.round(v * 255));
};
const release = (gl) => {
  if (gl && !gl.isContextLost()) gl.getExtension('WEBGL_lose_context')?.loseContext();
};
let n = 0;
const canvasFor = (id, size = 16) => {
  const c = document.createElement('canvas');
  c.id = id ?? `s2-${++n}`;
  c.width = size;
  c.height = size;
  c.style.cssText = `width:${size}px;height:${size}px;display:inline-block`;
  document.body.append(c);
  return c;
};
const painted = () => {
  const c = document.createElement('canvas');
  c.width = 4;
  c.height = 4;
  const x = c.getContext('2d');
  x.fillStyle = 'rgb(200, 100, 50)';
  x.fillRect(0, 0, 4, 4);
  return c;
};
const pixels = new Uint8ClampedArray(64).map((_, i) => [200, 100, 50, 255][i % 4]);
const sources = {
  canvas: async () => painted(),
  bitmap: () => createImageBitmap(new ImageData(new Uint8ClampedArray(pixels), 4, 4), { premultiplyAlpha: 'none', colorSpaceConversion: 'none' }),
  imageData: async () => new ImageData(new Uint8ClampedArray(pixels), 4, 4),
};

/** Makes a page context, leaves state, creates a renderer, draws `color` or the texture, reads. */
async function one(id, leave, { color = [0, 1, 0, 1], source = null, read = readBytes, at = [0, 0], size = 16, keep = false } = {}) {
  const c = canvasFor(id, size);
  const gl = c.getContext('webgl2', { antialias: false });
  const src = source ? await sources[source]() : null;
  leave(gl);
  const r = await createRenderer(c, { width: size, height: size, antialias: false });
  flat(r);
  const material = src ? r.createMaterial({ texture: r.createTexture(src, { mipmaps: false, wrapS: 'clamp', wrapT: 'clamp' }) }) : r.createMaterial({ baseColor: color });
  r.createObject(r.createMesh(quad()), material);
  r.frame();
  const res = { pixels: at.length === 2 ? read(gl, at[0], at[1]) : at.map(([x, y]) => read(gl, x, y)), glError: gl.getError() };
  if (keep) {
    res.renderer = r;
  } else {
    r.dispose();
    release(gl);
  }
  return res;
}

async function run() {
  // Polygon mode, read off the quad's diagonal (the diagonal runs through the centre).
  const offDiagonal = [[-0.5, 0.5], [0.5, -0.5], [0, 0]];
  out.cases.polygonLine = await one(null, (gl) => {
    const x = gl.getExtension('WEBGL_polygon_mode');
    x.polygonModeWEBGL(gl.FRONT_AND_BACK, x.LINE_WEBGL);
  }, { at: offDiagonal });
  out.cases.polygonControl = await one(null, () => {}, { at: offDiagonal });

  // A page's unpackColorSpace, for each source kind web3d takes; each against its control.
  for (const source of Object.keys(sources)) {
    out.cases[`unpackP3-${source}`] = await one(null, (gl) => (gl.unpackColorSpace = 'display-p3'), { source });
    out.cases[`unpackControl-${source}`] = await one(null, () => {}, { source });
  }

  // drawingBufferStorage: an sRGB drawing buffer and a float one, each read in its own type,
  // against an RGBA8 control. Linear 0.2 grey.
  const grey = [0.2, 0.2, 0.2, 1];
  out.cases.storageControl = await one(null, () => {}, { color: grey });
  out.cases.storageSrgb = await one(null, (gl) => gl.drawingBufferStorage(gl.SRGB8_ALPHA8, 16, 16), { color: grey });
  out.cases.storageFloat = await one(null, (gl) => (gl.getExtension('EXT_color_buffer_float'), gl.drawingBufferStorage(gl.RGBA16F, 16, 16)), { color: grey, read: readFloats });

  // drawingBufferColorSpace: the same bytes, shown as P3 instead of sRGB. Two 64-pixel canvases
  // left on the page for the driver to screenshot (a saturated orange, linear (0.6, 0.12, 0.02)).
  const orange = [0.6, 0.12, 0.02, 1];
  out.cases.spaceControl = await one('space-control', () => {}, { color: orange, size: 64, keep: true });
  out.cases.spaceP3 = await one('space-p3', (gl) => (gl.drawingBufferColorSpace = 'display-p3'), { color: orange, size: 64, keep: true });
  out.spaceP3After = out.cases.spaceP3.renderer.gl.drawingBufferColorSpace;
  for (const k of ['spaceControl', 'spaceP3']) delete out.cases[k].renderer;

  // The polygon-mode warning: getExtension on three fresh contexts, a task apart, with marks.
  for (let i = 0; i < 3; i++) {
    console.info(`MARK polygon-mode context ${i}`);
    const gl = document.createElement('canvas').getContext('webgl2');
    gl.getExtension('WEBGL_polygon_mode');
    await timeout(200);
    release(gl);
  }
  console.info('MARK polygon-mode end');
  return out;
}
window.__probe10 = { done: run().catch((e) => ({ error: String(e?.stack ?? e), partial: out })) };
