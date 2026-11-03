// Drives the shipped wasm with stub GL imports: morph-count overflow on wasm32, and a null renderer pointer.
import { readFile } from 'node:fs/promises';
const bytes = await readFile('/Users/jay/pudding/loadngo/web3d/pkg/loadngo_web3d.wasm');
let next = 1; const id = () => next++;
const logs = [];
const gl = new Proxy({}, { get: (_, name) => {
  switch (name) {
    case 'get_integer': return (p) => ({ 0x0D33: 4096, 0x8A34: 256, 0x8A30: 16384 })[p] ?? 0;
    case 'timer_query_supported': return () => 0;
    case 'program_check': case 'program_bind_block': case 'program_bind_sampler': return () => 1;
    case 'program_block_size': return () => 0;
    case 'program_uniform_offset': return () => -1;
    case 'program_create': case 'buffer_create': case 'buffer_alloc': case 'vertex_array_create': case 'texture_create_staged': case 'query_create': return () => id();
    case 'query_result_ns': return () => 0;
    case 'log': return (l, p, n) => logs.push(new TextDecoder().decode(new Uint8Array(mem.buffer, p, n)));
    default: return () => 0;
  }
} });
const { instance } = await WebAssembly.instantiate(bytes, { web3d_gl: gl });
const x = instance.exports; const mem = x.memory;
const err = (w) => new TextDecoder().decode(new Uint8Array(mem.buffer, x.web3d_error_ptr(w) >>> 0, x.web3d_error_len(w) >>> 0));
const w = x.web3d_create();
console.log('init', x.web3d_init(w, 64, 64), err(w));
const stageF32 = (a) => { const p = x.web3d_alloc(a.length * 4) >>> 0; new Float32Array(mem.buffer, p, a.length).set(a); return p; };
const stageU32 = (a) => { const p = x.web3d_alloc(a.length * 4) >>> 0; new Uint32Array(mem.buffer, p, a.length).set(a); return p; };
const pos = stageF32([0, 0, 0, 1, 0, 0, 0, 1, 0]), idx = stageU32([0, 1, 2]), deltas = stageF32([0, 0, 0, 0, 0, 0]);
const mesh = (targets) => { const h = x.web3d_mesh_create(w, pos, 9, 0, 0, 0, 0, idx, 3, 0, 0, 0, 0, deltas, 6, targets); return h ? `handle ${h}` : `refused: ${err(w)}`; };
console.log('3 vertices, 6 deltas, 2 targets          ->', mesh(2));
console.log('3 vertices, 6 deltas, 0x55555556 targets ->', mesh(0x55555556), '(3 * 9 * 0x55555556 wraps to 6 on wasm32)');
// Null renderer pointer, as web3d.js passes after dispose(): no trap, a phantom renderer at address 0.
console.log('web3d_frame(0)                 ->', (() => { try { x.web3d_frame(0); return 'returned normally'; } catch (e) { return `trapped: ${e.message}`; } })());
console.log('web3d_material_create(0, ...) ->', (() => { try { const h = x.web3d_material_create(0, 0, 0, 0); return `returned ${h}, error at address 0 says: "${err(0)}"`; } catch (e) { return `trapped: ${e.message}`; } })());
const low = new Uint8Array(mem.buffer, 0, 4096); let nz = 0; for (const b of low) if (b) nz++;
console.log('non-zero bytes written into the first 4 KiB of linear memory (stack bottom):', nz);
// Handle aliasing: 10 generation bits, a LIFO free list. Create/destroy one material 1024 times.
const h0 = x.web3d_material_create(w, 0, 0, 0);
x.web3d_material_destroy(w, h0);
for (let i = 1; i < 1024; i++) x.web3d_material_destroy(w, x.web3d_material_create(w, 0, 0, 0));
const h1024 = x.web3d_material_create(w, 0, 0, 0);
console.log(`material handle before: ${h0}, after 1024 create/destroy cycles: ${h1024}; stale ${h0} now destroys the new material ->`, x.web3d_material_destroy(w, h0) === 0 ? 'accepted (aliased)' : `refused: ${err(w)}`);
