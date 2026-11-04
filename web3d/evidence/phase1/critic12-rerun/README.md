# The twelfth critic's findings, answered, and every critic's probes rerun (2026-11-04)

The thirteenth builder round, on the working tree of loadngo `web3d-phase1` at `a77fe0f9`. Chrome
156.0.8078.80 headless (`channel: 'chrome'`, ANGLE over Metal, Apple M4 Pro); rustc 1.99.0,
wasm-opt 133. The Mac's GPU was claimed for the runs and shared with other sessions, whose headless
Chromes drew during most of this round; `gates-contention.txt` records which Chromes were alive,
and whose, every 5 s of the gate run.

The supported model is unchanged: one copy of web3d, in the canvas's own realm, drives a canvas,
and a canvas element is taken only when its owner document is this window's document. What changed
is that the checks now hold at every moment of a create, after a document is reopened, and after a
frame navigates, and that the per-canvas state is kept where every copy that can reach the canvas
sees it. Only `js/` changed in what ships; the Rust source did not, and the wasm is byte for byte
the one the critic built (`4d6594b5…`).

## What changed

### F1: document.open() no longer silently breaks loss recovery

The two listeners are now the controller's own functions (`lostListener`, `restoredListener` in
`controllerFor`), and `arm` puts them on:

- **at every create**, as it claims the canvas, before its first await. Adding a listener that is
  on already does nothing, so this re-arms a canvas whose listeners were erased;
- **after every `document.open()`**, without waiting for a create. `document.open()` (and
  `document.write()` after load, which calls it) erases every listener in the document's tree, and
  also takes the document's children away. `arm` watches the document of each canvas element it
  arms with one `MutationObserver` a realm (`childList` on the document node only), and its callback
  puts back the listeners of every canvas armed in the realm. It runs at the next microtask
  checkpoint, so before the next task: a lost event, dispatched in a task of its own, finds them on.

That is a third way, beside the two the critic offered. Re-arming in `frame()` would leave the gap
from a reopening to the next frame, would not cover a canvas with no live renderer (whose losses the
controller also cancels), and would cost two binding calls a frame. Documenting "dispose and create
again after a reopen" would leave the second case broken. The observer costs nothing a frame.

What is left, and stated in WEB3D.md ("The loss controller", "Limits"):

- a canvas that was in another document when that one was reopened (a page handing canvases between
  windows) has its listeners back at its next create here (probe DO, case C);
- a lost listener of the page's that runs before web3d's and reopens the document inside the lost
  event's own dispatch: web3d's listener is removed before it is reached, and nothing cancels that
  event.

**Probe DO** (`demo/probe.js`, `documentReopened`): a same-origin frame's copy drives canvas A and
has used canvas B. The top reopens the frame's document, puts both back, draws on A, and loses A in
the same task: the loss is cancelled, the restore comes, each hook once, a new renderer draws. The
frame's own code then reopens again with `document.write()` after load, and B, with no renderer, is
lost and restored and drawn on. Canvas C, moved into another frame's document that is then
reopened, comes back with no listener and has them again after its next create, which recovers from
a loss. The twelfth critic's `docopen.html` and `blank.html` now read fixed
(`results/rerun-critic12-docopen.json`: listeners 1 and 1 after the reopening, the loss cancelled,
both hooks once, a new renderer created; `blank` `b2top` recovers).

### F2: a navigated-away copy is refused, and its claims and renderers stop holding canvases

`ownRealm()` (`js/web3d.js`) is `GLOBAL[SHARED_KEY] === REALM_STATE`, read in a `try` (a window
navigated to another origin throws). The WindowProxy names the new page's window after a navigation,
and that window holds its own realm's state, so this reads false in the old realm, as the critic's
`nav.json` `fixSignal` showed.

- `canvasKind` refuses every canvas when it is false, before anything is recorded on the canvas;
- `inThisDocument`, used by `canvasKind` and by `Renderer._inPlace` (`frame()`, `resize()`, the
  creates), requires it before it compares documents; `frame()` then throws "navigated away";
- a renderer `_holds()` its canvas only while it is live *and* its realm is its window's own, and a
  create's claim holds only while its realm is (a navigated-away realm's awaits never finish, so its
  claim would otherwise block the canvas for good).

The comment at `js/web3d.js` 305-308 is rewritten: the global is the WindowProxy, and its
`document` is the window's own only while `ownRealm()` holds.

**Probe NV** (`navigatedAway`): a frame's copy draws on its canvas, drives a second canvas it then
moves into the top document, and begins a create whose module never arrives (a `Response` whose
body never ends), which claims a third. The frame navigates. The old copy, called through the
references the top kept, is refused the new page's canvas with nothing recorded on it (its promise
never settles, as any navigated-away realm's), and the new page's copy takes that canvas, with its
listeners only (1 and 1), and recovers from a loss. The old renderers no longer draw: one returns
false (its context went with its document), the other, whose canvas is in the top document, throws
"navigated away". The top's copy then takes both of those canvases, draws and recovers.

NV also found a bug that the F2 fix made reachable (before it, no other copy could take a canvas a
navigated-away copy's renderer held), now fixed: the old copy's renderer, disposed after the top had lost and restored its canvas, sent its
deletes into the restored context. Its shim learnt of losses only from its own realm's controller,
whose listeners no longer ran. The loss count is now on the canvas's record (below), counted by
whichever realm's listener runs, so every renderer on the canvas learns of it. The control,
`control/probes-losses-per-realm.json`, is this round's `pkg/` with only that count moved back to
the realm's controller (`control/losses-per-realm.py`): all 80 probes pass, and the page logs 18
`INVALID_OPERATION` "object does not belong to this context" warnings, which `check.mjs` fails on.
Today's run logs none.

### F3: one record of a canvas, kept on the canvas

The critic's suggested fix. `recordOf(canvas)` keeps `{ owner, antialias, losses }` on the canvas
itself, under `Symbol.for('loadngo-web3d')`, as a non-writable, non-configurable, non-enumerable
property with a null-prototype value. Chrome wraps a node in one object for all the realms that can
reach it, so every copy that could create on the canvas reads the same record, whatever its home. The
home's owner and antialias maps are gone; the home keeps only the serial counter.

This keeps the twelfth critic's point about the earlier departure: owners are not per realm. They
are shared more widely than before, by every copy that can touch the canvas, so the plain
same-origin case stays covered (XD's `moved.frameCopy`), and the `document.domain` case is now
covered too.

- The critic's `dd2.html` (`results/rerun-critic12-dd2.json`, from `a.localhost` with
  `Origin-Agent-Cluster: ?0`): both directions refused, "already has a live renderer"
  (`gap.forward` and `gap.reverse` false), and the first renderer still clears to its own red
  (`r1AfterMoveBack.corner` 255, 0, 0).
- **Probe RC** (`canvasRecord`) stands in for it in the rendered check, whose page is cross-origin
  isolated, where `document.domain` does nothing: `demo/probe-home.html` is a frame whose copy is
  given a home of its own by a test hook. It is refused this window's driven canvas, moved into its
  document, and this window's copy is refused the frame's; moved back, each clears to its own red.
  RC also checks the record is private and is one object from the frame's realm, and that a sealed
  canvas, and canvases with a writable or a getter property there, are refused untouched, the getter
  never run.

At `a77fe0f9` RC's frame creates the second renderer, and the first then clears to the second's
colour (`control/probes-at-a77fe0f9.json`, `ours.back` false), the critic's F3 reproduced in-page.

### F4: a canvas moved during a create is refused, and leaves nothing it can remove

`createRenderer` now does, in order:

1. reads the options (the caller's getters run here), then checks the canvas through built-ins only,
   so no page code runs between that check and the claim (an options getter that moves the canvas is
   seen: MC m0);
2. claims the canvas in its record and puts the listeners on;
3. compiles and instantiates the module: its only awaits. The shim is made unbound and given its
   context afterwards (`createGl()` and `bind` in `js/gl.js`); the module has no start function, so
   no import runs before that. A module that is not web3d is refused here, before any context;
4. in one task (`take`): checks the canvas again, takes the context, makes the renderer, and checks
   again after each size it sets, the one place page code can still run (a customized built-in
   canvas's `attributeChangedCallback`). The context attributes are a null-prototype object, so the
   dictionary conversion reads nothing from `Object.prototype`.

A create refused before the context leaves no context, no claim and no listener it added. One that
fails after leaves the context (no call removes it), puts the size back, releases the claim, and
takes off the listeners it added, unless it failed because the context is lost (they then bring its
`onContextRestored`). Listeners the realm had on the canvas before stay. WEB3D.md 188 is rewritten to
say exactly this ("What a failed create leaves").

**Probe MC** (`movedDuringCreate`): m0, an options getter that moves the canvas (refused, nothing
recorded); m1, moved right after the call (refused, no listener, no claim, no context), and m1b, the
frame's copy then takes it; m1c, moved away and back during the awaits (created); m2a, moved by its
own `attributeChangedCallback` at web3d's width set (refused, size put back to 20x10, no listener, no
claim, the context kept), and m2b, the same on a canvas this window drove before (its listeners
kept, and created once moved back); m2c, re-entrancy from that callback during `resize()` (refused
with the new message) and a create inside a create (refused); m3, a module that is not web3d (no
listener, no context); m4, a `MutationObserver` that moves the canvas when web3d sets its width,
which runs after the create's last check: the create resolves and `frame()` throws "moved" until the
canvas is back, as for any driven canvas moved.

Two consequences, both in WEB3D.md:

- **A loss and restore while a create loads its module** come before it takes the context, which is
  then the restored one: the create succeeds and draws (LE's ET, rewritten). At `a77fe0f9` the create
  took the context first and failed "lost".
- **A loss while a create loads its module** is cancelled by the listener added at the claim. So the
  ninth critic's N7b now sees web3d's cancellation from its own later listener (`bPrevented` true; it
  still fails on its message, which hedges, as before).

The comments the critic named are corrected: `js/web3d.js` 23-26 (the header) and 544-546
(`createRenderer`).

### F5: the claims, corrected (each checked against its source)

| WEB3D.md (critic's line) | Was | Now | Source |
| --- | --- | --- | --- |
| 442-443 | "3,000 frames after 40 s" | sampled over 10 s, about 600 frames, once the page had drawn 40 s (its `frames` reads 3,000 at the end) | `critic11-rerun/handle-boxing/run.mjs` lines 17-20 |
| 298-299 | Chrome's polygon-mode warning "once a page" | once a context: the probe page's messages hold two, from the two contexts PS enables it on | `../rendered-check.json` `probes.messages`; `demo/probe.js` PS |
| 260, 323-324 | a running capture "is ended" | the bound transform feedback object's capture is ended, running or paused; one left paused on an object the page unbound stays, and draws right | `gl.js` `end_transform_feedback`; `critic12/rerun/run10-state.json` |
| 201-203 | an `OffscreenCanvas`'s realm "is the window or worker that made it" | taken when its prototype is this realm's; one made by `transferControlToOffscreen()` from a canvas a frame touched first has the frame's, and is refused with a message naming that case (probe TO) | `critic12/results/transfer.json` |
| 746-749 | cites `listeners.json` for the DevTools premise | cites the critic's `per.json` (0 from the top's context, 2 from the frame's), rerun here | `results/rerun-critic12-per.json` |
| 620-623 | "no Chrome in the 60 s before", "four to nine times" | removed; the quiet table is now the twelfth critic's run, whose `contention-during.txt` shows only its own Chromes | `critic12/gates/` |
| 463-465 | the 22.0 in `runs-40s.txt` (the eleventh's) | 22, in `critic10-rerun/cpu-timing/runs-40s.txt`, `bare=1&held=const&cpu=1` | that file, line 9 |
| 188 | "a failed create leaves nothing behind" | what it leaves: the context only (see F4) | `take`, `createRenderer` |
| 759 | "runs only on a push" | on GitHub, for a push or a pull request (and `workflow_dispatch`) | `.github/workflows/ci.yml` `on:` |
| 91, 234 | a second renderer refused; the copy there refused (false across homes) | true across homes: the record is on the canvas | `rerun-critic12-dd2.json`, RC |
| 358-368 | listeners kept "for the canvas's life", run "as long as the document does" | when they are on, what erases them, how they come back, and the two cases left | DO |

### The notes

- **The re-entrancy message** now reads "a renderer method was called while the renderer was reading
  another call's arguments or sizing its canvas, from page code that ran there (a getter, valueOf or
  a Proxy trap, or a custom element's attributeChangedCallback)". It keeps "while the renderer was
  reading", which the fifth and sixth critics' probes and probe U match.
- **The YAML gate** (`../gates.sh`) finds the step by name and then checks what it runs: the wasm32
  target added, and the very clippy command the local gate runs (one shell variable feeds both), on a
  matrix row that exists, in a workflow that runs on pushes and pull requests. A step with another
  command fails it: run with a command missing `--all-targets`, it exits 1 with "the step does not
  run: …".
- **The confidentiality pattern** is widened, in the game repository's
  `evidence/web3d/round10/confidentiality.sh` (it names what it looks for, so it is kept there), to
  every term of the critic's wider scan. One of them, three letters long, matches random bytes in
  the compressed pixels of three screenshots, so a PNG is now scanned through every chunk but IDAT,
  its compressed text and profile chunks decompressed; every other file is scanned whole, as before.
  Its positive and negative controls are kept beside the game's notes.

### Also changed

- **Events the page dispatches itself are ignored.** A synthetic `webglcontextlost` used to count as
  a loss and finish every renderer on the canvas. Both listeners now return unless `isTrusted`
  (probe SY; at `a77fe0f9` SY's renderer is finished by the page's own event and the probe throws).
- **The loss count is on the canvas's record** (NV, above).
- **Collected canvases leave the armed set**: each create that arms a new canvas first drops the
  set's refs to canvases since collected, so it does not grow with every canvas a page ever made.
- **`js/web3d.d.ts`**: `createRenderer`, `onContextRestored` and `frame` say the same.

## The probes (`demo/probe.js`, run by `demo/check.mjs`): 80, all pass

Six new ones: DO (F1), NV (F2), MC (F4), RC (F3), TO (F5's `OffscreenCanvas`) and SY. Changed: LE's
ET (above), U's and Y's descriptions (Y's module that is not web3d is now refused before the
context), MS unchanged and passing with the new message, which still says "lost before web3d first
used this canvas" where it cannot tell, and now starts "the context is lost", as K3 and the seventh
critic's LC3 match.

### The control: today's probes against the code the twelfth critic judged

`../critic10-rerun/build-before-fix.sh <dir> a77fe0f9` builds `a77fe0f9` outside the repository;
`../critic11-rerun/tools/against.mjs` serves today's probe page with that `pkg/`
(`control/probes-at-a77fe0f9.json`): 73 of 80 pass. LE fails on ET (its create fails "lost"); DO
(listeners `{}` after the reopening, losses not cancelled, the contexts lost for good); NV (the new
page's copy refused its own canvas, "already has a live renderer"); MC (m1, m2a and m2b created, m3
leaves listeners and a context); RC (the second live renderer, and sealed and forged canvases
taken); TO (the old message); SY (the page's own event finishes the renderer).

## Every critic's probes, rerun (`rerun-critics.sh`; `rerun-critics.log`, `rerun/`, `results/rerun-*.json`)

```
PLAYWRIGHT_CORE=.../playwright-core/index.mjs THREE_BUILD=.../three/build bash rerun-critics.sh <port>
```

All 46 runs exited 0: the 24 scripts and pages of critics one to ten, the eleventh's six pages, the
twelfth round's four, and the twelfth critic's twelve (served from its own folder, which is left as
it is). Against the twelfth critic's own rerun (`../critic12/rerun/`, `../critic12/results/rerun-*`),
`../critic11/tools/bool-diff.mjs` finds 0 differing booleans in 28 of the 29 result files, and two in
the ninth's `run9.json`: N7a's and N7b's `bPrevented`, false to true, since web3d's listener now goes
on when the create is called (F4). `booleans.txt` has the whole comparison.

**By premise**, each unchanged from the twelfth critic's list, which agreed with them:

- the fourth's and fifth's X8: the fraction rounds to the handle itself below 2^50;
- the fifth's K1 and E1, the sixth's G1, G12 and G14: limits and layouts that changed (no 32-renderer
  limit, handles below 2^50, 2,048 generations a slot, a counter that goes round);
- the sixth's G8a and the eighth's E16: `<img>` and `bitmaprenderer` canvases refused by design;
- the seventh's LC5: a later loss cancelled by design;
- the eighth's `manyOnOneCanvas`: it reads a map that no longer exists (its intent passes in the
  eleventh critic's `intent.html`, rerun: `results/rerun-critic11-intent.json`);
- the ninth's N7b: its message hedges where web3d cannot tell whether the lost event came before the
  call (its other premise, its own listener seeing web3d's cancellation, now holds); N11: no browser
  call tells whether a canvas has a context without making one;
- the ninth's N10, N10c and N10e, and the tenth's `notop`: a frame's copy given a canvas of the top
  document, refused by design; their intent passes (`results/rerun-round12-intent.json`);
- the tenth's `state`: its colour-space and drawing-buffer cases read as their controls, which fail
  its green-pixel test too; `xtop` and the eleventh's A7: they look for a home's state where it no
  longer is; `state2`'s `storageFloat`: the buffer is put back to `RGBA8`.

**The twelfth critic's pages** against its own results:

| Page | Then | Now |
| --- | --- | --- |
| `docopen` | listeners erased, loss not cancelled, context lost for good, no hook, new renderer with no listener | each of those false; the control (a detached canvas) still recovers |
| `nav` | the new page's copy refused its own canvas | it draws (`newPageCopyGetsItsOwnCanvasAfterTheOldCopyWasCalled` true); the top's renderers still survive the navigation |
| `dd2` | a second live renderer both ways, the first clearing to the second's colour | refused both ways, its own red |
| `midcreate` | m1 and m2a created on a moved canvas, with listeners | refused, no listener (`{}`); m2b and m2c refused, with the new message |
| `blank` | `b2` and `b2top` lost for good after the reopening | `b2top` recovers; `b2`'s new create on the old canvas is "created" now (it was "lost"). Its `prevented` still reads false by its premise: its own listener, added in the reopening's own task, comes before web3d's re-added one, and reads `defaultPrevented` during dispatch, before web3d's runs; the restore that follows proves web3d cancelled it |
| `transfer` | the wrong message | the new message; the frame's copy still draws |
| `adopt`, `override`, `bf`, `f4`, `per`, `counter` | held | hold, 0 booleans differ. `counter`'s one blind spot (a removed frame's canvas reads `{}`) is DevTools', and no probe counts one |

## Gates

`../gates.sh 5666 5667`, recorded in `../gates.txt` (home written `<home>`), exited 0: every gate
held. It was run twice this round; the record is the second, on the final code (the first, before
the change that drops collected canvases from the armed set, also held every gate).

| Gate | Result |
| --- | --- |
| `cargo fmt --check` | exit 0 |
| clippy, CI's macOS row (the workspace, `-D warnings`) and its wasm32 step | exit 0, exit 0 |
| clippy `-p loadngo-web3d --all-features` on wasm32, aarch64 Linux, Windows MSVC and the host | exit 0 each |
| `cargo test --locked -p loadngo-web3d`, debug and release | 75 passed each |
| `build.sh` x3, the third from an empty target directory | byte-identical `pkg/`, no local path |
| `three-reference.mjs --check` | 20 lines verbatim, output matches |
| the CI step's YAML, now by its commands | the step found, and it runs the local gate's command |
| `check.mjs` | exit 0 |
| `check.mjs --blind` | exit 3, as required |

The builds (`sha256`):

- wasm 70,249 bytes (32,174 gzipped), `4d6594b5…`, the same bytes as `a77fe0f9`'s: no Rust changed;
- `gl.js` 22,570 bytes (7,617 gzipped), `6542cb39…`;
- `web3d.js` 66,468 bytes (20,570 gzipped), `65bb9a0c…`; 9,871 bytes more than `a77fe0f9`'s
  (2,934 gzipped), 5,670 of them in comment lines, the rest the new checks and messages;
- `web3d.d.ts` `c46f3cc2…`.

The rendered check (`../rendered-check.json`, `../rendered-check.log`):

- 64 draws and 320,000 triangles in every run, the clear colour exact, the centre lit, `getError` 0,
  no page error;
- 80 of 80 probes, no `INVALID_OPERATION`, the two pending errors raised on purpose, and 0 contexts
  lost to Chrome's limit;
- steady allocation 0 sampled bytes a frame over 601 frames, none in `pkg/`;
- the positive control 86.1;
- cold: 258.5 (the module's call wrappers), 30.5 (`web3d.js`), 2.3 (`gl.js`).

The blind run (`../blind-control/`): the control read 0.45, everything else held, exit 3.

Two edits came after the run, both comments: `gates.sh`'s header now points at this folder's
`rerun-critics.sh`, and the critics' rerun and the first gate run were made before the last comment
edits in `js/web3d.js` and `js/gl.js` (the second gate run built them).

## Timing

**Contended.** Other sessions' headless Chromes drew through most of the final gate run, one after
another, and a desktop Chrome window was open throughout
(`gates-contention.txt`, every 5 s; each Chrome is named only by whose it is, since another
session's command line names its own project). So the run's GPU times are not capacity figures:

- GPU p50 3.587, 0.584 and 0.431 ms, p95 7.655, 4.782 and 1.918 ms (1024x576 with MSAA, 1366x768,
  without MSAA);
- CPU p50 0.04, 0.04 and 0.045 ms, p95 0.055, 0.06 and 0.065 ms;
- every frame took 16.67 ms.

WEB3D.md's table is now the twelfth critic's quiet run (`../critic12/gates/rendered/`, with its
`contention-during.txt`), in place of the eleventh round's run with its two unsupported figures. The
GPU's work has not changed since: the wasm is the same bytes (`4d6594b5…`), and `frame()` gained
one property read in JavaScript, which the allocation gate runs through.

## The confidentiality scan

Kept outside this repository, since its pattern names what it looks for: over every file the
branch changes from `dev` and every untracked file (976, this folder and `../critic12/` included),
and the branch's commit messages, with the pattern widened as above. Zero hits on every line: names
and home paths, by `grep -a` and by `strings`.
