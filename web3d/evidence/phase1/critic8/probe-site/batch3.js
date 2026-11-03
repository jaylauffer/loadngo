// Eighth critic's probes, batch 3: what a 'bitmaprenderer' canvas smaller than its bitmap uploads.
// The bitmap's left half is green and its right half red; its top half bright and bottom half dim.
import { createRenderer } from '/web3d/pkg/web3d.js';

const out = { probes: [] };
window.__probe8 = { done: null, out };
const log = (id, claim, pass, detail) => out.probes.push({ id, claim, pass: Boolean(pass), detail });
const IDENT = new Float32Array([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1]);
const ORTHO = new Float32Array([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, -2 / 9.9, 0, 0, 0, -10.1 / 9.9, 1]);
const quad = {
  positions: new Float32Array([-1, -1, -5, 1, -1, -5, 1, 1, -5, -1, 1, -5]),
  normals: new Float32Array([0, 0, 1, 0, 0, 1, 0, 0, 1, 0, 0, 1]),
  uvs: new Float32Array([0, 1, 1, 1, 1, 0, 0, 0]),
  indices: new Uint32Array([0, 1, 2, 0, 2, 3]),
};
const pattern = (w, h) => {
  const d = new Uint8ClampedArray(w * h * 4);
  for (let y = 0; y < h; y++) for (let x = 0; x < w; x++) {
    const v = y < h / 2 ? 255 : 120;
    d.set(x < w / 2 ? [0, v, 0, 255] : [v, 0, 0, 255], (y * w + x) * 4);
  }
  return createImageBitmap(new ImageData(d, w, h), { premultiplyAlpha: 'none', colorSpaceConversion: 'none' });
};

async function main() {
  const c = document.createElement('canvas');
  document.body.append(c);
  const r = await createRenderer(c, { width: 16, height: 16, antialias: false });
  r.setCamera(IDENT, ORTHO);
  r.setKeyLight(0, 0, 1, 0, 0, 0, 0);
  r.setAmbient(1, 1, 1, 1, 1, 1, Math.PI);
  const px = (nx, ny) => {
    const p = new Uint8Array(4);
    r.gl.readPixels(Math.floor(((nx + 1) / 2) * 16), Math.floor(((ny + 1) / 2) * 16), 1, 1, r.gl.RGBA, r.gl.UNSIGNED_BYTE, p);
    return [p[0], p[1], p[2]];
  };
  const d = {};
  for (const [name, offscreen, cw, ch, bw, bh] of [
    ['element 4x4, bitmap 8x8', false, 4, 4, 8, 8],
    ['offscreen 4x4, bitmap 8x8', true, 4, 4, 8, 8],
    ['element 4x8, bitmap 8x8', false, 4, 8, 8, 8],
    ['element 8x8, bitmap 8x8 (control)', false, 8, 8, 8, 8],
  ]) {
    const src = offscreen ? new OffscreenCanvas(cw, ch) : Object.assign(document.createElement('canvas'), { width: cw, height: ch });
    src.getContext('bitmaprenderer').transferFromImageBitmap(await pattern(bw, bh));
    let made;
    try {
      made = r.createTexture(src, { srgb: false, mipmaps: false, wrapS: 'clamp', wrapT: 'clamp' });
    } catch (e) {
      d[name] = { refused: String(e.message).slice(0, 80) };
      continue;
    }
    const o = r.createObject(r.createMesh(quad), r.createMaterial({ texture: made }));
    r.frame();
    // Quadrants of the drawn texture (texture v = 0 is the image's first row, drawn at the top).
    d[name] = { reported: [src.width, src.height], leftTop: px(-0.5, 0.5), rightTop: px(0.5, 0.5), leftBottom: px(-0.5, -0.5), rightBottom: px(0.5, -0.5) };
    r.destroyObject(o);
  }
  r.dispose();
  const ctl = d['element 8x8, bitmap 8x8 (control)'];
  // The whole image shows when the right half is red and the bottom half dimmer, as in the control.
  const whole = (q) => q && q.rightTop[0] > 150 && q.leftTop[1] > 150 && q.leftBottom[1] < q.leftTop[1];
  log('E16', "a 'bitmaprenderer' canvas smaller than its bitmap: the texture holds the whole image (scaled), not a crop of its corner", whole(d['element 4x4, bitmap 8x8']) && whole(d['offscreen 4x4, bitmap 8x8']), d);
  log('E16c', 'control: an 8x8 canvas with its own 8x8 bitmap shows the whole image', whole(ctl), ctl);
  out.passed = out.probes.filter((p) => p.pass).length;
  return out;
}
window.__probe8.done = main().catch((e) => ({ probes: [{ id: 'E16', claim: 'threw', pass: false, detail: String(e.stack) }] }));
