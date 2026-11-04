// Critic 15, S2: a module worker that gives its global a `closed` before it imports web3d
// (?mode=data sets `globalThis.closed = true`; ?mode=accessor defines an accessor that returns
// true; ?mode=accessor-false one that returns false; ?mode=none leaves it), then draws on the
// OffscreenCanvas it is sent. A worker has no built-in `closed` (nor `top`). The round's T3 did
// this for `top`. BSD-3-Clause; see the repository's LICENSE.
import { green, settle, OPTS } from '../../critic13/site/common.js';
const mode = new URL(import.meta.url).searchParams.get('mode');
if (mode === 'data') globalThis.closed = true;
if (mode === 'accessor') Object.defineProperty(globalThis, 'closed', { get: () => true, configurable: true });
if (mode === 'accessor-false') Object.defineProperty(globalThis, 'closed', { get: () => false, configurable: true });
const web3d = await import('/pkg/web3d.js');
onmessage = async (e) => {
  const made = await settle(web3d.createRenderer(e.data.canvas, OPTS));
  if (!made.ok) return postMessage({ created: false, message: made.message });
  const g = green(made.r);
  made.r.dispose();
  postMessage({ created: true, isGreen: g.isGreen });
};
const d = Object.getOwnPropertyDescriptor(globalThis, 'closed');
postMessage({ ready: true, closed: d ? (d.get ? `accessor, configurable ${d.configurable}` : `data ${String(d.value)}`) : 'none' });
