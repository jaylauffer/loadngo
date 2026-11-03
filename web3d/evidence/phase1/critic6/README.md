# Sixth critic, web3d phase 1 (2026-11-03), generic evidence

Judged: loadngo `web3d-phase1` at `59b74ced`. Chrome 156.0.8078.76 headless (`channel: 'chrome'`, ANGLE
over Metal, Apple M4 Pro); rustc 1.99.0, wasm-opt 133. Anything about the application this renderer
is built for is kept in that application's own repository. Verdict: **FAIL**.

## Gates (all rerun here)

| Gate | Result | File |
| --- | --- | --- |
| `cargo fmt --check` | pass | `gates.txt` |
| clippy `-D warnings`: CI's macOS-row workspace command; `-p loadngo-web3d` on host, wasm32 (CI's command), aarch64-linux, windows-msvc, from an empty target dir | pass | `gates.txt` |
| `cargo test --locked -p loadngo-web3d`, empty target dir | 74 pass | `gates.txt` |
| `build.sh` twice, then from an empty target dir | `pkg/` byte-identical to the pkg found (sha256 of all four files); no local path in the wasm or JS | `build.txt` |
| `three-reference.mjs --check` (three 0.180.0) | pass, 20 lines verbatim | `three-reference-check.txt` |
| `demo/check.mjs` | exit 0: 64 draws, 320,000 triangles, 59/59 probes, steady 0 sampled B/frame, control 87.7 | `check.log`, `check/` |
| `demo/check.mjs --blind` | exit 1, as it must: control 0.5 against 40 | `check-blind.log`, `blind/` |
| every earlier critic's scripts (`rerun-earlier.sh`) | first 29/29, second 14/14 and its ABI probe, third 10/10, fourth X1-X7, fifth C1, C2, H5, R1, R2, T1, T2, L5, Y5, B1 pass | `earlier/`, `rerun-earlier.log` |

GPU times here were taken while another session's browser checks ran; they are not a capacity
measure.

## The three probes said to be obsolete

The builder's explanation (`../critic5-rerun/README.md`) is right for all three; the intent of each was
re-tested against the new layout (`run6.mjs`, `probe-site/`):

- **X8** (fourth and fifth critics): every handle is at least 2^33, where a double's step is 2^-19, so
  `o + 2^-30` is `o` (critic 4's run hides its own object with it). G2 and G3 try fractions a double
  does hold there (`o ± 2^-19`, `o ± 0.5`), negatives, -0, NaN, ±Infinity, 2^53 and above, other
  serials, kinds 0 and 5-7, other generations and indices, strings, Number objects, BigInts, Arrays
  and `valueOf` objects: all refused through the wrapper, and every number refused straight into the
  export (`web3d_object_set_visible` returns 1); the object stays drawn.
- **K1**: there is no 32-renderer limit to refuse. The probe's second create is on a canvas whose
  renderer is still live, so the "one renderer per canvas" refusal is correct. G13: 24 live renderers
  all create; Chrome drops the oldest 8 contexts (its 16-context limit); the live ones draw; failed
  creates leave no listener; a new canvas works afterwards. (But see finding 1.)
- **E1**: rerun here, 1,073,709,057 handles (1 + 65,534 x 16,384) in 123 s, then "too many objects";
  first, middle and last refused; the kept object still draws; `getError` 0. G12: one slot through
  all 16,384 generations, then the next slot; none issued twice; its first and last handles refused.

## This critic's probes (`run6.mjs`; `run6.json`, `run6-extra.json`)

`pass` means the documented claim held.

| Probe | Result | What it shows |
| --- | --- | --- |
| G1 | pass | Handles decode exactly as `serial << 33 \| kind << 30 \| generation << 16 \| index + 1`, all safe integers below 2^53. |
| G2, G3 | pass | Forgery, as above (X8's intent). |
| G4 | pass | Two live renderers refuse each other's handles; 300 renderers come and go: 1,200 handles, none twice, every one refused by both live renderers (9,600 tries over four calls), which still draw. |
| G5 | pass | Two module copies (`web3d.js?copy=a`, `?copy=b`) take distinct serials, refuse each other's handles, share canvas ownership and context settings; a copy imported later carries the serials on. |
| G6 | **FAIL** | A same-origin iframe runs its own copy of the module in its own realm, with its own counter. Its first renderer had serial 1, as the top page's first did, and issued the same three handles (9663676417, 11811160065, 12884901889). The top renderer accepted the iframe's object handle and hid its own object (pixel green to black). |
| G7 | pass | A Proxy over an Array matrix is read exactly `length`, `0`..`15`, once each; an accessor element runs once; a hole is refused; element getters that call `frame()` or `dispose()` are refused as re-entrant; NaN, 1e39 (inf as f32) and Infinity are refused by the Rust side in every setter; BigInt64Array, detached arrays and array-likes are refused; mesh descriptors and morph lists read each field once. |
| G8a | pass | Plain images, ImageData, a closed ImageBitmap (refused, size 0), a broken image (refused), a cross-origin image (refused, "refused the texture upload", `getError` 0). |
| G8b | **FAIL** | `<img srcset="... 2x">` over an 8x8 bitmap: `naturalWidth` is 4, so the texture is made 4x4; WebGL uploads the 8x8 bitmap: `GL_INVALID_VALUE` ("Offset overflows texture dimensions"), yet `createTexture` returns a handle, and the texture is black. |
| G8c | **FAIL** | A `VideoFrame` coded 4x4 with display size 8x8: an 8x8 texture with the frame in one corner, no error, a handle. With display size 2x2: `GL_INVALID_VALUE`, a handle, a black texture. |
| G9a | **FAIL** | A page-made context with `depth: false` (antialias matching) is accepted. A near green quad drawn first and a far red quad drawn after it read red; on a web3d-made context, green. No error. |
| G9b | pass | A page-made WebGL1 or 2d context is refused, twice with the same message (not left claimed), size kept. |
| G9c | **FAIL** | See finding 1. |
| G10 | pass, with a note | A context lost during instantiation: size restored, no listener left. The error reads "the plain shader program failed to compile or link (the browser console has the compiler log)"; the console has no log. |
| G11 | pass | One fetch for a URL used three times (one spelt `/count/../count/mod.wasm`); a 200 that is not wasm is fetched again. |
| G12 | pass | E1's intent, as above. |
| G13 | pass | K1's intent, as above. |
| G14 | pass | Serial 2^20-1 (reached by setting the shared counter): handles exact (up to 9007194959773697), draws; the next create is refused ("1048575 renderers"), twice, size kept. |
| G16 | pass | A renderer on an `OffscreenCanvas` draws. |

## Findings, most severe first

1. **A create during a context loss poisons the canvas's recorded antialias, and the documented
   recovery is then refused** (`js/web3d.js:375-377`; G9c, G13). On a lost context
   `getContextAttributes()` returns null, so `build` records antialias `false` for the canvas
   (`CONTEXTS.set`) and throws a mismatch, not "the context is lost". With the default
   `antialias: true`: the create during the loss says "this canvas's WebGL context has antialias
   false"; after `onContextRestored` and `dispose()`, `createRenderer(canvas, {...})` is refused with
   the same false message; asking for `false` is refused with "has antialias true"; only a third try
   works. The same wrong message comes from a canvas whose context Chrome dropped (too many
   contexts). Against WEB3D.md (Context loss) "every create throws" [that the context is lost] and
   "create a new one after onContextRestored", and the claim that a renderer is refused only for
   another antialias than the context has. Probe K misses it because it asks for `antialias: false`.
   Fix: in `build`, throw LOST before reading attributes when `gl.isContextLost()`, and never record
   a setting from null attributes; add a K case with the default options.
2. **`createTexture` makes the texture at a size WebGL does not upload** (`js/web3d.js:180-208`,
   `IMAGE_KINDS`; `js/gl.js:224-236`; G8b, G8c). `naturalWidth` is not the bitmap's width when an
   image has a `srcset` density or `sizes`, and `displayWidth` is not the uploaded size of a
   `VideoFrame`. The shim's re-check compares the same getters, so it passes, the upload fails or
   fills a corner, the handle is returned and `GL_INVALID_VALUE` is left in the context: the fifth
   critic's T2 outcome, by another route. gl.js:224-225 says the check prevents exactly this. Fix:
   size VideoFrames by `visibleRect`; for images, decode to an ImageBitmap first, or check
   `getError()` after the upload (once a texture, not per frame) and refuse.
3. **A page-made context without a depth buffer is accepted and draws in the wrong order**
   (`js/web3d.js:374-377`; G9a). The fix for the fifth critic's Y5 checks antialias only. Fix: refuse
   a context whose attributes lack `depth` (and say which settings are required).
4. **"No handle is issued twice in a page" is false across realms** (WEB3D.md:155-168,
   `js/web3d.js:13-14`, `:25-26`, `:244-259`; `src/slots.rs:10-11`, `:16`; `src/web.rs:424-427`;
   `js/web3d.d.ts:9-11`; G6). The counter lives on `globalThis`, which an iframe, or a worker drawing on
   an OffscreenCanvas, has its own of. Low risk (handles passed between realms), but the claim is
   false as written. Fix: say "in a realm (a window, an iframe or a worker each count their own)",
   or start each realm's counter at a random serial so a collision is unlikely.
5. **Two comments in live sources are false now.** `demo/check.mjs:52` "all 60 objects in view" (the
   demo has 64). `demo/probe.js:287-288` "The wasm boundary converts numbers with ToInt32": handles now
   cross as doubles.

## Notes

- WEB3D.md "By hand" (lines 462-471) lists the first four critics' scripts, not the fifth's
  `critic5/run5.mjs`, which the Results table cites.
- `js/web3d.d.ts:4-5` types matrices as `ArrayLike<number>`; the wrapper refuses array-likes that are
  not Arrays (G7), so TypeScript accepts what throws.
- A `wasm` option that is an object with `toString` is accepted, and its `toString` runs during the
  build, after the options were read (G11). Nothing breaks; it is outside the documented types.
- A `WebAssembly.Module` from another realm fails `instanceof` and is then fetched as the URL
  "[object WebAssembly.Module]" (`js/web3d.js:278`).
- The dolores figures are for the earlier load and predate two shader changes, as WEB3D.md says:
  phase 1 has no measurement of its present shaders on the hardware it is built for.
