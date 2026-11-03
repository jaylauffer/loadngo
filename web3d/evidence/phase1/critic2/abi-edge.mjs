// Critic's own raw-ABI probe: drives pkg/loadngo_web3d.wasm in Node with stub GL imports that record calls.
import { readFile } from 'node:fs/promises';

const bytes = await readFile(new URL('../../../pkg/loadngo_web3d.wasm', import.meta.url));
let next = 1;
const id = () => next++;
const calls = [];
let mem;
const dec = new TextDecoder();
let record = true;
const gl = new Proxy(
  {},
  {
    get: (_, name) => {
      const rec = (ret) => (...a) => {
        if (record) calls.push([name, ...a]);
        return typeof ret === 'function' ? ret(...a) : ret;
      };
      switch (name) {
        case 'get_integer':
          return rec((p) => ({ 0x0d33: 4096, 0x8a34: 256, 0x8a30: 16384 })[p] ?? 0);
        case 'timer_query_supported':
          return rec(1);
        case 'program_check':
        case 'program_bind_block':
        case 'program_bind_sampler':
          return rec(1);
        case 'program_block_size':
          return rec(0);
        case 'program_uniform_offset':
          return rec(-1);
        case 'program_create':
        case 'buffer_create':
        case 'buffer_alloc':
        case 'vertex_array_create':
        case 'texture_create_staged':
        case 'query_create':
          return rec(() => id());
        case 'query_available':
          return rec(1);
        case 'query_result_ns':
          return rec(2.5e6);
        case 'gpu_disjoint':
          return rec(0);
        case 'now':
          return rec(() => performance.now());
        case 'log':
          return (l, p, n) => console.log('LOG', l, dec.decode(new Uint8Array(mem.buffer, p, n)));
        default:
          return rec(undefined);
      }
    },
  },
);
const { instance } = await WebAssembly.instantiate(bytes, { web3d_gl: gl });
const x = instance.exports;
mem = x.memory;
console.log('exports:', Object.keys(x).join(' '));
const err = (w) => dec.decode(new Uint8Array(mem.buffer, x.web3d_error_ptr(w) >>> 0, x.web3d_error_len(w) >>> 0));
const low = () => new Uint8Array(mem.buffer, 0, 1024).slice();
const try_ = (label, f) => {
  try {
    const v = f();
    return `${label}: ${v}`;
  } catch (e) {
    return `${label}: TRAP ${e.message}`;
  }
};

// 1. Every export with a null renderer pointer.
const before = low();
const callsBefore = calls.length;
const nulls = [
  try_('init', () => x.web3d_init(0, 64, 64)),
  try_('error_ptr/len', () => `${x.web3d_error_ptr(0) >>> 0}/${x.web3d_error_len(0)} "${err(0)}"`),
  try_('io', () => x.web3d_io(0)),
  try_('stats', () => x.web3d_stats(0)),
  try_('caps', () => x.web3d_caps(0)),
  try_('set_cpu_timing', () => x.web3d_set_cpu_timing(0, 1)),
  try_('resize', () => x.web3d_resize(0, 10, 10)),
  try_('mesh_create', () => x.web3d_mesh_create(0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0)),
  try_('mesh_destroy', () => x.web3d_mesh_destroy(0, 1)),
  try_('texture_create', () => x.web3d_texture_create(0, 4, 4, 3)),
  try_('texture_destroy', () => x.web3d_texture_destroy(0, 1)),
  try_('material_create', () => x.web3d_material_create(0, 0, 0, 0)),
  try_('material_create bad mode', () => x.web3d_material_create(0, 0, 9, 0)),
  try_('material_destroy', () => x.web3d_material_destroy(0, 1)),
  try_('object_create', () => x.web3d_object_create(0, 1, 1)),
  try_('object_set_transform', () => x.web3d_object_set_transform(0, 1)),
  try_('object_set_visible', () => x.web3d_object_set_visible(0, 1, 0)),
  try_('object_destroy', () => x.web3d_object_destroy(0, 1)),
  try_('set_camera', () => x.web3d_set_camera(0)),
  try_('set_camera_look_at', () => x.web3d_set_camera_look_at(0)),
  try_('set_key_light', () => x.web3d_set_key_light(0, 0, 1, 0, 1, 1, 1, 1)),
  try_('set_ambient', () => x.web3d_set_ambient(0, 1, 1, 1, 1, 1, 1, 1)),
  try_('set_clear_color', () => x.web3d_set_clear_color(0, 1, 1, 1)),
  try_('frame', () => x.web3d_frame(0)),
  try_('destroy', () => x.web3d_destroy(0)),
  try_('free', () => x.web3d_free(0, 16)),
];
const after = low();
let changed = 0;
for (let i = 0; i < 1024; i++) if (before[i] !== after[i]) changed++;
console.log('NULL POINTER:\n  ' + nulls.join('\n  '));
console.log(`  low 1 KiB bytes changed: ${changed}; GL calls made: ${calls.length - callsBefore}`);

// 2. A real renderer.
const w = x.web3d_create();
console.log('init', x.web3d_init(w, 64, 64), JSON.stringify(err(w)));
const io = () => new Float32Array(mem.buffer, x.web3d_io(w) >>> 0, 64);
const stage = (Kind, arr) => {
  const p = x.web3d_alloc(arr.length * Kind.BYTES_PER_ELEMENT) >>> 0;
  new Kind(mem.buffer, p, arr.length).set(arr);
  return p;
};
const pos = stage(Float32Array, [0, 0, 0, 1, 0, 0, 0, 1, 0]);
const idx = stage(Uint32Array, [0, 1, 2]);
const mesh = x.web3d_mesh_create(w, pos, 9, 0, 0, 0, 0, idx, 3, 0, 0, 0, 0, 0, 0, 0);
console.log('mesh', mesh, err(w));

// 3. Handle churn: materials and objects, 3,000 cycles each; no handle reissued, the first stays refused.
io().set([1, 1, 1, 1, 0.5, 1, 0]);
for (const [kind, create, destroy] of [
  ['material', () => x.web3d_material_create(w, 0, 0, 0), (h) => x.web3d_material_destroy(w, h)],
  [
    'object',
    () => {
      io().set([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1]);
      return x.web3d_object_create(w, mesh, keepMat);
    },
    (h) => x.web3d_object_destroy(w, h),
  ],
]) {
  var keepMat = keepMat ?? x.web3d_material_create(w, 0, 0, 0);
  const seen = new Set();
  const first = create();
  seen.add(first);
  destroy(first);
  let dup = 0;
  let maxh = 0;
  for (let i = 0; i < 3000; i++) {
    const h = create() >>> 0;
    if (h === 0) {
      console.log(kind, 'create failed', err(w));
      break;
    }
    if (seen.has(h)) dup++;
    seen.add(h);
    maxh = Math.max(maxh, h);
    destroy(h);
  }
  const fresh = create();
  const stale = destroy(first);
  console.log(`${kind}: first ${first}, 3,001 cycles later new ${fresh}; duplicates ${dup}; max handle ${maxh} (< 2^30: ${maxh < 2 ** 30}); stale first -> ${stale === 0 ? 'ACCEPTED (aliased)' : 'refused: ' + err(w)}`);
  destroy(fresh);
}

// 4. Edge inputs.
const edges = [];
const E = (label, f) => edges.push(try_(label, () => {
  const r = f();
  return `${r} ${r === 0 || r === 1 ? JSON.stringify(err(w)) : ''}`;
}));
E('mesh: 0 positions', () => x.web3d_mesh_create(w, pos, 0, 0, 0, 0, 0, idx, 3, 0, 0, 0, 0, 0, 0, 0));
E('mesh: null positions', () => x.web3d_mesh_create(w, 0, 9, 0, 0, 0, 0, idx, 3, 0, 0, 0, 0, 0, 0, 0));
E('mesh: null indices', () => x.web3d_mesh_create(w, pos, 9, 0, 0, 0, 0, 0, 3, 0, 0, 0, 0, 0, 0, 0));
E('mesh: morph targets 0x55555556 with 6 deltas', () => x.web3d_mesh_create(w, pos, 9, 0, 0, 0, 0, idx, 3, 0, 0, 0, 0, stage(Float32Array, new Array(6).fill(0)), 6, 0x55555556));
E('mesh: morph targets usize::MAX', () => x.web3d_mesh_create(w, pos, 9, 0, 0, 0, 0, idx, 3, 0, 0, 0, 0, stage(Float32Array, new Array(9).fill(0)), 9, -1));
E('mesh: morph count without deltas', () => x.web3d_mesh_create(w, pos, 9, 0, 0, 0, 0, idx, 3, 0, 0, 0, 0, 0, 0, 2));
E('mesh: infinite morph delta', () => x.web3d_mesh_create(w, pos, 9, 0, 0, 0, 0, idx, 3, 0, 0, 0, 0, stage(Float32Array, [0, 0, 0, Infinity, 0, 0, 0, 0, 0]), 9, 1));
const joints = stage(Uint16Array, new Array(12).fill(0));
E('mesh: weights 1.75 each', () => x.web3d_mesh_create(w, pos, 9, 0, 0, 0, 0, idx, 3, joints, 12, stage(Float32Array, new Array(12).fill(1.75)), 12, 0, 0, 0));
E('mesh: weights NaN', () => x.web3d_mesh_create(w, pos, 9, 0, 0, 0, 0, idx, 3, joints, 12, stage(Float32Array, [NaN, 0.5, 0.25, 0.25, 1, 0, 0, 0, 1, 0, 0, 0]), 12, 0, 0, 0));
E('mesh: weights all zero (unskinned vertex)', () => x.web3d_mesh_create(w, pos, 9, 0, 0, 0, 0, idx, 3, joints, 12, stage(Float32Array, [0, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0]), 12, 0, 0, 0));
E('mesh: weights sum 1.019', () => x.web3d_mesh_create(w, pos, 9, 0, 0, 0, 0, idx, 3, joints, 12, stage(Float32Array, [0.5, 0.519, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0]), 12, 0, 0, 0));
E('mesh: joints without weights', () => x.web3d_mesh_create(w, pos, 9, 0, 0, 0, 0, idx, 3, joints, 12, 0, 0, 0, 0, 0));
E('mesh: index u32::MAX', () => x.web3d_mesh_create(w, pos, 9, 0, 0, 0, 0, stage(Uint32Array, [0, 1, 0xffffffff]), 3, 0, 0, 0, 0, 0, 0, 0));
E('mesh: NaN normal (accepted? packs as +Y)', () => x.web3d_mesh_create(w, pos, 9, stage(Float32Array, [NaN, 0, 0, 0, 0, 1, 0, 0, 1]), 9, 0, 0, idx, 3, 0, 0, 0, 0, 0, 0, 0));
E('mesh: NaN uv', () => x.web3d_mesh_create(w, pos, 9, 0, 0, stage(Float32Array, [0, 0, NaN, 0, 0, 1]), 6, idx, 3, 0, 0, 0, 0, 0, 0, 0));
E('texture: 0x0', () => x.web3d_texture_create(w, 0, 0, 3));
E('texture: 4097 wide', () => x.web3d_texture_create(w, 4097, 4, 3));
E('texture: wrap bits 3 (invalid) -> ?', () => x.web3d_texture_create(w, 4, 4, 0b111111));
E('material: alpha mode 3', () => x.web3d_material_create(w, 0, 3, 0));
E('material: stale texture handle 12345', () => x.web3d_material_create(w, 12345, 0, 0));
io().set([NaN, 1, 1, 1, 0.5, 1, 0]);
E('material: NaN base colour', () => x.web3d_material_create(w, 0, 0, 0));
io().set([1, 1, 1, 1, 0.5, 1, 0]);
E('object: mesh handle 0', () => x.web3d_object_create(w, 0, 1));
E('object: handle u32::MAX', () => x.web3d_object_set_transform(w, -1));
io().fill(0);
E('camera: zero view matrix', () => x.web3d_set_camera(w));
io().set([0, 0, 5, 0, 0, 5, 0, 1, 0, 1, 0.1, 100]);
E('look-at: eye == target', () => x.web3d_set_camera_look_at(w));
io().set([0, 0, 5, 0, 0, 0, 0, 1, 0, 0, 0.1, 100]);
E('look-at: fov 0', () => x.web3d_set_camera_look_at(w));
io().set([0, 0, 5, 0, 0, 0, 0, 1, 0, 1, 0, 100]);
E('look-at: near 0', () => x.web3d_set_camera_look_at(w));
io().set([0, 0, 5, 0, 0, 0, 0, 0, 0, 1, 0.1, 100]);
E('look-at: up zero', () => x.web3d_set_camera_look_at(w));
E('key light: zero direction', () => x.web3d_set_key_light(w, 0, 0, 0, 1, 1, 1, 1));
E('key light: infinite intensity', () => x.web3d_set_key_light(w, 0, 1, 0, 1, 1, 1, Infinity));
E('key light: negative intensity', () => x.web3d_set_key_light(w, 0, 1, 0, 1, 1, 1, -3));
E('ambient: NaN', () => x.web3d_set_ambient(w, NaN, 1, 1, 1, 1, 1, 1));
E('resize to 0x0 then frame', () => (x.web3d_resize(w, 0, 0), x.web3d_frame(w), 'ok'));
E('resize to 2^31+ (u32 > i32)', () => {
  const n = calls.length;
  x.web3d_resize(w, 0x80000001, 4);
  x.web3d_frame(w);
  const vp = calls.slice(n).find((c) => c[0] === 'viewport');
  x.web3d_resize(w, 64, 64);
  return `viewport import received ${JSON.stringify(vp)}`;
});
console.log('EDGES:\n  ' + edges.join('\n  '));

// 5. Stats after frames, with CPU timing off and on.
x.web3d_resize(w, 64, 64);
io().set([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, -3, 1]);
const obj = x.web3d_object_create(w, mesh, keepMat);
for (let i = 0; i < 5; i++) x.web3d_frame(w);
const st = () => Array.from(new Float64Array(mem.buffer, x.web3d_stats(w) >>> 0, 8));
console.log('stats, cpu timing off:', JSON.stringify(st()));
const nowCalls = () => calls.filter((c) => c[0] === 'now').length;
const n0 = nowCalls();
x.web3d_frame(w);
console.log('clock readings in a frame with timing off:', nowCalls() - n0);
x.web3d_set_cpu_timing(w, 1);
const n1 = nowCalls();
x.web3d_frame(w);
console.log('clock readings in a frame with timing on:', nowCalls() - n1, 'stats', JSON.stringify(st()));

// 6. Destroy releases; count deletes.
const d0 = calls.length;
x.web3d_destroy(w);
const dels = calls.slice(d0).reduce((m, c) => ((m[c[0]] = (m[c[0]] ?? 0) + 1), m), {});
console.log('destroy made:', JSON.stringify(dels));
