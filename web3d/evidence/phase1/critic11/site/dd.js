import * as web3d from '/pkg/web3d.js';
import { sleep, until, frame, err, tryCreate, drawQuad } from './common.js';
const out = { top: location.origin };
const f = await frame(`http://b.a.localhost:${location.port}/evidence/phase1/critic11/site/dd-frame.html`);
const fw = f.contentWindow;
await until(() => { try { return fw.ready; } catch { return false; } });
out.frameOrigin = fw.location.origin;
out.firstHomeIsFrameWindow = fw.firstHomeIsSelf;
out.topHasState = window[Symbol.for('loadngo-web3d')] !== undefined;
// One canvas of the frame's own document, two copies of web3d in the frame's realm.
const c = fw.document.createElement('canvas');
fw.document.body.appendChild(c);
const a = await tryCreate(fw.first, c);
out.copy1Create = a.ok ? 'accepted' : a.error;
const b = await tryCreate(fw.second, c);
out.copy2CreateWhileCopy1Live = b.ok ? 'accepted (two live renderers on one canvas)' : b.error;
if (a.ok && b.ok) {
  out.sameContext = a.r.gl === b.r.gl;
  // Each draws; the second's draw follows state the first set, and the reverse.
  const da = drawQuad(a.r, [1, 0, 0]);
  const db = drawQuad(b.r, [0, 0, 1]);
  out.draws = { first: { drew: da.drew, centre: da.centre, error: da.error }, second: { drew: db.drew, centre: db.centre, error: db.error } };
  // The first again, after the second has used the context.
  a.r.frame();
  const px = new Uint8Array(4); a.r.gl.readPixels(32, 32, 1, 1, a.r.gl.RGBA, a.r.gl.UNSIGNED_BYTE, px);
  out.firstAgainCentre = [...px];
  out.errorAfter = a.r.gl.getError();
  // Listeners now on the canvas (two controllers?) and what a loss does.
  let hooks = 0;
  const lose = a.r.gl.getExtension('WEBGL_lose_context');
  lose.loseContext(); await sleep(150);
  lose.restoreContext(); await sleep(300);
  out.restoredAfterLoss = !a.r.gl.isContextLost();
}
if (a.ok) a.r.dispose();
if (b.ok) b.r.dispose();
f.remove();
window.__done = Promise.resolve(out);
