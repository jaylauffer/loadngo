// An application's per-frame path, as a three.js scene drives it: plain-Array matrices
// (Matrix4.elements) mutated in place, setCamera and setTransform each frame, then frame() and readStats().
import { createRenderer } from '/web3d/pkg/web3d.js';
const canvas = document.getElementById('c');
const r = await createRenderer(canvas, { width: 1024, height: 576 });
const quad = { positions: new Float32Array([-1,-1,0, 1,-1,0, 1,1,0, -1,1,0]), normals: new Float32Array([0,0,1, 0,0,1, 0,0,1, 0,0,1]), uvs: new Float32Array([0,1, 1,1, 1,0, 0,0]), indices: new Uint32Array([0,1,2, 0,2,3]) };
const mesh = r.createMesh(quad);
const mat = r.createMaterial({ baseColor: [0.6, 0.4, 0.3, 1] });
const objs = [];
for (let i = 0; i < 60; i++) objs.push(r.createObject(mesh, mat, [1,0,0,0, 0,1,0,0, 0,0,1,0, (i % 10) - 5, Math.floor(i / 10) - 3, -10, 1]));
// three.js-style matrices: plain Arrays of 16 numbers, mutated in place.
const view = [1,0,0,0, 0,1,0,0, 0,0,1,0, 0,0,0,1];
const proj = [1.2,0,0,0, 0,2.1,0,0, 0,0,-1.002,-1, 0,0,-0.2002,0];
const model = [1,0,0,0, 0,1,0,0, 0,0,1,0, 0,0,-10,1];
const stats = new Float64Array(8);
const clock = new Float64Array(1);
let frames = 0;
window.__game = { frames: () => frames, renderer: r.info.renderer };
const t0 = performance.now();
const tick = (now) => {
  requestAnimationFrame(tick);
  clock[0] = (now - t0) / 1000;
  view[12] = Math.sin(clock[0]) * 0.5;
  model[12] = Math.cos(clock[0]) * 2;
  r.setCamera(view, proj);
  r.setTransform(objs[0], model);
  r.frame();
  r.readStats(stats);
  frames++;
};
requestAnimationFrame(tick);
