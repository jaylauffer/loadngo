// F2 (the eleventh critic): two copies of web3d in one realm, loaded either side of document.domain,
// keep one home: one owner map (the second's create on the first's live canvas is refused), one
// serial counter (their serials follow one another), and one loss controller a canvas (two
// listeners, not four). The top's own copy has its own home, as before.
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const err = (e) => String(e?.message ?? e).slice(0, 120);
const opts = { width: 16, height: 16, antialias: false };
const settle = (p) => p.then((r) => ({ ok: true, r }), (e) => ({ ok: false, message: err(e) }));
const out = { top: location.origin };
const f = document.createElement('iframe');
f.src = `http://b.a.localhost:${location.port}/evidence/phase1/critic11-rerun/site/dd-frame.html`;
document.body.append(f);
await new Promise((r) => (f.onload = r));
for (let i = 0; i < 200; i++) { try { if (f.contentWindow.ready) break; } catch {} await sleep(10); }
const fw = f.contentWindow;
out.frameOrigin = fw.location.origin;
out.topReachableAfter = fw.topReachableAfter;
const key = Symbol.for('loadngo-web3d');
out.realmHomeIsTheFrame = fw[key]?.home === fw;
out.topStateMadeByTheFrame = key in window;
const c = fw.document.createElement('canvas');
fw.document.body.append(c);
const a = await settle(fw.first.createRenderer(c, opts));
out.copy1 = a.ok ? 'created' : a.message;
const b = await settle(fw.second.createRenderer(c, opts));
out.copy2WhileCopy1Live = b.ok ? 'created (two live renderers on one canvas)' : b.message;
if (b.ok) b.r.dispose();
out.serial1 = a.r?._serial;
if (a.ok) a.r.dispose();
const b2 = await settle(fw.second.createRenderer(c, opts));
out.copy2AfterCopy1Disposed = b2.ok ? 'created' : b2.message;
out.serial2 = b2.r?._serial;
out.serialsInSequence = out.serial2 === (out.serial1 % (2 ** 20 - 1)) + 1;
// Listeners on the canvas: one controller (two listeners), though two copies drove it.
window.__targets = { c };
out.listeners = typeof window.__listeners === 'function' ? await window.__listeners('@c') : 'no driver';
if (b2.ok) b2.r.dispose();
f.remove();
out.pass = out.copy1 === 'created' && /already has a live renderer/.test(out.copy2WhileCopy1Live) && out.copy2AfterCopy1Disposed === 'created' && out.serialsInSequence && out.realmHomeIsTheFrame && out.listeners?.webglcontextlost === 1 && out.listeners?.webglcontextrestored === 1;
window.__done = Promise.resolve(out);
