# The thirteenth critic's findings, answered, and every critic's probes rerun (2026-11-04)

The fourteenth builder round, on the working tree of loadngo `web3d-phase1` at `757d0100`. Chrome
157.0.8103.38 headless (`channel: 'chrome'`, ANGLE over Metal, Apple M4 Pro), as every result of
this round records; rustc 1.99.0, wasm-opt 133. (Corrected in the fifteenth round: this line said
Chrome 156.0.8078.80, the fourteenth critic's F4.) The Mac's GPU was claimed for the runs and
shared with another session, whose headless Chromes drew during this round;
`gates-contention/contention-during.txt` records which Chromes were alive, and whose, every 5 s of
the gate run.

The supported model is unchanged: one copy of web3d, in the canvas's own realm, drives a canvas,
and a canvas element is taken only when its owner document is this window's document. This round
adds as little code as it could: one getter taken at load and one condition in `ownRealm()`. The
rest is documentation, one probe case, the YAML gate, and the confidentiality scan (kept in the
game's repository). Only `js/web3d.js` and `js/web3d.d.ts` changed in what ships (the latter in
comments only); the Rust source did not, and the wasm is byte for byte the one the critic built
(`4d6594b5…`).

## What changed

### F1: a removed frame's copy no longer holds a canvas

The critic's tested fix, applied. In a window, `ownRealm()` (`js/web3d.js`) now also requires that
the window is open, read through the built-in `closed` getter taken when the module loads (a
worker has none):

```js
return GLOBAL[SHARED_KEY] === REALM_STATE && !(CLOSED !== undefined && CLOSED.call(GLOBAL));
```

and `inThisDocument` calls `ownRealm()` instead of repeating its first half. A removed frame's
global still reads its own realm's state (`../critic13/results/f2.json`, R0), but its window reads
`closed` (`f2b.json`, `signals`: true once removed, false while attached and after a navigation).
So every check that already refused a navigated-away realm now refuses a removed frame's too: its
claims and live renderers no longer hold their canvases, `createRenderer` refuses every canvas
before recording anything, and `frame()`, `resize()` and the creates throw. The message names both
cases and keeps "navigated away", which the probes match: "this copy of web3d belongs to a page
that its window has navigated away from, or to a frame that has been removed (its window is
closed), so it drives no canvas any more; use a copy of web3d loaded in the present page of an
open window".

`frame()`'s per-frame check is now two property reads and two getter calls (`closed` and the
canvas's `ownerDocument`); it allocates nothing (the rendered check's allocation gate runs through
it, below).

**Probe NV** (`demo/probe.js`, `navigatedAway`) gains the critic's case and three more of the same
kind. A second frame's copy compiles the module (a renderer on a canvas it then moves into this
document, which `frame()` refuses as moved), begins a create whose module never arrives, and one on
its own canvas, and the frame is removed in that same task. Then:

- the window reads `closed`; called after the removal, the removed copy is refused a canvas of its
  own document before recording anything on it (`recordMadeAfterRemoval` "none");
- its renderer no longer draws (`frame()` throws the message above);
- this window's copy takes all four canvases (the two whose creates were begun, the renderer's,
  and the one the removed copy was called on), draws on each, and recovers each from a loss and
  restore, each hook once.

The control, `control/probes-at-757d0100.json`, is today's probe page against `757d0100`'s `pkg/`
(its `js/` with the same wasm, the hashes the critic's `gates/builds.txt` lists): 79 of 80 pass,
and NV fails, this window's copy refused all four canvases, "this canvas already has a live
renderer". Today's run: 80 of 80.

WEB3D.md 228-237 (now "A copy whose window has navigated away, or whose frame has been removed,
drives nothing") and the comment on `CLAIM` say why a claim must stop holding: a navigated-away
realm never finishes a create's awaits, and nor does a removed frame's once its copy has compiled
the module. Every other place that named only navigation says both now (the API table, the refused
list, "Context loss", the Limits, `js/web3d.d.ts`). One sentence adds the critic's R2: a canvas
still in the removed frame's document with a context loses it with the frame, the browser's doing.

### F2: a reopening listener ahead of web3d's, for either event, is documented as a limit

Documented, not closed in code. WEB3D.md's "When the listeners are on" now says exactly which
placements are covered and lists three cases left, not two, and the Limits say the same:

- covered: a page listener that reopens the document during a lost or restored event's dispatch on
  the window, the document, an ancestor element or a shadow root (capturing; these events do not
  bubble), or on the canvas after web3d's. A microtask checkpoint follows each listener's callback,
  so the observer has put web3d's listeners back before the event reaches the canvas;
- left: such a listener on the canvas itself, ahead of web3d's. During a lost event's dispatch,
  nothing cancels the event and the context is lost for good (the case WEB3D.md had). During a
  restored event's dispatch (the critic's F2), the context is restored but nothing calls that
  restore's hooks: the controller stays as it was, and its hooks come with the canvas's next
  restore, after that loss's `onContextLost` (`../critic13/results/f1b.json`, E).

**`site/reopen-paths.html`** (`results/reopen-paths.json`) measures each placement for each event,
with a control: a fresh frame, its copy's renderer on its canvas held in a `<div>` (or a shadow
root), the page's reopening listener at one place, a loss and restore by the documented recovery.
Twelve of fourteen cases are covered (the context restored, each hook once); the two left are the
canvas listener ahead of web3d's, lost (not restored, no hook) and restored (restored, no
`onContextRestored`).

Why not the critic's code fix (a capturing listener on the canvas's root node, each event handled
once): the root changes as the canvas moves between the document and shadow roots, so the listener
would have to follow it, come back after each reopening (the root's listeners are erased too), and
dedupe against the canvas's own pair, which a canvas in no document still needs. That is new state
and ordering for a page that reopens its own document from inside a WebGL event.

**Corrected in the fifteenth round** (the fourteenth critic's F2(b)): this paragraph went on to
reject calling the missed restore's hooks from the observer because "it cannot tell a missed
restore from one whose event is still to come". That does not hold: in Chrome no task sees a
restored context before its restored event has run, so a check in a later task can tell them apart
(`../critic14/results/c7-restore-gap.json`); and during a lost event's dispatch the observer runs
while `window.event` is that event, so it could cancel a lost event for a canvas outside any shadow
tree (`../critic14/results/c3-reopen.json`, R6). The reason these cases stay open is now the page
contract (WEB3D.md, "The page contract" and "Why the last two cases stay open"): they are outside
it, and web3d carries no code for pages outside it.

The comments on `controllerFor` and `arm` that said the observer re-arms "before any lost event can
be dispatched" now say what holds: before any later event, and before one being dispatched reaches
the canvas from a listener on the window or another node ahead of it.

### F3: the allocation figures are this round's run, cited to the files this round leaves

WEB3D.md's "No per-frame allocation" quotes this round's final run, which `../rendered-check.json`
and `../blind-control/` hold (see Gates below): steady 0.09 sampled bytes a frame, the control 88.9,
blind 0.6, cold 257.6, 29.9 and 1.9. The 56-byte sentence stays, now true of the file it cites:
this run's steady window holds the check's own measuring call, 56 bytes
(`window.__web3d.frames`), and nothing else from the page's scripts, as `a77fe0f9`'s did (the
committed `757d0100` run's held none).

### The notes

- **A driven canvas moved away and back, with the other document reopened meanwhile:** stated
  beside the move limit, with its cost. It comes back with none of web3d's listeners (the reopening
  erased them, and this realm's observer watches only documents it armed canvases in), `frame()`
  draws again, and the next loss is for good, and so is the canvas for WebGL, which keeps its lost
  context (`../critic13/results/f1b.json`, I). The first of the three cases left says the same, and
  so does the Limits entry.
- **"After each size it sets"** now reads "after it sets the canvas's width and height", one check
  for the pair, and again after the two width sets that remake a page's stored drawing buffer
  (WEB3D.md's API table, "What a failed create leaves", "One realm drives a canvas"; the comments at
  the top of `js/web3d.js` and on `take`; `js/web3d.d.ts`). The critic's H2 (`f4.json`), moved away
  at the width set and back at the height set, is cited as passing, sized while away, with no event
  in between. "After each await" now reads "after its last await".
- **What a failed create leaves:** one that fails once web3d sizes the canvas also leaves
  `EXT_disjoint_timer_query_webgl2` and `KHR_parallel_shader_compile` enabled (`bind` enables them
  before the sizing; the critic's `f4.json`, X), and one whose init fails also leaves enabled those
  of `EXT_clip_control`, `EXT_depth_clamp` and `WEBGL_clip_cull_distance` the browser has, which
  the reset enables to put their state back. Enabling an extension changes no state, and WebGL has
  no call that disables one.
- **The YAML gate** is tightened and moved into its own file, `../ci-step.rb`, which `gates.sh`
  runs. The step must run exactly two lines, the wasm32 target added and then the local gate's
  clippy command; neither the step nor its job may set `continue-on-error`; and no shell may be set
  on the step, the job or the workflow, so the step runs under GitHub's default `bash -e`.
  `yaml-gate/run.sh` runs it on the real workflow and on eight changed copies made in a temporary
  directory: `exit 0` first, `true` after, `|| true`, `--all-targets` dropped, `continue-on-error`
  on the step and on the job, a shell on the step and a default one on the workflow. The real one
  passes and all eight fail (`yaml-gate/results.txt`).
- **The confidentiality scan** (kept in the game's repository, since its pattern names what it
  looks for) gains a letter-boundary pass and a strict PNG reader; see "The confidentiality scan"
  below.

## The probes (`demo/probe.js`, run by `demo/check.mjs`): 80, all pass

No new probe; NV gains the removed frame (above), and its description says so.

## Every critic's probes, rerun (`rerun-critics.sh`; `rerun-critics.log`, `rerun/`, `results/rerun-*.json`)

```
PLAYWRIGHT_CORE=.../playwright-core/index.mjs THREE_BUILD=.../three/build bash rerun-critics.sh <port>
```

All 55 runs exited 0: the 46 the thirteenth critic ran (critics one to twelve and the twelfth
round's pages), the thirteenth critic's eight pages (served from its own folder, which is left as
it is), and this round's `reopen-paths.html`. `tools/run.mjs` is the critic's driver, copied so
that results land here, serving with the critic's own `serve.mjs`. `booleans.sh` compares every
boolean leaf with the critic's (`booleans.txt`): 0 differ in 50 of the 51 files; in the critic's
`f2.json`, six differ, all where F1's fix acts: R1 and R1b, refused before, now create, draw and
recover (their loss records exist only in this run).

The thirteenth critic's pages against its own results:

| Page | Then | Now |
| --- | --- | --- |
| `f2c` | refused in 18 of 36 trials: every one where the removed frame's copy had compiled the module, or its module never arrived | created in all 36 |
| `f2` | R1 and R1b refused, "already has a live renderer" | created, draws, recovers, each hook once; R1's second create 1 s later too |
| `f2b` | `claimed`: refused, a live raw context | created; the control and `signals` as before |
| `f2-fix` | the trial fix, 6 of 6 created and drawn | unchanged (it runs the critic's trial `pkg/`) |
| `f1` | every case as the critic read it | 0 booleans and 0 strings differ |
| `f1b` | E: no `onContextRestored` on the restore; I: no listener after the move and reopening | unchanged: both are now documented limits (F2, and the note on moved canvases) |
| `f3` | the attacks held; the out-of-scope ones as stated | unchanged, but G's message, now the new one |
| `f4` | H1, L, R held; H2 passes; X leaves two extensions | unchanged: H2 and X are now in WEB3D.md |

`f2`'s R2 (a removed frame's live renderer, its canvas then adopted here) reads as before, except
that the first create's message is now "lost before web3d first used this canvas" where the
critic's read "lost; create the renderer again…": which one appears depends on whether the
browser's lost event reached this window's listener before the create took the context, a matter
of timing. Either way the context went with the frame, the browser's doing.

**By premise**, each unchanged from the thirteenth critic's list, which agreed with the twelfth
round's:

- the fourth's and fifth's X8: the fraction rounds to the handle itself below 2^50;
- the fifth's K1 and E1, the sixth's G1, G12 and G14: limits and layouts that changed (no 32-renderer
  limit, handles below 2^50, 2,048 generations a slot, a counter that goes round);
- the sixth's G8a and the eighth's E16: `<img>` and `bitmaprenderer` canvases refused by design;
- the seventh's LC5: a later loss cancelled by design;
- the eighth's `manyOnOneCanvas`: it reads a map that no longer exists (its intent passes in the
  eleventh critic's `intent.html`, rerun: `results/rerun-critic11-intent.json`);
- the ninth's N7b: its message hedges where web3d cannot tell whether the lost event came before the
  call; N11: no browser call tells whether a canvas has a context without making one;
- the ninth's N10, N10c and N10e, and the tenth's `notop`: a frame's copy given a canvas of the top
  document, refused by design; their intent passes (`results/rerun-round12-intent.json`);
- the tenth's `state`: its colour-space and drawing-buffer cases read as their controls, which fail
  its green-pixel test too; `xtop` and the eleventh's A7: they look for a home's state where it no
  longer is; `state2`'s `storageFloat`: the buffer is put back to `RGBA8`;
- the twelfth's `blank`: `b2`'s `prevented` reads false because its own listener, added in the
  reopening's task, runs before web3d's re-added one; the restore that follows shows web3d
  cancelled the event.

And the thirteenth critic's own pages, as its README reads them:

- `f1`'s P2, G, D2 and E3 `recovered` false: each page reopens the document from a listener and so
  erases its own measuring listener, added before; web3d's hooks ran once each and the context was
  restored (the critic's "attacks that held");
- `f1`'s D and E, and `f1b`'s E: a listener of the page's on the canvas ahead of web3d's that
  reopens during a lost or restored event, now both documented limits (F2);
- `f1b`'s I: a driven canvas moved away and back with the other document reopened, documented with
  its cost;
- `f3`'s I (a copy in an isolated world), and its key defined on `EventTarget.prototype` or
  overwritten on the window: out of scope as WEB3D.md states (a page that patches built-ins or
  web3d's key; an extension's world);
- `f2`'s R2: the browser loses the removed frame's context.

## Gates

`../gates.sh 5668 5669`, run under the critic's contention sampler
(`gates-contention/contention-during.txt`) and recorded in `../gates.txt` (home written `<home>`),
exited 0: every gate held. It was run twice this round; the record is the second, on the final
code (the first, before four comments in `js/web3d.js` were rewrapped, also held every gate).

| Gate | Result |
| --- | --- |
| `cargo fmt --check` | exit 0 |
| clippy, CI's macOS row (the workspace, `-D warnings`) and its wasm32 step | exit 0, exit 0 |
| clippy `-p loadngo-web3d --all-features` on wasm32, aarch64 Linux, Windows MSVC and the host | exit 0 each |
| `cargo test --locked -p loadngo-web3d`, debug and release | 75 passed each |
| `build.sh` x3, the third from an empty target directory | byte-identical `pkg/`, no local path |
| `three-reference.mjs --check` | 20 lines verbatim, output matches three 0.180.0 |
| the CI step's YAML (`../ci-step.rb`) | the step found; it runs exactly the two lines, with no `continue-on-error` and no shell set |
| `check.mjs` | exit 0 |
| `check.mjs --blind` | exit 3, as required |

The builds (`sha256`):

- wasm 70,249 bytes (32,174 gzipped), `4d6594b5…`, the same bytes as `757d0100`'s: no Rust changed;
- `gl.js` 22,570 bytes (7,617 gzipped), `6542cb39…`, unchanged;
- `web3d.js` 67,916 bytes (21,021 gzipped), `91f5bc13…`; 1,448 bytes more than `757d0100`'s (451
  gzipped): 140 outside comment lines, 69 of them the longer NAVIGATED message, and the rest in
  comments;
- `web3d.d.ts` `d44794fb…` (comments only).

The rendered check (`../rendered-check.json`, `../rendered-check.log`):

- 64 draws and 320,000 triangles in every run, the clear colour exact (33, 36, 43), the centre lit,
  `getError` 0, no page error, every frame 16.67 ms;
- 80 of 80 probes, no `INVALID_OPERATION`, the two pending errors raised on purpose, and 0 contexts
  lost to Chrome's limit;
- steady allocation 0.09 sampled bytes a frame over 601 frames: the 56 bytes of the check's own
  measuring call (`window.__web3d.frames`), none in `pkg/`;
- the positive control 88.9;
- cold: 257.6 (the module's call wrappers), 29.9 (`web3d.js`), 1.9 (`gl.js`).

The blind run (`../blind-control/`): the control read 0.6, everything else held, exit 3.

**Timing: contended.** A desktop Chrome window and one to five other sessions' headless Chromes were
alive at every sample of the run (each named only by whose it is). The wasm is unchanged, so the
GPU's work is too, and WEB3D.md's table stays the twelfth critic's quiet run, which the thirteenth
critic's near-quiet run matched within 0.03 ms at the median. This run's figures, not capacity
figures:

- GPU p50 0.475, 0.318 and 0.569 ms, p95 4.681, 0.642 and 7.423 ms (1024x576 with MSAA, 1366x768,
  without MSAA);
- CPU p50 0.05, 0.055 and 0.065 ms, p95 0.08, 0.075 and 0.08 ms.

## The confidentiality scan

Kept outside this repository, since its pattern names what it looks for. It covers every file the
branch changes from `dev` and every untracked file (this folder and `../critic13/` included), and
the branch's commit messages. This round, as the critic asked:

- **a letter-boundary pass**, beside the whole-word one: each file is read with underscores and
  digits turned to spaces, then again with camel-case humps split, and searched with any one
  non-letter, or none, between the words of the pattern's multi-word names. The whole-word pass's
  `\b` treats underscores
  and digits as letters and cannot see case, so it missed a name joined to a letter by an
  underscore, a digit or a camel-case hump;
- **a strict PNG reader**: it walks the chunks from the signature to IEND and refuses the file (a
  count of its own, and the whole file is then scanned too) on bytes after IEND, a chunk type the
  PNG specification does not define, a CRC that does not match, or a chunk cut short. Before, a
  chunk typed IDAT after IEND was skipped as pixels.

Its controls, in a throwaway repository: six joined names (an underscore, a camel-case run, a
digit, a name inside a camel-case identifier, a multi-word name without separators, and one with
dots between its words) found by the new pass and missed by the old; the earlier controls (a text file, tEXt and zTXt chunks) found by
both; five broken PNGs refused, one of them with a name in an IDAT chunk after IEND; and a clean
text file, a clean PNG and a name only inside a valid PNG's IDAT neither named nor refused.

The scan of the branch as this round leaves it: zero on every line, names and home paths by
`grep -a` and by `strings`, names at letter boundaries both ways, PNGs refused, and the commit
messages both ways.
