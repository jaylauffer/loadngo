# The fourteenth critic's findings: the fifteenth round, stopped partway (2026-11-04)

**Status: incomplete.** The session ended about a third of the way through the round. Some code and
comment changes are made, in `js/web3d.js` and `js/web3d.d.ts`. No gate has run on them, no probe
has run, and WEB3D.md is unchanged. Nothing is committed or staged. The fourteenth critic's folder
(`../critic14/`) is untouched.

The working tree is loadngo `web3d-phase1` at `9a4ab9c7`, plus the changes listed under "Files".

## The page contract (drafted here, not yet in WEB3D.md)

The supported model stays as it is: one copy of web3d, in the canvas's own realm, drives a canvas,
and it takes an element only if the element's owner document is this window's. web3d supports a
**cooperative page**: one that does none of the following. web3d has no code against these cases,
and none of them is tested as supported.

1. **It does not stop or reorder a WebGL context event before web3d's listener has run.** This
   covers `webglcontextlost` and `webglcontextrestored` on a canvas web3d has armed.
   - It does not call `stopPropagation()` or `stopImmediatePropagation()`, or set
     `cancelBubble = true`, in a capturing listener on a node the event passes through before the
     canvas: the window, the document, an ancestor element, or the shadow root the canvas is in.
   - It does not call `stopImmediatePropagation()` in a listener on the canvas that sits ahead of
     web3d's. A page listener gets ahead in three ways: it was added before web3d's first create on
     the canvas in this realm; or after a failed create took web3d's listeners off, before the next
     create; or after a `document.open()` erased web3d's listeners, before web3d's observer put
     them back.
   - It does not reopen the canvas's document (`document.open()`, or `document.write()` once the
     document has loaded) from inside a listener for these events.

   **If it does:** a lost event that web3d's listener never sees is not cancelled, so the context
   is lost for good and no `onContextLost` is called (`../critic14/results/c3-reopen.json`, R1 and
   R5). A restored event that web3d's listener never sees still brings the context back, but no
   `onContextRestored` is called then. With this round's code change (F2(a) below), the hooks that
   restore owed come at the canvas's next restore, if one comes, whatever the page does in between,
   and each is called once.

   Measured, though not supported: a reopening from a listener anywhere other than on the canvas
   ahead of web3d's is still handled, because the observer puts web3d's listeners back before the
   event reaches the canvas (`../critic13-rerun/results/reopen-paths.json`).

   Still to measure: the draft says that `stopPropagation()` in a canvas listener ahead of web3d's
   does not stop web3d's own listener, which is on the same node. That follows from the DOM
   standard but was not run this round.

2. **It does not shadow or patch a built-in that web3d reads.** This covers a global declaration of
   any kind named after one (`var`, `let`, `const`, `function` or `class` at a classic script's top
   level: `let` and `class` shadow a bare global name for module code too), an assignment to one,
   `Object.defineProperty` or `delete` on one, and a patched prototype method or accessor.
   - **If it does:** what happens depends on the built-in; there is no general guarantee.
   - **Still to do:** list the built-ins by name in WEB3D.md. Every built-in read at load or call
     time in `js/web3d.js` and `js/gl.js` belongs on that list, including the WebGL2 methods, which
     `gl.js` calls on the prototype each time.

3. **It does not move a driven canvas to another document.**
   - **If it does:** `frame()`, `resize()` and the creates throw while the canvas is away, and work
     again once it is back. If the other document was reopened while the canvas was in it, the
     canvas comes back with none of web3d's listeners, so its next loss is for good.
   - Handing a canvas between windows between renderers (dispose, move, create) stays supported.

The other limits outside this contract stay as WEB3D.md states them: the page leaves web3d's own
state under `Symbol.for('loadngo-web3d')` and a renderer's private fields alone, and a copy run in
an extension's isolated world is out of scope.

## Each item: done or not

| Item | State |
| --- | --- |
| F1, the removed-frame check | **Code done, not run.** `ownRealm()` now reads the built-in `top` getter, taken at load (`TOP`), instead of `closed`: `GLOBAL[SHARED_KEY] === REALM_STATE && (TOP === undefined \|\| TOP.call(GLOBAL) !== null)`. This differs from the critic's tried line (`GLOBAL.top !== null`) on purpose. In a window the two are the same, since `top` is an own, non-configurable accessor from the window's start. In a worker, which has no built-in `top`, a page's `globalThis.top = null` would turn the critic's line false, and so refuse every canvas; it leaves `TOP` undefined. `CLOSED` is gone. The comment above `GLOBAL` and the comment on `ownRealm()` are corrected. Not run against the 80 probes or any critic's pages; the module only loads in Node. **Left:** WEB3D.md 244-248 and 299 ("`closed`"), and the 772-802 mentions; an NV case for a frame that declares `function closed() {}` (planned as a new `demo/probe-frame-closed.html`; not written); measuring when a closed popup's `top` reads null (the new message assumes "once the browser has discarded it") |
| F2(a), a missed restore's hooks | **Code done, not run.** It is the critic's tried fix, verbatim: `take()` runs `if (controller.current) waitForRestore(controller, controller.current.onRestored);` before setting `current`. On top of that, the lost listener now clears `current.onLost` after calling it, so a renderer still current at a later loss (one that missed the restore in between) gets no second `onContextLost`. That makes the critic's "neither disposes nor creates" case read "first lost", then "first restored" at the second restore. Inside the contract nothing changes, since a handled restore clears `current`. **Left:** rerun `../critic14/site/c3-reopen.html` and `c6-r4-fix.html` against the new `pkg/` |
| F2(b), the wrong reasons | **Not done.** WEB3D.md 489-515 and `../critic13-rerun/README.md` ("Why not the critic's code fix") still give the reasons the critic disproved. Rewrite them as the contract's first item and its stated limit. Also fold in the nit at 496 (the third way a listener gets ahead), which the contract draft above already lists |
| F3, stopping propagation | **Comments done; WEB3D.md not.** The comment at the top of `js/web3d.js` now says the controller cancels every lost event "that reaches its listener" and names the contract. So do the `controllerFor` comment and the `onContextRestored` comment in `js/web3d.d.ts`. **Left:** WEB3D.md 528 ("however the page orders its own listeners") and 519-526 |
| F4, the browser version | **Not done.** `../critic13-rerun/README.md` lines 3-4 still say Chrome 156.0.8078.80; every result of that round records 157.0.8103.38. WEB3D.md 768-777 should say that the browser changed (156 to 157) and that the GPU conclusion still holds: the fourteenth critic's near-quiet run on 157 matched the table within 0.03 ms at the median |
| Polish: a closing popup's message | **Code done.** `NAVIGATED` now also names "a window that has been closed", and keeps "navigated away", which probe NV matches. Every "navigated away or its frame has been removed" in `js/web3d.js` and `js/web3d.d.ts` now names closed windows too |
| Polish: the unwrapped `.d.ts` line | **Done** (the `createRenderer` comment) |
| The CI-step gate | **Not done.** Planned, cheap tightenings of `../ci-step.rb`, one per critic attack: refuse any `env` key at the step, job or workflow level outside an allow-list (`CARGO_TERM_COLOR`, `LOADNGO_CAS_MIN_FREE_GIB`); refuse any `run` that writes `GITHUB_ENV` or `GITHUB_PATH`; refuse a job `if`; allow no `push` or `pull_request` filter except `paths-ignore`, and require that no pattern in it matches the crate's sources, `Cargo.toml`, `Cargo.lock` or the workflow. Then state in WEB3D.md 945-953 exactly what the gate checks and what it does not: the actions the job uses, the toolchain step, the working directory, and repository config such as `.cargo/config.toml`. The control script also needs fixing: each changed copy should have to fail with the gate's own refusal (a distinct exit code and the expected reason), so that a Ruby error no longer counts as an expected failure |
| The confidentiality scan | **Not done** (it lives in the game's repository). The scan script is unchanged, so it still misses all ten of the critic's controls. A draft of the wider pass, compiled but never run, sits in that repository's round-15 folder, with a README giving its state. No controls script was written, and the branch was not scanned this round |
| Other WEB3D.md updates | **Not done.** The status line ("thirteen critic rounds"), the critics table (a fourteenth row), the probe list, the "By hand" list, and the Limits section (point it at the contract) |

## Gates

**None of the round's gates has run.** No `cargo` command ran, nor `build.sh`, the rendered check,
the blind run, the CI-step gate, or the critics' reruns. `../gates.txt` is still the fourteenth
round's record. `pkg/` is that round's build: `js/` now differs from `pkg/`, so `pkg/` must be
rebuilt before any rendered check.

The only check run, without the GPU: `js/web3d.js` loads as a module under Node 24 and exports
`Renderer`, `compile` and `createRenderer`.

The Rust source is unchanged, so the wasm should come out byte for byte the same (`4d6594b5…`). The
GPU's work is then unchanged too, and a contended timed run, labelled as contended, would be
acceptable.

## What's left, in order

1. Finish WEB3D.md: put the contract above near the top of a supported-model subsection, after the
   API table. Then do F1's, F2(b)'s, F3's and F4's corrections and the other updates in the table.
   Correct `../critic13-rerun/README.md` (F4's Chrome version, F2(b)'s reason), with a note that it
   was corrected in this round.
2. Add the NV case: a frame declaring `function closed() {}`, removed during a create that has
   already compiled the module, and called after its removal. This window's copy should take both
   canvases, draw on each, and recover each.
3. A page of this round's own under `site/`, testing:
   - `top` in a removed frame;
   - `top` in a closing popup, right after `close()` and later;
   - a module worker that sets `globalThis.top = null` before it imports web3d;
   - `stopPropagation()` and `cancelBubble` at each place in the event's path;
   - the critic's four R4 variants with this round's code.
4. Tighten the CI-step gate and fix its control script (above).
5. Claim the Mac GPU on the board. Then run `../gates.sh <port> <port>` with ports from 5661-5679,
   and the critics' reruns: a copy of `../critic13-rerun/rerun-critics.sh` that adds the fourteenth
   critic's `c1`-`c7` pages (`c2` needs its no-DevTools driver). Compare booleans with
   `../critic14/`, and argue each case that now differs or falls outside the contract.
6. Rerun the confidentiality scan on the final branch.

## Files

Changed in this round:

- `web3d/js/web3d.js`: F1 (`TOP` replaces `CLOSED`, in `ownRealm()`); F2(a) (`take()`, and the lost
  listener's single `onContextLost`); the `NAVIGATED` message; comments for F1, F3 and closed
  windows.
- `web3d/js/web3d.d.ts`: the rewrapped `createRenderer` comment; the `onContextRestored` comment
  (F3, the contract); closed windows.
- `web3d/evidence/phase1/critic14-rerun/README.md`: this file.

Not changed: `docs/WEB3D.md`, `web3d/demo/`, `web3d/pkg/`, `web3d/evidence/phase1/ci-step.rb`,
`../gates.txt`, and every earlier critic's folder.
