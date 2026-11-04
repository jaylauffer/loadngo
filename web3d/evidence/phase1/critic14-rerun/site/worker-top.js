// Round 15: a module worker that gives its global a `top` before it imports web3d (?mode=data sets
// `globalThis.top = null`; ?mode=accessor defines an accessor that returns null; ?mode=none leaves
// it), then draws on the OffscreenCanvas it is sent. A worker has no built-in `top`. BSD-3-Clause;
// see the repository's LICENSE.
import { green, settle, OPTS } from '../../critic13/site/common.js';
const mode = new URL(import.meta.url).searchParams.get('mode');
if (mode === 'data') globalThis.top = null;
if (mode === 'accessor') Object.defineProperty(globalThis, 'top', { get: () => null });
const web3d = await import('/pkg/web3d.js');
onmessage = async (e) => {
  const made = await settle(web3d.createRenderer(e.data.canvas, OPTS));
  if (!made.ok) return postMessage({ created: false, message: made.message });
  const g = green(made.r);
  made.r.dispose();
  postMessage({ created: true, isGreen: g.isGreen });
};
const d = Object.getOwnPropertyDescriptor(globalThis, 'top');
postMessage({ ready: true, top: d ? (d.get ? `accessor, configurable ${d.configurable}` : `data ${String(d.value)}`) : 'none' });
