# Critic 12: phase 1 at `a77fe0f9`, judged (2026-11-04)

Branch `web3d-phase1` at `a77fe0f9`, working tree clean. Chrome 156.0.8078.80 headless
(`channel: 'chrome'`, ANGLE over Metal, Apple M4 Pro); rustc 1.99.0, wasm-opt 133.

**Verdict: FAIL.** One correctness bug inside the supported model (F1), one more where a stale
reference is enough (F2), and false claims.

## Findings

### F1. `document.open()` erases web3d's loss listeners, and web3d never puts them back

`document.open()` (and `document.write()` after load, which calls it) erases the listeners of every
node in the document's tree. The window, the document and the canvas stay the same, so web3d keeps
accepting and driving the canvas. Its loss controller is kept in a WeakMap with its two listeners
added once (`js/web3d.js` `controllerFor`, lines 683-703), so they are never added again.
`results/docopen.json`, a same-origin frame's own copy on its own canvas:

- listeners 1+1 before, `{}` after; the live renderer still draws;
- the next loss is not cancelled: `restoreContext` gives "context restoration not allowed", neither
  hook runs, and every later create on that canvas is refused "the context is lost";
- a canvas web3d had used before the reopen gets a new renderer with no listener at all;
- control: a canvas that was out of the tree at the reopen keeps its listeners and recovers.

`results/blank.json` (`b2`, `b2top`) shows the same in the top document. False now: WEB3D.md 358-368
(the listeners are kept "for the canvas's life" and "run as long as the canvas's document ... does";
every lost event is cancelled), and the `controllerFor` comment.

**Fix:** keep the two handlers in the controller and call `addEventListener` for both at every
create. A second call with the same arguments does nothing, so this re-arms the canvas after a
reopen. Then either re-arm in `frame()` as well, or document that a page that reopens its document
must dispose its renderers and create new ones. Add the case to the probes.

### F2. A navigated-away frame's copy takes the new page's canvas, which its own copy is then refused

`GLOBAL` is the WindowProxy. After the frame navigates, the old realm reads `GLOBAL.document` as the
new page's document (`results/nav.json`, `oldRealmGlobalDocument`). So the old copy, called through
a reference the top kept, passes the document check for the new page's canvas. It marks the canvas
PENDING on the shared home, and its promise never settles, since a dead realm's microtasks do not
run. From then on, the new page's own copy is refused its own canvas: "already has a live renderer".

False now: the comment at `js/web3d.js` 305-308, "reading it through the global taken here gives the
window's own document".

**Fix:** also require `GLOBAL[SHARED_KEY] === REALM_STATE` in `canvasKind` and `_inPlace`. It reads
false in a navigated-away realm (`nav.json`, `fixSignal`).

Also in `nav.json`: the top's renderers on top canvases that have the frame's prototype survive the
frame's navigation, and recover twice.

### F3. Two homes joined by `document.domain`: two live renderers and a stale state cache

This is the gap the round names, shown with wrong output (`results/dd2.json`, both directions):

1. the frame's copy creates a second live renderer on a moved driven canvas;
2. moved back, the first renderer clears to the second's colour, with no error.

WEB3D.md 91 ("a second renderer is refused while the first is live") and 234 ("while it is away, the
copy there is refused it too") are false in this arrangement. WEB3D.md does not state the
exception. The arrangement needs `Origin-Agent-Cluster: ?0`, `document.domain` and an unsupported
move.

**The departure from the suggested fix was right:** per-realm owners would open the same hole in the
plain same-origin case.

**Fix:** keep the per-canvas state (owner, antialias) on the canvas object under a `Symbol.for` key.
Every realm that can touch the canvas sees it, so no home is needed for that state. Failing that,
state the limit in WEB3D.md.

### F4. A canvas moved during `createRenderer` gets a context and listeners

The document is checked only before the first await. A canvas moved into a same-origin frame's
document right after the call (or by a customized built-in canvas's `attributeChangedCallback` when
web3d sets the width) gets the context and web3d's two listeners, and the create resolves.
`frame()` then throws "moved" (`results/midcreate.json`, `m1`, `m2a`).

False now: WEB3D.md 213-224 and `js/web3d.js` 23-26 and 544-546.

**Fix:** check again after each await in `build` and after the width sets, failing through the
cleanup that is already there.

### F5. Other false or unsupported claims (each checked against its source)

- **WEB3D.md 442-443, "3,000 frames after 40 s":** each figure is a 10 s window (about 600 frames)
  taken after 40 s (`critic11-rerun/handle-boxing/run.mjs`, line 20).
- **298-299, Chrome's polygon-mode warning "once a page":** `../rendered-check.json` holds two, one a
  context.
- **260 and 323-324, a running query or capture "is ended":** a paused capture on an unbound object
  is left as it is (`rerun/run10-state.json`, "paused capture left on an unbound object").
- **201-203, an `OffscreenCanvas`'s realm "is the window or worker that made it":** this window's
  code can make one with `transferControlToOffscreen`, through its own prototype's method too, from
  a canvas of its own document that a frame's observer touched first. It then has the frame's
  prototype, is refused here with "it was made in, or transferred to, another window", and its loss
  events stop when the frame goes (`results/transfer.json`). The refusal is safe; the sentence and
  the message are wrong.
- **746-749, DevTools's per-context listing:** the premise is true (`results/per.json`: a frame's
  canvas reads 0 from the top's context and 2 from its own). But the cited `listeners.json` was made
  with the fixed counter and cannot show it.
- **620-623:** two figures with no evidence file: no Chrome in the 60 s before the run, and "four
  to nine times".
- **463-465:** the 22.0 is in `critic10-rerun/cpu-timing/runs-40s.txt`.
- **188, "a failed create leaves nothing behind":** the canvas keeps the context and web3d's two
  listeners.
- **759, "runs only on a push":** CI also runs on `pull_request`. Minor.

## Notes

- **The listener counter** (`results/counter.json`): exact across realms, nested frames, windowless
  documents and `OffscreenCanvas`es, with no double counting. One blind spot: a canvas of a removed
  frame's document reads `{}` though listeners are on it. No current probe counts one.
- **F4's side effects** (`results/f4.json`): only on a context with alpha whose buffer size was
  stored. A MutationObserver and `attributeChangedCallback` see two extra width changes (w to w−1,
  then back to w). There is no ResizeObserver entry, and the CSS size is unchanged.
  `drawingBufferStorage` on a context without alpha raises `INVALID_OPERATION`, so the premise holds.
- **Re-entrancy from `attributeChangedCallback`** during `resize` and a create is refused
  (`midcreate.json`, `m2b` and `m2c`). The message speaks of "reading another call's arguments".
- **Attacks that held:**
  - `results/adopt.json`: `adoptNode` and `importNode` each way; template, `DOMParser`,
    `createHTMLDocument` and `new Document()` canvases; shadow roots, open, closed and declarative;
  - `results/override.json`: `ownerDocument` overridden on the element and its prototypes, before
    and after load (`Node.prototype` patched before load is out of scope);
  - `results/bf.json`: a restore from the back-forward cache;
  - frame removal (`blank.json`, `b3`).
- **about:blank:** Chrome 156 did not reuse the window when a frame's initial about:blank loaded a
  same-origin page (`blank.json`, `b1`). The comment's example is harmless.

## Gates (`gates/`)

`gates.sh` rewrites the round's committed evidence, so each gate was run on its own with the same
command.

- fmt, clippy (CI's macOS row, its wasm32 step, and wasm32, aarch64 Linux, Windows and the host
  with all features), and 75 tests in debug and release: all exit 0.
- `build.sh` ×3, the third from an empty target directory: byte-identical (wasm `4d6594b5…`), with
  no local path.
- `three-reference.mjs --check`: 20 lines, a match.
- `check.mjs`: exit 0. 74 of 74 probes, 0 contexts lost to Chrome's limit, steady allocation 0.07,
  control 88.2.
- `--blind`: exit 3, control 0.6.

`gates.sh` judges each gate honestly. One check it does not make: its YAML gate looks for the CI
step by name, not by command.

**Timing, on a quiet GPU.** The only headless Chromes during the run were this check's own
(`gates/contention-during.txt`, sampled every 5 s).

| Run | GPU ms p50 | GPU ms p95 | CPU ms p50 | CPU ms p95 |
| --- | --- | --- | --- | --- |
| 1024x576 | 0.863 | 0.89 | 0.155 | 0.26 |
| 1366x768 | 1.048 | 1.069 | 0.175 | 0.25 |
| 1024x576, no MSAA | 0.585 | 0.61 | 0.105 | 0.19 |

The GPU figures agree with WEB3D.md's quiet table to within 0.02 ms.

## Earlier critics, rerun (`rerun-critics.sh`, `rerun/`, `results/rerun-*.json`)

All 34 runs exit 0. Every boolean matches the twelfth round's rerun: 31 result files, 0 differ
(`../critic11/tools/bool-diff.mjs`).

Each by-premise argument holds:

- X8: the fraction rounds to the handle itself;
- K1, E1, G1, G12 and G14: limits and layouts changed;
- G8a and E16: refused by design;
- LC5: a later loss is cancelled by design;
- `manyOnOneCanvas`: it reads a map that no longer exists;
- N7b: the message wording, and the order of the page's listener;
- N11: no browser call allows it;
- N10, N10c, N10e and `notop`: refused by design, and their intent passes (`rerun-round12-intent`);
- `xtop` and A7: the state's location changed;
- `storageFloat`: the buffer is put back to `RGBA8`.

N10's intent, though (no canvas left with a loss nobody cancels), is broken another way by F1.
