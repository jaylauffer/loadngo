// A module worker for probe7.js: makes a renderer on an OffscreenCanvas, draws green, reads it back,
// and reports its serial, a handle and where its shared state lives.
import { createRenderer } from '/web3d/pkg/web3d.js';

const KEY = Symbol.for('loadngo-web3d');
onmessage = async () => {
  const report = {};
  try {
    const canvas = new OffscreenCanvas(16, 16);
    const r = await createRenderer(canvas, { width: 16, height: 16, antialias: false });
    r.setCamera(new Float32Array([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1]), new Float32Array([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, -2 / 9.9, 0, 0, 0, -10.1 / 9.9, 1]));
    r.setKeyLight(0, 0, 1, 0, 0, 0, 0);
    r.setAmbient(1, 1, 1, 1, 1, 1, Math.PI);
    const mesh = r.createMesh({
      positions: new Float32Array([-1, -1, -5, 1, -1, -5, 1, 1, -5, -1, 1, -5]),
      normals: new Float32Array([0, 0, 1, 0, 0, 1, 0, 0, 1, 0, 0, 1]),
      indices: new Uint32Array([0, 1, 2, 0, 2, 3]),
    });
    report.object = r.createObject(mesh, r.createMaterial({ baseColor: [0, 1, 0, 1] }));
    r.frame();
    const px = new Uint8Array(4);
    r.gl.readPixels(8, 8, 1, 1, r.gl.RGBA, r.gl.UNSIGNED_BYTE, px);
    report.pixel = [...px];
    report.serial = r._serial;
    report.ownShared = self[KEY] !== undefined;
    report.made = self[KEY]?.serials?.made;
    r.dispose();
  } catch (e) {
    report.error = String(e?.message ?? e);
  }
  postMessage(report);
};
