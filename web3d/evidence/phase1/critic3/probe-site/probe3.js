// Third critic's GPU probes for web3d phase 1. Each test records what it saw; nothing here is
// trusted from the builder's probes. window.__p3 resolves to { info, tests: [{ name, pass, detail }] }.
import { createRenderer } from '/web3d/pkg/web3d.js';

const out = { tests: [] };
const log = (name, pass, detail) => out.tests.push({ name, pass: Boolean(pass), detail });
const attempt = (fn) => {
  try {
    return { threw: false, value: fn() };
  } catch (e) {
    return { threw: true, message: String(e?.message ?? e) };
  }
};
const N = 0.1;
const F = 10;
const ORTHO = new Float32Array([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, -2 / (F - N), 0, 0, 0, -(F + N) / (F - N), 1]);
const IDENT = new Float32Array([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1]);
const quad = (z = -5) => ({
  positions: new Float32Array([-1, -1, z, 1, -1, z, 1, 1, z, -1, 1, z]),
  normals: new Float32Array([0, 0, 1, 0, 0, 1, 0, 0, 1, 0, 0, 1]),
  uvs: new Float32Array([0, 1, 1, 1, 1, 0, 0, 0]),
  indices: new Uint32Array([0, 1, 2, 0, 2, 3]),
});
const reader = (gl, canvas) => {
  const px = new Uint8Array(4);
  return (nx, ny) => {
    gl.readPixels(Math.floor(((nx + 1) / 2) * canvas.width), Math.floor(((ny + 1) / 2) * canvas.height), 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, px);
    return [px[0], px[1], px[2]];
  };
};
const bitmap = (rgba) =>
  createImageBitmap(new ImageData(new Uint8ClampedArray(rgba), 1, 1), { premultiplyAlpha: 'none', colorSpaceConversion: 'none' });

/** Raw WebGL2, no web3d: is primitive restart really on for u16 0xFFFF in this browser? */
function rawRestart() {
  const canvas = document.getElementById('raw');
  const gl = canvas.getContext('webgl2', { antialias: false });
  const vs = `#version 300 es
in vec2 p; void main() { gl_Position = vec4(p, 0.0, 1.0); }`;
  const fs = `#version 300 es
precision mediump float; out vec4 o; void main() { o = vec4(1.0, 0.0, 0.0, 1.0); }`;
  const prog = gl.createProgram();
  for (const [type, src] of [[gl.VERTEX_SHADER, vs], [gl.FRAGMENT_SHADER, fs]]) {
    const s = gl.createShader(type);
    gl.shaderSource(s, src);
    gl.compileShader(s);
    gl.attachShader(prog, s);
  }
  gl.bindAttribLocation(prog, 0, 'p');
  gl.linkProgram(prog);
  gl.useProgram(prog);
  const read = reader(gl, canvas);
  const results = {};
  for (const [n, type, last] of [
    [65535, 'u16', 65534],
    [65536, 'u16', 65535],
    [65536, 'u32', 65535],
    [65537, 'u32', 65536],
  ]) {
    const pos = new Float32Array(n * 2);
    pos.set([3, -1], 0);
    pos.set([-1, 3], 2);
    pos.set([-1, -1], last * 2);
    const vbo = gl.createBuffer();
    gl.bindBuffer(gl.ARRAY_BUFFER, vbo);
    gl.bufferData(gl.ARRAY_BUFFER, pos, gl.STATIC_DRAW);
    gl.enableVertexAttribArray(0);
    gl.vertexAttribPointer(0, 2, gl.FLOAT, false, 0, 0);
    const ibo = gl.createBuffer();
    gl.bindBuffer(gl.ELEMENT_ARRAY_BUFFER, ibo);
    const idx = type === 'u16' ? new Uint16Array([last, 0, 1]) : new Uint32Array([last, 0, 1]);
    gl.bufferData(gl.ELEMENT_ARRAY_BUFFER, idx, gl.STATIC_DRAW);
    gl.clearColor(0, 0, 0, 1);
    gl.clear(gl.COLOR_BUFFER_BIT);
    gl.drawElements(gl.TRIANGLES, 3, type === 'u16' ? gl.UNSIGNED_SHORT : gl.UNSIGNED_INT, 0);
    results[`${n} ${type} index ${last}`] = read(0, 0);
    gl.deleteBuffer(vbo);
    gl.deleteBuffer(ibo);
  }
  const drew = (k) => results[k][0] > 200;
  log(
    'R0 raw WebGL2 control: u16 index 0xFFFF restarts (draws nothing); u32 65535 and u16 65534 draw',
    drew('65535 u16 index 65534') && !drew('65536 u16 index 65535') && drew('65536 u32 index 65535') && drew('65537 u32 index 65536'),
    { results, getError: gl.getError() },
  );
}

async function main() {
  rawRestart();
  const canvas = document.getElementById('main');
  const r = await createRenderer(canvas, { width: 128, height: 128, antialias: false });
  out.info = r.info;
  const gl = r.gl;
  const ctx = gl;
  out.precision = {
    fragMedium: (({ rangeMin, rangeMax, precision }) => ({ rangeMin, rangeMax, precision }))(gl.getShaderPrecisionFormat(gl.FRAGMENT_SHADER, gl.MEDIUM_FLOAT)),
    fragHigh: (({ rangeMin, rangeMax, precision }) => ({ rangeMin, rangeMax, precision }))(gl.getShaderPrecisionFormat(gl.FRAGMENT_SHADER, gl.HIGH_FLOAT)),
  };
  // Record every drawElements call's index type.
  const drawTypes = [];
  const realDraw = ctx.drawElements.bind(ctx);
  ctx.drawElements = (mode, count, type, offset) => {
    drawTypes.push({ count, type: type === ctx.UNSIGNED_SHORT ? 'u16' : type === ctx.UNSIGNED_INT ? 'u32' : type });
    return realDraw(mode, count, type, offset);
  };
  const read = reader(gl, canvas);
  r.setCamera(IDENT, ORTHO);
  r.setClearColor(0, 0, 0);
  r.setKeyLight(0, 0, 1, 1, 1, 1, Math.PI);
  r.setAmbient(0, 0, 0, 0, 0, 0, 0);
  const white = r.createMaterial({ baseColor: [1, 1, 1, 1] });
  const all = [];
  const show = (...objs) => {
    for (const o of all) r.setVisible(o, objs.includes(o));
  };
  const obj = (mesh, mat, m = IDENT) => {
    const o = r.createObject(mesh, mat, m);
    all.push(o);
    return o;
  };

  // P: the primitive-restart boundary through web3d, with the index type it actually chose.
  const restart = {};
  for (const n of [65535, 65536, 65537]) {
    const pos = new Float32Array(n * 3);
    for (let i = 0; i < n; i++) pos[i * 3 + 2] = -5;
    pos.set([3, -1, -5], 0);
    pos.set([-1, 3, -5], 3);
    pos.set([-1, -1, -5], (n - 1) * 3);
    const nor = new Float32Array(n * 3);
    for (let i = 0; i < n; i++) nor[i * 3 + 2] = 1;
    const o = obj(r.createMesh({ positions: pos, normals: nor, indices: new Uint32Array([n - 1, 0, 1]) }), white);
    show(o);
    drawTypes.length = 0;
    r.frame();
    restart[n] = { pixel: read(0, 0), draws: [...drawTypes] };
  }
  log('P 65,535 vertices: u16, draws', restart[65535].draws[0]?.type === 'u16' && restart[65535].pixel[0] > 200, restart[65535]);
  log('P 65,536 vertices: u32, draws', restart[65536].draws[0]?.type === 'u32' && restart[65536].pixel[0] > 200, restart[65536]);
  log('P 65,537 vertices: u32, draws', restart[65537].draws[0]?.type === 'u32' && restart[65537].pixel[0] > 200, restart[65537]);

  // T: the texture state cache across texture creation (the shim leaves the new one on unit 0),
  // including a failed upload (closed ImageBitmap).
  {
    r.setKeyLight(0, 0, 1, 0, 0, 0, 0);
    r.setAmbient(1, 1, 1, 1, 1, 1, Math.PI / 1.04);
    const texRed = r.createTexture(await bitmap([255, 0, 0, 255]), { mipmaps: false });
    const oRed = obj(r.createMesh(quad()), r.createMaterial({ texture: texRed }));
    show(oRed);
    r.frame();
    const p1 = read(0, 0);
    const texGreen = r.createTexture(await bitmap([0, 255, 0, 255]), { mipmaps: false });
    r.frame();
    const p2 = read(0, 0);
    const closed = await bitmap([0, 0, 255, 255]);
    closed.close();
    const failed = attempt(() => r.createTexture(closed, { mipmaps: false }));
    r.frame();
    const p3 = read(0, 0);
    const oGreen = obj(r.createMesh(quad()), r.createMaterial({ texture: texGreen }));
    show(oGreen);
    r.frame();
    const p4 = read(0, 0);
    show(oRed);
    r.frame();
    const p5 = read(0, 0);
    log('T texture cache: red stays red after another texture is created, after a failed upload, and after drawing green', p1[0] > 200 && p1[1] < 60 && p2[0] > 200 && p2[1] < 60 && p3[0] > 200 && p3[1] < 60 && p4[1] > 200 && p4[0] < 60 && p5[0] > 200 && p5[1] < 60, { p1, p2, failed, p3, p4, p5, getError: gl.getError() });
  }

  // V: creating a mesh between frames must not land its index buffer in the last-bound vertex array.
  {
    r.setKeyLight(0, 0, 1, 1, 1, 1, Math.PI);
    r.setAmbient(0, 0, 0, 0, 0, 0, 0);
    const meshX = r.createMesh(quad());
    const oX = obj(meshX, white);
    show(oX);
    r.frame();
    const before = read(-0.9, 0.9);
    // A one-triangle mesh: if its index buffer replaced X's, X's 6-index draw reads past its end.
    r.createMesh({ positions: new Float32Array([0, 0, -5, 0.1, 0, -5, 0, 0.1, -5]), indices: new Uint32Array([0, 1, 2]) });
    r.frame();
    const after = read(-0.9, 0.9);
    const err = gl.getError();
    log('V a mesh created between frames leaves the last vertex array alone', before[0] > 200 && after[0] > 200 && err === 0, { before, after, getError: err });
  }

  // H: handles coerced by the wasm boundary (ToInt32): a non-integer or a handle plus 2^32.
  {
    const o = obj(r.createMesh(quad()), white);
    show(o);
    r.frame();
    const shown = read(0, 0);
    const alias = attempt(() => r.setVisible(o + 2 ** 32, false));
    r.frame();
    const hiddenByAlias = read(0, 0);
    const frac = attempt(() => r.setVisible(o + 0.75, true));
    r.frame();
    const shownByFraction = read(0, 0);
    const str = attempt(() => r.setVisible(String(o), false));
    r.frame();
    const hiddenByString = read(0, 0);
    log('H (informational) a handle plus 2^32, plus 0.75, or as a string is refused, not aliased', alias.threw && frac.threw && str.threw, { handle: o, alias, hiddenByAlias, frac, shownByFraction, str, hiddenByString, shown });
    show(o);
  }

  // C: a non-finite clear colour.
  {
    show();
    const nan = attempt(() => r.setClearColor(NaN, 0, 0));
    r.frame();
    const p = read(0, 0);
    log('C (informational) a NaN clear colour is refused like every other non-finite input', nan.threw, { nan, pixel: p, getError: gl.getError() });
    r.setClearColor(0, 0, 0);
  }

  // Z: resize to zero and back.
  {
    const o = obj(r.createMesh(quad()), white);
    show(o);
    const zero = attempt(() => r.resize(0, 0));
    const f0 = attempt(() => r.frame());
    const dims0 = [gl.drawingBufferWidth, gl.drawingBufferHeight, canvas.width, canvas.height];
    const back = attempt(() => r.resize(96, 64));
    r.frame();
    const corners = [read(-0.95, -0.95), read(0.95, 0.95)];
    const vp = gl.getParameter(gl.VIEWPORT);
    log('Z resize to 0x0 and back: no throw, viewport follows, corners drawn', !zero.threw && !f0.threw && !back.threw && corners.every((c) => c[0] > 200) && vp[2] === 96 && vp[3] === 64, { zero, f0, dims0, back, corners, viewport: Array.from(vp), getError: gl.getError() });
  }
  out.mainGlError = gl.getError();
  r.dispose();

  // S: two renderers on one canvas share one WebGL context. Does the first still draw its own frame?
  {
    const c2 = document.getElementById('other');
    const a = await createRenderer(c2, { width: 64, height: 64, antialias: false });
    const g = a.gl;
    const rd = reader(g, c2);
    a.setCamera(IDENT, ORTHO);
    a.setKeyLight(0, 0, 1, 0, 0, 0, 0);
    a.setAmbient(1, 1, 1, 1, 1, 1, Math.PI);
    a.createObject(a.createMesh(quad()), a.createMaterial({ baseColor: [1, 0, 0, 1] }));
    a.frame();
    const aBefore = rd(0, 0);
    const b = await createRenderer(c2, { width: 64, height: 64, antialias: false }).catch((e) => e);
    let aAfter = null;
    let bDisposedErr = null;
    if (!(b instanceof Error)) {
      b.setCamera(IDENT, ORTHO);
      b.setKeyLight(0, 0, 1, 0, 0, 0, 0);
      b.setAmbient(0, 0, 0, 0, 0, 0, 0); // b's frame block: no light at all
      b.frame();
      a.frame();
      aAfter = rd(0, 0);
      b.dispose();
      a.frame();
      bDisposedErr = g.getError();
    }
    log('S (informational) a second renderer on the same canvas is refused, or leaves the first drawing correctly', b instanceof Error || (aAfter && aAfter[0] > 200 && bDisposedErr === 0), { sameContext: !(b instanceof Error) && b.gl === a.gl, aBefore, aAfter, afterSecondDisposedGetError: bDisposedErr, second: b instanceof Error ? b.message : 'created' });
    a.dispose();
  }
  out.passed = out.tests.filter((t) => t.pass).length;
  out.failed = out.tests.filter((t) => !t.pass).map((t) => t.name);
}

window.__p3 = main().then(
  () => out,
  (e) => {
    out.error = String(e?.stack ?? e);
    return out;
  },
);
