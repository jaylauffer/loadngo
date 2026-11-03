// The web3d phase 1 demo: a procedural scene of 64 draws and 320,000 triangles, a round load of
// its own, drawn by the Rust/WebAssembly renderer, with fps, GPU ms and CPU ms on screen.
//
// Opaque torus knots overlap in depth across a grid; one sphere wears a generated canvas texture;
// a masked, double-sided grille and a blended, double-sided glass pane exercise the other alpha
// modes; two shiny spheres show the specular. The camera orbits, and three objects move each
// frame. Nothing on the demo's per-frame path allocates (it holds the moving objects' handles in
// constants: read from an Array, a handle would be boxed at each call); the readout's text is
// rebuilt once a second.
//
// window.__web3d (for scripted checks): ready (a promise of the renderer's info), measure(ms)
// (a promise of frame-time, CPU and GPU statistics over the next ms of frames), pixels(points) (a
// promise of the drawing buffer's sRGB bytes at [x, y] points, counted from the top left, read
// right after the next frame), frames(ms) (a promise of how many frames were drawn in the next ms,
// with no statistics built, for allocation checks), control(on) (the allocation check's positive
// control: while on, each frame makes one short-lived Array of 8 numbers, about 90 bytes, which the
// next frame drops), clearColor (the linear background), glError().
//
// BSD-3-Clause; see the repository's LICENSE.

import { createRenderer } from '../pkg/web3d.js';

const params = new URLSearchParams(location.search);
const num = (name, fallback) => {
  const v = Number(params.get(name));
  return params.has(name) && Number.isFinite(v) ? v : fallback;
};
const SCALE = num('scale', 1);
const KNOTS = Math.max(0, Math.floor(num('knots', 58)));
const TUBULAR = Math.max(8, Math.floor(num('tubular', 192)));
const RADIAL = Math.max(3, Math.floor(num('radial', 14)));
const STILL = params.get('still') === '1';
// CPU timing is on unless cputime=0: it costs two boxed numbers a frame (the allocation check
// turns it off to show the rest of the frame allocates nothing).
const CPU_TIMING = params.get('cputime') !== '0';
// hud=0: no on-screen readout (its once-a-second text is the demo's only steady allocation).
const HUD = params.get('hud') !== '0';
// arrays=1: matrices and vectors as plain Arrays, as three.js holds them (Matrix4.elements), instead
// of Float32Arrays; the allocation check uses it, since that is the path an application takes.
const ARRAYS = params.get('arrays') === '1';

const canvas = document.getElementById('view');
const hud = document.getElementById('hud');

// ---- geometry -------------------------------------------------------------------------------

/** A torus knot, after three.js's TorusKnotGeometry (outward normals, counter-clockwise faces). */
function torusKnot(radius, tube, tubular, radial, p, q) {
  const count = (tubular + 1) * (radial + 1);
  const positions = new Float32Array(count * 3);
  const normals = new Float32Array(count * 3);
  const uvs = new Float32Array(count * 2);
  const indices = new Uint32Array(tubular * radial * 6);
  const p1 = [0, 0, 0];
  const p2 = [0, 0, 0];
  const curve = (u, out) => {
    const qu = (q / p) * u;
    const cs = Math.cos(qu);
    out[0] = radius * (2 + cs) * 0.5 * Math.cos(u);
    out[1] = radius * (2 + cs) * 0.5 * Math.sin(u);
    out[2] = radius * Math.sin(qu) * 0.5;
  };
  let v = 0;
  for (let i = 0; i <= tubular; i++) {
    const u = (i / tubular) * p * Math.PI * 2;
    curve(u, p1);
    curve(u + 0.01, p2);
    const t = [p2[0] - p1[0], p2[1] - p1[1], p2[2] - p1[2]];
    let n = [p2[0] + p1[0], p2[1] + p1[1], p2[2] + p1[2]];
    let b = cross(t, n);
    n = cross(b, t);
    b = unit(b);
    n = unit(n);
    for (let j = 0; j <= radial; j++) {
      const a = (j / radial) * Math.PI * 2;
      const cx = -tube * Math.cos(a);
      const cy = tube * Math.sin(a);
      const x = p1[0] + (cx * n[0] + cy * b[0]);
      const y = p1[1] + (cx * n[1] + cy * b[1]);
      const z = p1[2] + (cx * n[2] + cy * b[2]);
      positions.set([x, y, z], v * 3);
      normals.set(unit([x - p1[0], y - p1[1], z - p1[2]]), v * 3);
      uvs.set([i / tubular, j / radial], v * 2);
      v++;
    }
  }
  let k = 0;
  for (let j = 1; j <= tubular; j++) {
    for (let i = 1; i <= radial; i++) {
      const a = (radial + 1) * (j - 1) + (i - 1);
      const b = (radial + 1) * j + (i - 1);
      const c = (radial + 1) * j + i;
      const d = (radial + 1) * (j - 1) + i;
      indices.set([a, b, d, b, c, d], k);
      k += 6;
    }
  }
  return { positions, normals, uvs, indices };
}

/** A UV sphere; v = 0 at the top, as glTF maps an image's first row. */
function sphere(radius, widthSegments, heightSegments) {
  const count = (widthSegments + 1) * (heightSegments + 1);
  const positions = new Float32Array(count * 3);
  const normals = new Float32Array(count * 3);
  const uvs = new Float32Array(count * 2);
  const idx = [];
  let v = 0;
  for (let iy = 0; iy <= heightSegments; iy++) {
    const fv = iy / heightSegments;
    for (let ix = 0; ix <= widthSegments; ix++) {
      const fu = ix / widthSegments;
      const nx = -Math.cos(fu * Math.PI * 2) * Math.sin(fv * Math.PI);
      const ny = Math.cos(fv * Math.PI);
      const nz = Math.sin(fu * Math.PI * 2) * Math.sin(fv * Math.PI);
      positions.set([radius * nx, radius * ny, radius * nz], v * 3);
      normals.set([nx, ny, nz], v * 3);
      uvs.set([fu, fv], v * 2);
      v++;
    }
  }
  const row = widthSegments + 1;
  for (let iy = 0; iy < heightSegments; iy++) {
    for (let ix = 0; ix < widthSegments; ix++) {
      const a = iy * row + ix + 1;
      const b = iy * row + ix;
      const c = (iy + 1) * row + ix;
      const d = (iy + 1) * row + ix + 1;
      if (iy !== 0) idx.push(a, b, d);
      if (iy !== heightSegments - 1) idx.push(b, c, d);
    }
  }
  return { positions, normals, uvs, indices: new Uint32Array(idx) };
}

/** A ground grid in the XZ plane, facing +Y. */
function ground(size, segmentsX, segmentsZ) {
  const row = segmentsX + 1;
  const vertices = row * (segmentsZ + 1);
  const positions = new Float32Array(vertices * 3);
  const normals = new Float32Array(vertices * 3);
  const indices = new Uint32Array(segmentsX * segmentsZ * 6);
  for (let iz = 0; iz <= segmentsZ; iz++) {
    for (let ix = 0; ix <= segmentsX; ix++) {
      const v = iz * row + ix;
      positions.set([(ix / segmentsX - 0.5) * size, 0, (iz / segmentsZ - 0.5) * size], v * 3);
      normals.set([0, 1, 0], v * 3);
    }
  }
  let k = 0;
  for (let iz = 0; iz < segmentsZ; iz++) {
    for (let ix = 0; ix < segmentsX; ix++) {
      const a = iz * row + ix;
      indices.set([a, a + row, a + 1, a + 1, a + row, a + row + 1], k);
      k += 6;
    }
  }
  return { positions, normals, indices };
}

/** A width x height panel facing +Z, its image upright. */
function panel(width, height) {
  const w = width / 2;
  const h = height / 2;
  return {
    positions: new Float32Array([-w, -h, 0, w, -h, 0, w, h, 0, -w, h, 0]),
    normals: new Float32Array([0, 0, 1, 0, 0, 1, 0, 0, 1, 0, 0, 1]),
    uvs: new Float32Array([0, 1, 1, 1, 1, 0, 0, 0]),
    indices: new Uint32Array([0, 1, 2, 0, 2, 3]),
  };
}

function cross(a, b) {
  return [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
}

function unit(a) {
  const l = Math.hypot(a[0], a[1], a[2]) || 1;
  return [a[0] / l, a[1] / l, a[2] / l];
}

/** compose() with the yaw turning at `rate` radians a second, by the clock in clock[0]. */
function composeAt(out, clock, x, y, z, yaw0, rate, pitch) {
  const yaw = yaw0 + rate * clock[0];
  const cy = Math.cos(yaw);
  const sy = Math.sin(yaw);
  const cp = Math.cos(pitch);
  const sp = Math.sin(pitch);
  out[0] = cy; out[1] = 0; out[2] = -sy; out[3] = 0;
  out[4] = sy * sp; out[5] = cp; out[6] = cy * sp; out[7] = 0;
  out[8] = sy * cp; out[9] = -sp; out[10] = cy * cp; out[11] = 0;
  out[12] = x; out[13] = y; out[14] = z; out[15] = 1;
  return out;
}

/** Writes a column-major transform (scale, then rotate about Y then X, then translate) into out. */
function compose(out, x, y, z, yaw, pitch, s) {
  const cy = Math.cos(yaw);
  const sy = Math.sin(yaw);
  const cp = Math.cos(pitch);
  const sp = Math.sin(pitch);
  // R = Ry(yaw) * Rx(pitch), columns scaled by s.
  out[0] = cy * s; out[1] = 0; out[2] = -sy * s; out[3] = 0;
  out[4] = sy * sp * s; out[5] = cp * s; out[6] = cy * sp * s; out[7] = 0;
  out[8] = sy * cp * s; out[9] = -sp * s; out[10] = cy * cp * s; out[11] = 0;
  out[12] = x; out[13] = y; out[14] = z; out[15] = 1;
  return out;
}

// ---- textures -------------------------------------------------------------------------------

function makeCanvas(size) {
  if (typeof OffscreenCanvas !== 'undefined') return new OffscreenCanvas(size, size);
  const c = document.createElement('canvas');
  c.width = size;
  c.height = size;
  return c;
}

/** A checker with a label and a gradient ring: shows orientation, filtering and sRGB decoding. */
function labelTexture() {
  const size = 512;
  const c = makeCanvas(size);
  const g = c.getContext('2d');
  const cells = 8;
  for (let y = 0; y < cells; y++) {
    for (let x = 0; x < cells; x++) {
      g.fillStyle = (x + y) % 2 ? '#e9e4d8' : '#2c6e8f';
      g.fillRect((x * size) / cells, (y * size) / cells, size / cells, size / cells);
    }
  }
  const ring = g.createLinearGradient(0, 0, size, 0);
  ring.addColorStop(0, '#000000');
  ring.addColorStop(1, '#ffffff');
  g.strokeStyle = ring;
  g.lineWidth = 28;
  g.beginPath();
  g.arc(size / 2, size / 2, size * 0.34, 0, Math.PI * 2);
  g.stroke();
  g.fillStyle = '#c4462b';
  g.fillRect(0, size * 0.4, size, size * 0.2);
  g.fillStyle = '#ffffff';
  g.font = 'bold 72px sans-serif';
  g.textAlign = 'center';
  g.textBaseline = 'middle';
  g.fillText('web3d ↑', size / 2, size / 2);
  return c;
}

/** A grille: an opaque panel with round holes, for the masked material. */
function grilleTexture() {
  const size = 256;
  const c = makeCanvas(size);
  const g = c.getContext('2d');
  g.fillStyle = '#b9a27a';
  g.fillRect(0, 0, size, size);
  g.fillStyle = '#8a7350';
  for (let i = 0; i < size; i += 16) g.fillRect(0, i, size, 4);
  g.globalCompositeOperation = 'destination-out';
  const step = size / 6;
  for (let y = 0; y < 6; y++) {
    for (let x = 0; x < 6; x++) {
      g.beginPath();
      g.arc((x + 0.5) * step, (y + 0.5) * step, step * 0.34, 0, Math.PI * 2);
      g.fill();
    }
  }
  return c;
}

// ---- measurement ----------------------------------------------------------------------------

const MAX_FRAMES = 16384;
const rec = {
  on: false,
  duration: 0,
  start: -1,
  prev: 0,
  n: 0,
  cpuN: 0,
  gpuN: 0,
  lastGpuFrame: -1,
  intervals: new Float64Array(MAX_FRAMES),
  cpu: new Float64Array(MAX_FRAMES),
  gpu: new Float64Array(MAX_FRAMES),
  summary: true,
  resolve: null,
};

function percentile(sorted, p) {
  if (sorted.length === 0) return null;
  const i = Math.min(sorted.length - 1, Math.max(0, Math.ceil(p * sorted.length) - 1));
  return sorted[i];
}

function describe(values) {
  const sorted = Float64Array.from(values).sort();
  if (sorted.length === 0) return null;
  const mean = sorted.reduce((a, b) => a + b, 0) / sorted.length;
  const round = (x) => Math.round(x * 1000) / 1000;
  return {
    n: sorted.length,
    mean: round(mean),
    p50: round(percentile(sorted, 0.5)),
    p95: round(percentile(sorted, 0.95)),
    max: round(sorted[sorted.length - 1]),
  };
}

// ---- main -----------------------------------------------------------------------------------

let resolveReady;
let rejectReady;
const ready = new Promise((resolve, reject) => {
  resolveReady = resolve;
  rejectReady = reject;
});
window.__web3d = { ready, measure: null, glError: null, error: null };

function drawingSize() {
  const dpr = window.devicePixelRatio || 1;
  // Floored, as three.js's setPixelRatio sizes its canvas (1366 x 0.75 gives 1024).
  return [
    Math.max(1, Math.floor(canvas.clientWidth * dpr * SCALE)),
    Math.max(1, Math.floor(canvas.clientHeight * dpr * SCALE)),
  ];
}

async function main() {
  const [width, height] = drawingSize();
  const r = await createRenderer(canvas, { width, height, antialias: params.get('aa') !== '0', cpuTiming: CPU_TIMING });

  const meshKnots = [
    r.createMesh(torusKnot(0.62, 0.19, TUBULAR, RADIAL, 2, 3)),
    r.createMesh(torusKnot(0.6, 0.17, TUBULAR, RADIAL, 3, 4)),
    r.createMesh(torusKnot(0.58, 0.16, TUBULAR, RADIAL, 2, 5)),
  ];
  // 58 knots of 192 x 14 x 2 = 5,376 triangles, a 38 x 37 ground, a 32 x 16 sphere, two 48 x 24
  // spheres and two panels: 64 draws and 320,000 triangles.
  const meshGround = r.createMesh(ground(26, 38, 37));
  const meshBall = r.createMesh(sphere(1.1, 32, 16));
  const meshSmallBall = r.createMesh(sphere(0.75, 48, 24));
  const meshPanel = r.createMesh(panel(2.6, 2.0));

  const opts = { premultiplyAlpha: 'none', colorSpaceConversion: 'none' };
  const texLabel = r.createTexture(await createImageBitmap(labelTexture(), opts));
  const texGrille = r.createTexture(await createImageBitmap(grilleTexture(), opts), { wrapS: 'clamp', wrapT: 'clamp' });

  const palette = [
    { baseColor: [0.62, 0.18, 0.09, 1], roughness: 0.7 },
    { baseColor: [0.05, 0.35, 0.38, 1], roughness: 0.45 },
    { baseColor: [0.75, 0.52, 0.08, 1], roughness: 0.5 },
    { baseColor: [0.18, 0.22, 0.3, 1], roughness: 0.35 },
    { baseColor: [0.8, 0.78, 0.7, 1], roughness: 0.6 },
    { baseColor: [1.0, 0.77, 0.34, 1], roughness: 0.3, metallic: 1 },
  ].map((m) => r.createMaterial(m));
  const matGround = r.createMaterial({ baseColor: [0.16, 0.16, 0.17, 1], roughness: 0.9 });
  const matLabel = r.createMaterial({ texture: texLabel, roughness: 0.5 });
  const matGold = r.createMaterial({ baseColor: [1.0, 0.77, 0.34, 1], roughness: 0.25, metallic: 1 });
  const matGloss = r.createMaterial({ baseColor: [0.7, 0.05, 0.08, 1], roughness: 0.15 });
  const matGrille = r.createMaterial({ texture: texGrille, alphaMode: 'mask', alphaCutoff: 0.5, doubleSided: true, roughness: 0.6 });
  const matGlass = r.createMaterial({ baseColor: [0.35, 0.6, 0.9, 0.35], alphaMode: 'blend', doubleSided: true, roughness: 0.1 });

  const m = ARRAYS ? [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1] : new Float32Array(16);
  let triangles = 0;
  let draws = 0;
  const place = (mesh, material, tris, x, y, z, yaw = 0, pitch = 0, s = 1) => {
    draws++;
    triangles += tris;
    return r.createObject(mesh, material, compose(m, x, y, z, yaw, pitch, s));
  };
  const knotTris = TUBULAR * RADIAL * 2;
  place(meshGround, matGround, 38 * 37 * 2, 0, 0, 0);
  const columns = 9;
  for (let i = 0; i < KNOTS; i++) {
    const col = i % columns;
    const rowIndex = Math.floor(i / columns);
    place(meshKnots[i % 3], palette[(i * 7) % palette.length], knotTris,
      (col - (columns - 1) / 2) * 2.3, 1.25 + 0.15 * Math.sin(i), 3.2 - rowIndex * 2.3, i * 0.7, 0.3 * Math.cos(i), 0.95);
  }
  const ball = place(meshBall, matLabel, 32 * 16 * 2 - 2 * 32, 0, 1.45, 6.2);
  place(meshSmallBall, matGold, 48 * 24 * 2 - 2 * 48, -3.1, 0.95, 6.4);
  place(meshSmallBall, matGloss, 48 * 24 * 2 - 2 * 48, 3.1, 0.95, 6.4);
  const grille = place(meshPanel, matGrille, 2, -5.6, 1.4, 5.6);
  const glass = place(meshPanel, matGlass, 2, 5.6, 1.4, 5.6);

  // Intensities in three.js r180 units: a white surface facing a light of intensity pi is white.
  r.setKeyLight(0.5, 1.0, 0.35, 1.0, 0.95, 0.85, 1.7 * Math.PI);
  r.setAmbient(0.3, 0.36, 0.48, 0.1, 0.085, 0.07, Math.PI);
  const CLEAR = [0.015, 0.018, 0.024];
  r.setClearColor(...CLEAR);

  const eye = ARRAYS ? [0, 0, 0] : new Float32Array(3);
  const target = ARRAYS ? [0, 1.0, -1.2] : new Float32Array([0, 1.0, -1.2]);
  const fov = (50 * Math.PI) / 180;
  const stats = new Float64Array(8);
  const meter = { frames: 0, since: 0, cpu: 0, cpuN: 0, gpu: 0, gpuN: 0, intervals: new Float64Array(512), n: 0, prev: 0 };
  const scratch = new Float64Array(512);

  window.addEventListener('resize', () => {
    const [w, h] = drawingSize();
    r.resize(w, h);
  });

  const info = r.info;
  const header = `web3d phase 1 · ${info.renderer}`;
  window.__web3d.glError = () => r.gl.getError();
  window.__web3d.clearColor = CLEAR;
  // Pixels must be read in the task that drew them (the drawing buffer is not preserved).
  let pixelRequest = null;
  window.__web3d.pixels = (points) =>
    new Promise((resolve) => {
      pixelRequest = { points, resolve };
    });
  const readPixels = (points) => {
    const gl = r.gl;
    const px = new Uint8Array(4);
    return points.map(([x, y]) => {
      gl.readPixels(x, canvas.height - 1 - y, 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, px);
      return [px[0], px[1], px[2]];
    });
  };
  window.__web3d.renderer = r;
  window.__web3d.scene = { draws, triangles };
  window.__web3d.frames = (ms) =>
    new Promise((resolve) => {
      rec.on = true;
      rec.summary = false;
      rec.duration = ms;
      rec.start = -1;
      rec.n = 0;
      rec.gpuN = 0;
      rec.cpuN = 0;
      rec.lastGpuFrame = -1;
      rec.resolve = resolve;
    });
  window.__web3d.measure = (ms) =>
    new Promise((resolve) => {
      rec.on = true;
      rec.summary = true;
      rec.duration = ms;
      rec.start = -1;
      rec.n = 0;
      rec.gpuN = 0;
      rec.cpuN = 0;
      rec.lastGpuFrame = -1;
      rec.resolve = resolve;
    });

  const finishRecording = () => {
    rec.on = false;
    if (!rec.summary) {
      rec.resolve(rec.n);
      return;
    }
    const seconds = (rec.prev - rec.start) / 1000;
    rec.resolve({
      seconds: Math.round(seconds * 1000) / 1000,
      frames: rec.n,
      fps: Math.round((rec.n / seconds) * 100) / 100,
      intervalMs: describe(rec.intervals.subarray(0, rec.n)),
      cpuMs: describe(rec.cpu.subarray(0, rec.cpuN)),
      gpuMs: describe(rec.gpu.subarray(0, rec.gpuN)),
      draws: stats[3],
      triangles: stats[4],
      culled: stats[5],
      programBinds: stats[6],
      materialBinds: stats[7],
      canvas: [canvas.width, canvas.height],
      antialias: info.antialias,
      samples: info.samples,
      renderer: info.renderer,
      timerQuery: info.timerQuery,
    });
  };

  /** Adds a frame to a window that measure() asked for. */
  const record = (now) => {
    if (rec.start < 0) {
      rec.start = now;
    } else if (rec.n < MAX_FRAMES) {
      rec.intervals[rec.n] = now - rec.prev;
      rec.n++;
      if (stats[0] === stats[0]) rec.cpu[rec.cpuN++] = stats[0];
    }
    rec.prev = now;
    if (stats[1] === stats[1] && stats[2] !== rec.lastGpuFrame && rec.gpuN < MAX_FRAMES) {
      rec.gpu[rec.gpuN++] = stats[1];
      rec.lastGpuFrame = stats[2];
    }
    if (rec.start >= 0 && now - rec.start >= rec.duration) finishRecording();
  };

  /** The on-screen readout: sums per frame, text once a second (the only allocation). */
  const readout = (now) => {
    if (meter.prev > 0) {
      meter.intervals[meter.n % 512] = now - meter.prev;
      meter.n++;
    }
    meter.prev = now;
    meter.frames++;
    if (stats[0] === stats[0]) {
      meter.cpu += stats[0];
      meter.cpuN++;
    }
    if (stats[1] === stats[1]) {
      meter.gpu += stats[1];
      meter.gpuN++;
    }
    if (meter.since === 0) meter.since = now;
    if (now - meter.since >= 1000 && meter.n > 0) showReadout(now);
  };

  const showReadout = (now) => {
    const elapsed = now - meter.since;
    const n = Math.min(meter.n, 512);
    scratch.set(meter.intervals.subarray(0, n));
    const sorted = scratch.subarray(0, n).sort();
    const fps = (meter.frames * 1000) / elapsed;
    const gpu = meter.gpuN ? `${(meter.gpu / meter.gpuN).toFixed(2)} ms` : 'n/a';
    hud.textContent = [
      header,
      `${fps.toFixed(1)} fps   frame p50 ${percentile(sorted, 0.5).toFixed(1)} / p95 ${percentile(sorted, 0.95).toFixed(1)} ms`,
      `GPU ${gpu}   CPU ${meter.cpuN ? `${(meter.cpu / meter.cpuN).toFixed(2)} ms` : 'n/a'}`,
      `${stats[3]} draws   ${(stats[4] / 1000).toFixed(0)}k tris   ${stats[5]} culled   ${canvas.width}\u00d7${canvas.height}   ${info.antialias ? `MSAA ${info.samples}x` : 'no MSAA'}`,
    ].join('\n');
    meter.frames = 0;
    meter.since = now;
    meter.cpu = 0;
    meter.cpuN = 0;
    meter.gpu = 0;
    meter.gpuN = 0;
    meter.n = 0;
  };

  // Seconds since the start, in a typed array. The per-frame code reads it where it needs it and
  // passes only constants as arguments: a computed number passed to a call is boxed (allocated)
  // by V8 until it optimises the caller fully.
  const clock = new Float64Array(1);
  const spin = (object, x, y, z, yaw, rate, pitch) => {
    r.setTransform(object, composeAt(m, clock, x, y, z, yaw, rate, pitch));
  };
  const orbit = () => {
    const a = 0.6 + clock[0] * 0.12;
    eye[0] = Math.sin(a) * 15;
    eye[1] = 5.5;
    eye[2] = Math.cos(a) * 15 - 1.2;
  };

  // The positive control: a short-lived allocation of a known size each frame, held only until the
  // next, so only a profiler that counts objects already collected sees it.
  const control = { on: false, last: null };
  window.__web3d.control = (on) => {
    control.on = Boolean(on);
    control.last = null;
  };

  const start = performance.now();
  let first = true;
  const tick = (now) => {
    requestAnimationFrame(tick);
    clock[0] = STILL ? 0 : (now - start) / 1000;
    orbit();
    r.setCameraLookAt(eye, target, fov, 0.1, 80);
    spin(ball, 0, 1.45, 6.2, 0, 0.6, 0.25);
    spin(grille, -5.6, 1.4, 5.6, 0.5, 0.35, 0);
    spin(glass, 5.6, 1.4, 5.6, -0.5, -0.3, 0);
    r.frame();
    r.readStats(stats);
    if (pixelRequest) {
      const { points, resolve } = pixelRequest;
      pixelRequest = null;
      resolve(readPixels(points));
    }

    if (first) {
      first = false;
      resolveReady(info);
    }
    if (control.on) control.last = [clock[0], 1, 2, 3, 4, 5, 6, 7];
    if (rec.on) record(now);
    if (HUD) readout(now);
  };
  requestAnimationFrame(tick);
}

main().catch((error) => {
  hud.className = 'error';
  hud.textContent = String(error?.stack ?? error);
  window.__web3d.error = String(error);
  rejectReady(error);
});
