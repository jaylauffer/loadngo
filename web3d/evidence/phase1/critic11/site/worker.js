import * as web3d from '/pkg/web3d.js';
let kept = null;
const err = (e) => String(e?.message ?? e).slice(0, 160);
onmessage = async (e) => {
  const { op } = e.data;
  try {
    if (op === 'own') {
      const c = new OffscreenCanvas(64, 64);
      const r = await web3d.createRenderer(c, { antialias: false });
      const ok = r.frame();
      r.dispose();
      postMessage({ ok, homeIsSelf: self[Symbol.for('loadngo-web3d')] !== undefined });
    } else if (op === 'take') {
      const r = await web3d.createRenderer(e.data.canvas, { width: 64, height: 64, antialias: false });
      postMessage({ ok: r.frame() });
      r.dispose();
    } else if (op === 'giveBack') {
      kept = new OffscreenCanvas(32, 32);
      postMessage({ canvas: kept }, [kept]);
    } else postMessage({ op: 'unknown' });
  } catch (x) {
    postMessage({ error: err(x) });
  }
};
