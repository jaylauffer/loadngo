// Critic 11: attacks on the reset (gl.js reset_webgl_state and its polygon-mode test draw).
import * as web3d from '/pkg/web3d.js';
import { sleep, err, tryCreate, drawQuad } from './common.js';
const out = {};
const T = async (name, f) => { try { out[name] = await f(); } catch (e) { out[name] = { threw: err(e) }; } };
const canvas = (w = 64, h = 64) => { const c = document.createElement('canvas'); c.width = w; c.height = h; document.body.appendChild(c); return c; };
const ATTR = { alpha: false, antialias: false, depth: true, stencil: false };

await T('S1 platform', () => {
  const gl = canvas().getContext('webgl2', ATTR);
  const dbg = gl.getExtension('WEBGL_debug_renderer_info');
  return {
    renderer: gl.getParameter(dbg ? dbg.UNMASKED_RENDERER_WEBGL : gl.RENDERER),
    lineWidthRange: [...gl.getParameter(gl.ALIASED_LINE_WIDTH_RANGE)],
    polygonMode: gl.getSupportedExtensions().includes('WEBGL_polygon_mode'),
    drawingBufferStorage: typeof gl.drawingBufferStorage === 'function',
    drawingBufferToneMapping: 'drawingBufferToneMapping' in gl,
    drawBuffersIndexed: gl.getSupportedExtensions().includes('OES_draw_buffers_indexed'),
  };
});

// S2. The polygon-mode test draw and the line width it leaves as the page set it.
for (const width of [1, 3, 5, 8, 'max']) {
  await T(`S2 page left LINE mode with lineWidth ${width}`, async () => {
    const c = canvas();
    const gl = c.getContext('webgl2', ATTR);
    const pm = gl.getExtension('WEBGL_polygon_mode');
    if (!pm) return { skipped: 'no WEBGL_polygon_mode' };
    pm.polygonModeWEBGL(gl.FRONT_AND_BACK, pm.LINE_WEBGL);
    const w = width === 'max' ? gl.getParameter(gl.ALIASED_LINE_WIDTH_RANGE)[1] : width;
    gl.lineWidth(w);
    const t = await tryCreate(web3d, c);
    if (!t.ok) return { create: t.error };
    const d = drawQuad(t.r);
    const r = { lineWidthSet: w, lineWidthNow: gl.getParameter(gl.LINE_WIDTH), modeAfter: gl.getParameter(pm.POLYGON_MODE_WEBGL) === pm.FILL_WEBGL ? 'FILL' : 'LINE', centre: d.centre, offDiagonal: d.offDiagonal, error: d.error };
    r.filled = d.offDiagonal[1] > 100;
    t.r.dispose();
    return r;
  });
}

// S3. drawingBufferStorage on a context without alpha.
await T('S3 drawingBufferStorage on alpha:false', () => {
  const r = {};
  for (const fmt of ['SRGB8_ALPHA8', 'RGBA16F', 'RGBA8']) {
    const gl = canvas().getContext('webgl2', ATTR);
    gl.getExtension('EXT_color_buffer_half_float');
    const before = gl.drawingBufferFormat;
    gl.drawingBufferStorage(gl[fmt], gl.drawingBufferWidth, gl.drawingBufferHeight);
    r[fmt] = { error: gl.getError(), before, after: gl.drawingBufferFormat, RGBA8: gl.RGBA8 };
  }
  return r;
});

// S4. A page's SRGB8_ALPHA8 buffer on an antialiased context with alpha: does web3d's reset to
// RGBA8 keep the multisampling it checked for?
await T('S4 antialias + alpha + SRGB8_ALPHA8 storage, then web3d', async () => {
  const c = canvas(64, 64);
  const gl = c.getContext('webgl2', { ...ATTR, alpha: true, antialias: true });
  gl.drawingBufferStorage(gl.SRGB8_ALPHA8, 64, 64);
  const r = { samplesBefore: gl.getParameter(gl.SAMPLES), formatBefore: gl.drawingBufferFormat === gl.SRGB8_ALPHA8 ? 'SRGB8_ALPHA8' : gl.drawingBufferFormat };
  const t = await tryCreate(web3d, c, { antialias: true });
  if (!t.ok) return { ...r, create: t.error };
  r.samplesAfter = gl.getParameter(gl.SAMPLES);
  r.formatAfter = gl.drawingBufferFormat === gl.RGBA8 ? 'RGBA8' : gl.drawingBufferFormat;
  r.infoSamples = t.r.info.samples;
  const d = drawQuad(t.r);
  r.centre = d.centre;
  r.error = d.error;
  t.r.dispose();
  return r;
});

// S5. A page's buffer of another size (drawingBufferStorage 40x30 on a 300x150 canvas).
for (const opts of [{ width: 300, height: 150 }, {}]) {
  await T(`S5 page storage 40x30 on a 300x150 canvas, web3d asked ${JSON.stringify(opts)}`, async () => {
    const c = canvas(300, 150);
    const gl = c.getContext('webgl2', { ...ATTR, alpha: true });
    gl.drawingBufferStorage(gl.SRGB8_ALPHA8, 40, 30);
    const r = { afterPageStorage: [gl.drawingBufferWidth, gl.drawingBufferHeight] };
    const t = await web3d.createRenderer(c, { antialias: false, ...opts });
    r.afterCreate = [gl.drawingBufferWidth, gl.drawingBufferHeight];
    r.canvas = [c.width, c.height];
    r.format = gl.drawingBufferFormat === gl.RGBA8 ? 'RGBA8' : gl.drawingBufferFormat;
    const d = drawQuad(t);
    r.size = d.size;
    // The quad covers the whole view: a corner pixel of the drawing buffer must be lit too.
    const p = new Uint8Array(4); gl.readPixels(d.size[0] - 2, d.size[1] - 2, 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, p);
    r.cornerPixel = [...p];
    t.resize(300, 150);
    r.afterResize = [gl.drawingBufferWidth, gl.drawingBufferHeight, gl.drawingBufferFormat === gl.RGBA8 ? 'RGBA8' : gl.drawingBufferFormat];
    t.dispose();
    return r;
  });
}

// S6. preserveDrawingBuffer: true, made by the page.
await T('S6 preserveDrawingBuffer page context', async () => {
  const c = canvas();
  c.getContext('webgl2', { ...ATTR, preserveDrawingBuffer: true });
  const t = await tryCreate(web3d, c);
  if (!t.ok) return { create: t.error };
  const d = drawQuad(t.r);
  t.r.dispose();
  return { centre: d.centre, error: d.error };
});

// S7. What the test draw could leave behind, read right after create (before the first frame).
await T('S7 state after create, before the first frame', async () => {
  const c = canvas(64, 64);
  const gl = c.getContext('webgl2', ATTR);
  const pm = gl.getExtension('WEBGL_polygon_mode');
  if (pm) pm.polygonModeWEBGL(gl.FRONT_AND_BACK, pm.LINE_WEBGL); // so the test draw runs fully
  const t = await tryCreate(web3d, c);
  if (!t.ok) return { create: t.error };
  const r = {
    viewport: [...gl.getParameter(gl.VIEWPORT)],
    framebuffer: gl.getParameter(gl.FRAMEBUFFER_BINDING),
    texture2D: gl.getParameter(gl.TEXTURE_BINDING_2D),
    activeTexture: gl.getParameter(gl.ACTIVE_TEXTURE) - gl.TEXTURE0,
    error: gl.getError(),
  };
  const d = drawQuad(t.r);
  r.firstFrame = { centre: d.centre, viewportNow: [...gl.getParameter(gl.VIEWPORT)] };
  t.r.dispose();
  return r;
});

// S8. Per-draw-buffer state (OES_draw_buffers_indexed) the page left on buffer 0.
await T('S8 OES_draw_buffers_indexed: colour mask, blend equation and blend on buffer 0', async () => {
  const c = canvas();
  const gl = c.getContext('webgl2', ATTR);
  const ix = gl.getExtension('OES_draw_buffers_indexed');
  if (!ix) return { skipped: 'no OES_draw_buffers_indexed' };
  ix.colorMaskiOES(0, false, true, false, true);
  ix.blendEquationiOES(0, gl.MIN);
  ix.blendFunciOES(0, gl.ZERO, gl.ZERO);
  ix.enableiOES(gl.BLEND, 0);
  const t = await tryCreate(web3d, c);
  if (!t.ok) return { create: t.error };
  const d = drawQuad(t.r, [1, 0.5, 0]);
  t.r.dispose();
  return { centre: d.centre, error: d.error, expect: 'red > green > 0, as a new context draws it' };
});
await T('S8c control: the same quad on a new context', async () => {
  const c = canvas();
  const t = await tryCreate(web3d, c);
  const d = drawQuad(t.r, [1, 0.5, 0]);
  t.r.dispose();
  return { centre: d.centre, error: d.error };
});

window.__done = Promise.resolve(out);
