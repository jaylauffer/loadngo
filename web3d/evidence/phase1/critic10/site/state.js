// Tenth critic: page-made contexts left with state the PS probe does not set. Each case makes its
// own canvas and context, leaves the state, creates a renderer, draws one frame and reads the
// canvas's own drawing buffer in the same task. Results in window.__probe10.done.
import { createRenderer } from '/web3d/pkg/web3d.js';

const out = { cases: [], facts: {} };
const N = 0.1;
const F = 10;
const ORTHO = new Float32Array([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, -2 / (F - N), 0, 0, 0, -(F + N) / (F - N), 1]);
const IDENT = new Float32Array([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1]);
const quad = (z = -5, uvs = true) => {
  const q = {
    positions: new Float32Array([-1, -1, z, 1, -1, z, 1, 1, z, -1, 1, z]),
    normals: new Float32Array([0, 0, 1, 0, 0, 1, 0, 0, 1, 0, 0, 1]),
    uvs: new Float32Array([0, 1, 1, 1, 1, 0, 0, 0]),
    indices: new Uint32Array([0, 1, 2, 0, 2, 3]),
  };
  if (!uvs) delete q.uvs;
  return q;
};
const flat = (r) => {
  r.setCamera(IDENT, ORTHO);
  r.setKeyLight(0, 0, 1, 0, 0, 0, 0);
  r.setAmbient(1, 1, 1, 1, 1, 1, Math.PI);
  r.setClearColor(0, 0, 0);
};
const bitmap = (w, h, rgba) => createImageBitmap(new ImageData(new Uint8ClampedArray(rgba), w, h), { premultiplyAlpha: 'none', colorSpaceConversion: 'none' });
const settle = (p) => p.then((r) => ({ ok: true, r }), (e) => ({ ok: false, message: String(e?.message ?? e) }));
const pixel = (gl, nx, ny) => {
  const px = new Uint8Array(4);
  gl.bindFramebuffer(gl.READ_FRAMEBUFFER, null);
  gl.readPixels(Math.floor(((nx + 1) / 2) * gl.drawingBufferWidth), Math.floor(((ny + 1) / 2) * gl.drawingBufferHeight), 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, px);
  return [px[0], px[1], px[2]];
};
const isGreen = (p) => p[1] > 200 && p[0] < 70 && p[2] < 70;
const release = (gl) => {
  if (gl && !gl.isContextLost()) gl.getExtension('WEBGL_lose_context')?.loseContext();
};
let n = 0;
const canvasFor = (w = 16, h = 16) => {
  const c = document.createElement('canvas');
  c.id = `s10-${++n}`;
  c.width = w;
  c.height = h;
  document.body.append(c);
  return c;
};

const draw = {
  green: (r, gl) => {
    flat(r);
    r.createObject(r.createMesh(quad()), r.createMaterial({ baseColor: [0, 1, 0, 1] }));
    r.frame();
    const px = pixel(gl, 0, 0);
    return { px, ok: isGreen(px) };
  },
  // A blended material of alpha 1 over black: green by addition with SRC_ALPHA, ONE_MINUS_SRC_ALPHA.
  blend: (r, gl) => {
    flat(r);
    r.createObject(r.createMesh(quad()), r.createMaterial({ baseColor: [0, 1, 0, 1], alphaMode: 'blend' }));
    r.frame();
    const px = pixel(gl, 0, 0);
    return { px, ok: isGreen(px) };
  },
  // Textured mesh without uvs: the uv attribute's generic value, (0, 0, 0, 1), picks the left texel (green).
  attrib: (r, gl, b) => {
    flat(r);
    const t = r.createTexture(b, { mipmaps: false, wrapS: 'clamp', wrapT: 'clamp' });
    r.createObject(r.createMesh(quad(-5, false)), r.createMaterial({ texture: t }));
    r.frame();
    const px = pixel(gl, 0, 0);
    return { px, ok: isGreen(px) };
  },
  // The 2x2 red, green, blue, white texture upright.
  upload: (r, gl, b) => {
    flat(r);
    const t = r.createTexture(b, { mipmaps: false, wrapS: 'clamp', wrapT: 'clamp' });
    r.createObject(r.createMesh(quad()), r.createMaterial({ texture: t }));
    r.frame();
    const tl = pixel(gl, -0.75, 0.75);
    const tr = pixel(gl, 0.75, 0.75);
    const bl = pixel(gl, -0.75, -0.75);
    const only = (p, c) => p.every((v, i) => (i === c ? v > 200 : v < 70));
    return { px: { tl, tr, bl }, ok: only(tl, 0) && only(tr, 1) && only(bl, 2) };
  },
  // A mid grey, linear 0.2: sRGB-encoded once it reads 124; encoded twice it reads about 186.
  grey: (r, gl) => {
    flat(r);
    r.createObject(r.createMesh(quad()), r.createMaterial({ baseColor: [0.2, 0.2, 0.2, 1] }));
    r.frame();
    const px = pixel(gl, 0, 0);
    return { px, ok: px.every((v) => Math.abs(v - 124) <= 2) };
  },
  // A 2d canvas painted sRGB (200, 100, 50), uploaded: read back as (200, 100, 50) when no colour conversion runs.
  canvasUpload: (r, gl, src) => {
    flat(r);
    const t = r.createTexture(src, { mipmaps: false, wrapS: 'clamp', wrapT: 'clamp' });
    r.createObject(r.createMesh(quad()), r.createMaterial({ texture: t }));
    r.frame();
    const px = pixel(gl, 0, 0);
    return { px, ok: Math.abs(px[0] - 200) <= 2 && Math.abs(px[1] - 100) <= 2 && Math.abs(px[2] - 50) <= 2 };
  },
};

const rgbw = () => bitmap(2, 2, [255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255]);
const greenRed = () => bitmap(2, 1, [0, 255, 0, 255, 255, 0, 0, 255]);
const painted = () => {
  const c = document.createElement('canvas');
  c.width = 4;
  c.height = 4;
  const x = c.getContext('2d');
  x.fillStyle = 'rgb(200, 100, 50)';
  x.fillRect(0, 0, 4, 4);
  return c;
};
const tfProgram = (gl) => {
  const sh = (type, src) => {
    const s = gl.createShader(type);
    gl.shaderSource(s, src);
    gl.compileShader(s);
    return s;
  };
  const p = gl.createProgram();
  gl.attachShader(p, sh(gl.VERTEX_SHADER, '#version 300 es\nout float v_out;\nvoid main() { v_out = 1.0; gl_Position = vec4(0.0, 0.0, 0.0, 1.0); gl_PointSize = 1.0; }'));
  gl.attachShader(p, sh(gl.FRAGMENT_SHADER, '#version 300 es\nprecision mediump float;\nout vec4 o;\nvoid main() { o = vec4(0.0); }'));
  gl.transformFeedbackVaryings(p, ['v_out'], gl.SEPARATE_ATTRIBS);
  gl.linkProgram(p);
  return p;
};

const cases = [
  ['depth mask false, the depth buffer cleared to 0 by the page', {}, (gl) => (gl.clearDepth(0), gl.clear(gl.DEPTH_BUFFER_BIT), gl.depthMask(false))],
  ['viewport 1x1 at the corner', {}, (gl) => gl.viewport(0, 0, 1, 1)],
  ['front face CW with culling on', {}, (gl) => (gl.enable(gl.CULL_FACE), gl.frontFace(gl.CW))],
  ['cull face FRONT_AND_BACK with culling on', {}, (gl) => (gl.enable(gl.CULL_FACE), gl.cullFace(gl.FRONT_AND_BACK))],
  ['stencil mask 0 and op ZERO, stencil test off, on a context with stencil', { stencil: true }, (gl) => (gl.stencilMask(0), gl.stencilOp(gl.ZERO, gl.ZERO, gl.ZERO))],
  ['dither off', {}, (gl) => gl.disable(gl.DITHER)],
  ['OES_draw_buffers_indexed: colorMaskiOES(0, false x4)', {}, (gl, k) => {
    const x = gl.getExtension('OES_draw_buffers_indexed');
    if (!x) return (k.unsupported = true);
    x.colorMaskiOES(0, false, false, false, false);
  }],
  ['OES_draw_buffers_indexed: blendEquationiOES(0, FUNC_REVERSE_SUBTRACT), a blended draw', {}, (gl, k) => {
    const x = gl.getExtension('OES_draw_buffers_indexed');
    if (!x) return (k.unsupported = true);
    x.blendEquationiOES(0, gl.FUNC_REVERSE_SUBTRACT);
  }, 'blend'],
  ['OES_draw_buffers_indexed: blendFunciOES(0, ZERO, ZERO), a blended draw', {}, (gl, k) => {
    const x = gl.getExtension('OES_draw_buffers_indexed');
    if (!x) return (k.unsupported = true);
    x.blendFunciOES(0, gl.ZERO, gl.ZERO);
  }, 'blend'],
  ['OES_draw_buffers_indexed: enableiOES(BLEND, 0) with blendFunciOES(0, ZERO, ZERO), an opaque draw', {}, (gl, k) => {
    const x = gl.getExtension('OES_draw_buffers_indexed');
    if (!x) return (k.unsupported = true);
    x.enableiOES(gl.BLEND, 0);
    x.blendFunciOES(0, gl.ZERO, gl.ZERO);
  }],
  ['WEBGL_polygon_mode: LINE (excluded by design)', {}, (gl, k) => {
    const x = gl.getExtension('WEBGL_polygon_mode');
    if (!x) return (k.unsupported = true);
    x.polygonModeWEBGL(gl.FRONT_AND_BACK, x.LINE_WEBGL);
    k.check = () => gl.getParameter(x.POLYGON_MODE_WEBGL);
  }],
  ['an integer generic value on the uv attribute (vertexAttribI4ui), a textured mesh without uvs', {}, (gl) => gl.vertexAttribI4ui(2, 1, 1, 0, 1), 'attrib', greenRed],
  ['vertexAttribDivisor 1 on attributes 0-2 of the default vertex array', {}, (gl) => { for (let i = 0; i < 3; i++) gl.vertexAttribDivisor(i, 1); }],
  ['uniform-buffer bindings 0-3 taken by page buffers of zeros', {}, (gl) => {
    for (let i = 0; i < 4; i++) {
      const b = gl.createBuffer();
      gl.bindBuffer(gl.UNIFORM_BUFFER, b);
      gl.bufferData(gl.UNIFORM_BUFFER, 4096, gl.STATIC_DRAW);
      gl.bindBufferBase(gl.UNIFORM_BUFFER, i, b);
    }
  }],
  ['a red texture bound on every unit, active unit 7, an upload and a textured draw', {}, (gl) => {
    const n = gl.getParameter(gl.MAX_COMBINED_TEXTURE_IMAGE_UNITS);
    const t = gl.createTexture();
    for (let u = 0; u < n; u++) {
      gl.activeTexture(gl.TEXTURE0 + u);
      gl.bindTexture(gl.TEXTURE_2D, t);
    }
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, 1, 1, 0, gl.RGBA, gl.UNSIGNED_BYTE, new Uint8Array([255, 0, 0, 255]));
    gl.activeTexture(gl.TEXTURE7);
  }, 'upload', rgbw],
  ['transform feedback active and paused on a page object, then unbound', {}, (gl, k) => {
    const p = tfProgram(gl);
    gl.useProgram(p);
    const tf = gl.createTransformFeedback();
    gl.bindTransformFeedback(gl.TRANSFORM_FEEDBACK, tf);
    const b = gl.createBuffer();
    gl.bindBufferBase(gl.TRANSFORM_FEEDBACK_BUFFER, 0, b);
    gl.bufferData(gl.TRANSFORM_FEEDBACK_BUFFER, 64, gl.STREAM_COPY);
    gl.beginTransformFeedback(gl.POINTS);
    gl.pauseTransformFeedback();
    gl.bindTransformFeedback(gl.TRANSFORM_FEEDBACK, null);
    k.note = 'paused capture left on an unbound object';
  }],
  ['transform feedback active and paused on the bound page object', {}, (gl) => {
    const p = tfProgram(gl);
    gl.useProgram(p);
    gl.bindTransformFeedback(gl.TRANSFORM_FEEDBACK, gl.createTransformFeedback());
    const b = gl.createBuffer();
    gl.bindBufferBase(gl.TRANSFORM_FEEDBACK_BUFFER, 0, b);
    gl.bufferData(gl.TRANSFORM_FEEDBACK_BUFFER, 64, gl.STREAM_COPY);
    gl.beginTransformFeedback(gl.POINTS);
    gl.pauseTransformFeedback();
  }],
  ['a TRANSFORM_FEEDBACK_PRIMITIVES_WRITTEN and an ANY_SAMPLES_PASSED_CONSERVATIVE query left running', {}, (gl) => {
    gl.beginQuery(gl.TRANSFORM_FEEDBACK_PRIMITIVES_WRITTEN, gl.createQuery());
    gl.beginQuery(gl.ANY_SAMPLES_PASSED_CONSERVATIVE, gl.createQuery());
  }],
  ['WEBGL_clip_cull_distance: every CLIP_DISTANCEi on', {}, (gl, k) => {
    const x = gl.getExtension('WEBGL_clip_cull_distance');
    if (!x) return (k.unsupported = true);
    const m = gl.getParameter(x.MAX_CLIP_DISTANCES_WEBGL);
    for (let i = 0; i < m; i++) gl.enable(x.CLIP_DISTANCE0_WEBGL + i);
  }],
  ['EXT_blend_func_extended: blendFuncSeparate with SRC1 factors, a blended draw', {}, (gl, k) => {
    const x = gl.getExtension('EXT_blend_func_extended');
    if (!x) return (k.unsupported = true);
    gl.blendFuncSeparate(x.SRC1_COLOR_WEBGL, x.ONE_MINUS_SRC1_COLOR_WEBGL, gl.ZERO, gl.ZERO);
  }, 'blend'],
  ['blendColor 0 with blendFunc CONSTANT_COLOR, a blended draw', {}, (gl) => (gl.blendColor(0, 0, 0, 0), gl.blendFunc(gl.CONSTANT_COLOR, gl.ZERO)), 'blend'],
  ['UNPACK_ALIGNMENT 8 and UNPACK_IMAGE_HEIGHT 1, an upload', {}, (gl) => (gl.pixelStorei(gl.UNPACK_ALIGNMENT, 8), gl.pixelStorei(gl.UNPACK_IMAGE_HEIGHT, 1)), 'upload', rgbw],
  ['UNPACK_COLORSPACE_CONVERSION_WEBGL BROWSER_DEFAULT, an upload', {}, (gl) => gl.pixelStorei(gl.UNPACK_COLORSPACE_CONVERSION_WEBGL, gl.BROWSER_DEFAULT_WEBGL), 'upload', rgbw],
  ['unpackColorSpace display-p3, a 2d canvas painted sRGB (200, 100, 50) uploaded', {}, (gl, k) => {
    if (!('unpackColorSpace' in gl)) return (k.unsupported = true);
    gl.unpackColorSpace = 'display-p3';
    k.check = () => gl.unpackColorSpace;
  }, 'canvasUpload', painted],
  ['drawingBufferColorSpace display-p3 (pixels read back as written; their meaning is P3)', {}, (gl, k) => {
    if (!('drawingBufferColorSpace' in gl)) return (k.unsupported = true);
    gl.drawingBufferColorSpace = 'display-p3';
    k.check = () => gl.drawingBufferColorSpace;
  }, 'grey'],
  ['drawingBufferStorage(SRGB8_ALPHA8): an sRGB drawing buffer, a mid grey', {}, (gl, k) => {
    if (typeof gl.drawingBufferStorage !== 'function') return (k.unsupported = true);
    gl.drawingBufferStorage(gl.SRGB8_ALPHA8, 16, 16);
    k.check = () => gl.drawingBufferFormat;
  }, 'grey'],
  ['drawingBufferStorage(RGBA16F): a float drawing buffer, a mid grey', {}, (gl, k) => {
    if (typeof gl.drawingBufferStorage !== 'function') return (k.unsupported = true);
    gl.getExtension('EXT_color_buffer_float');
    gl.drawingBufferStorage(gl.RGBA16F, 16, 16);
    k.check = () => gl.drawingBufferFormat;
  }, 'grey'],
  ['nothing changed (control), a draw', {}, () => {}],
  ['nothing changed (control), a mid grey', {}, () => {}, 'grey'],
  ['nothing changed (control), a 2d canvas uploaded', {}, () => {}, 'canvasUpload', painted],
];

async function run() {
  out.facts.extensions = (() => {
    const c = document.createElement('canvas');
    const gl = c.getContext('webgl2');
    const list = gl.getSupportedExtensions();
    release(gl);
    return list;
  })();
  for (const [name, attributes, leave, kind = 'green', source = null] of cases) {
    const c = canvasFor();
    const gl = c.getContext('webgl2', { antialias: false, ...attributes });
    const b = source ? await source() : null;
    const k = {};
    leave(gl, k);
    const res = { name, pageError: gl.getError() };
    if (k.unsupported) {
      out.cases.push({ ...res, skipped: 'no such extension or attribute here' });
      release(gl);
      continue;
    }
    const r = await settle(createRenderer(c, { width: 16, height: 16, antialias: false }));
    if (!r.ok) {
      out.cases.push({ ...res, created: r.message, pass: false });
      release(gl);
      continue;
    }
    try {
      const d = draw[kind](r.r, gl, b);
      res.pixel = d.px;
      res.drawn = d.ok;
    } catch (e) {
      res.threw = String(e?.message ?? e).slice(0, 300);
    }
    res.glError = gl.getError();
    if (k.check) res.after = k.check();
    if (k.note) res.note = k.note;
    r.r.dispose();
    res.pass = res.pageError === 0 && res.drawn === true && res.glError === 0;
    out.cases.push(res);
    release(gl);
  }
  out.passed = out.cases.filter((x) => x.pass).length;
  out.failed = out.cases.filter((x) => x.pass === false).map((x) => x.name);
  out.skipped = out.cases.filter((x) => x.skipped).map((x) => x.name);
  return out;
}
window.__probe10 = { done: run().catch((e) => ({ error: String(e?.stack ?? e) })) };
