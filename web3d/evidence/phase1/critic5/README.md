# Fifth critic, web3d phase 1 (2026-11-03), generic evidence

Judged: loadngo `web3d-phase1` at `284194b6`. Chrome 156.0.8078.76 headless (`channel: 'chrome'`,
ANGLE over Metal, Apple M4 Pro); rustc 1.99.0. Anything about the application this renderer is built
for is kept in that application's private repository; one confidentiality finding is recorded there
only.

## Gates (all rerun here)

| Gate | Result | File |
| --- | --- | --- |
| `cargo fmt --check` (workspace) | pass | `gates.txt` |
| clippy `-D warnings`, `-p loadngo-web3d`, host, wasm32, aarch64-linux, windows-msvc (shared target and a fresh one) | pass | `gates.txt` |
| clippy as CI's macOS row (whole workspace) | pass | `gates-workspace-clippy.txt` |
| `cargo test --locked -p loadngo-web3d`, debug and release | 73 pass | `gates.txt` |
| `build.sh` twice, then once from an empty target directory | `pkg/` byte-identical to the pkg found (sha256), no local path in the wasm | `build.txt` |
| `three-reference.mjs --check` (three 0.180.0) | pass, 20 lines verbatim | `three-reference-check.txt` |
| `demo/check.mjs` | exit 0: 55/55 probes, steady 0.09 sampled B/frame, control 89.3 | `check.log`, `check/` |
| `demo/check.mjs --blind` | exit 1, as it must: control 0.5 against 40 | `check-blind.log`, `blind/` |
| the fourth critic's `run4.mjs` | X1-X7 pass; X8's one "failure" is `o + 2^-30`, which is `o` | `run4-rerun.txt`, `run4-rerun.json` |

## This critic's probes (`run5.mjs`, `probe-site/probe5.js`)

`pass` means the documented claim held.

| Probe | Result | What it shows |
| --- | --- | --- |
| X8 | pass | A representable fraction is refused (`4194306 + 2^-30` on a salt-0 mesh). Every object handle is at least `4 << 22 = 2^24`, where a double's step is 2^-28, so `o + 2^-30` rounds to `o`: the builder's reading of X8 is right. |
| C1 | **FAIL** | A matrix or vector passed as an Array is converted element by element by `TypedArray.prototype.set`: `createObject(mesh, mat, ['1','0',...])` returns a handle; 16 `valueOf`s run in one `setTransform`; strings, booleans and nulls are accepted by `setCameraLookAt` and `setCamera`. WEB3D.md "Numbers must be numbers. Nothing is converted, so no `valueOf` runs" and web3d.js:20 are false for Arrays. The busy flag still refuses re-entry, so this is a false claim, not a memory hazard. |
| C2 | **FAIL** (minor) | An Array's `length` is read twice (`_put`, then inside `set`): a Proxy answering 16 then 64 writes 64 floats into the exchange area (bounded by it), or makes `set` throw a bare RangeError. "Read once" is false for Arrays. |
| H5 | **FAIL** | Salt reuse: after renderer A (salt 2) is disposed and 31 more renderers come and go, renderer B gets salt 2 and issues the same number for its first object (83886081). `B.setVisible(oA, false)` is accepted and hides B's own object (the pixel went from green to black). "Handles ... are never reissued" and "another renderer's handle is refused" fail once a page has created 32 renderers. |
| R1 | pass | Rust refuses a staged array one float past its allocation, misaligned, pointing at the exchange area, with a length that overflows, or after its free; wrong-length, double and forged frees are ignored; a 4 GiB request returns 0. |
| R2 | pass | 300 creates that throw after staging (about 5 MiB each) leave the heap where it was. |
| T1 | pass | Texture option getters that call `frame()` or `dispose()` are refused; one that throws leaves the renderer usable. |
| T2 | **FAIL** | `createTexture` reads the source's size, then the options (caller code), then uploads the source as it now is. A getter that grows a 4x4 canvas to 64x64 gets a texture handle back and `GL_INVALID_VALUE` ("glTexSubImage2DRobustANGLE: Offset overflows texture dimensions"); a shrink would upload silently into a corner. |
| L5 | pass | A 503 then a 200 on the same URL: the second create loads. Two racing creates share one failed fetch, and the next loads. |
| Y5 | **FAIL** (minor) | A canvas whose WebGL2 context the page made with `antialias: false`: `createRenderer(c, { antialias: true })` is accepted and runs without MSAA (`info.antialias` false, 0 samples). The "must ask for the same, or is refused" rule covers only contexts web3d made. |
| B1 | pass | With a 2 GiB block allocated, the next staging allocation is at 2,148,537,040 (above 2^31); a mesh staged there draws and `getError` is 0. |
| E1 | pass | One object kept, then create/destroy until the table is spent: 4,194,049 handles (1 + 32,766 x 128) in 0.46 s, none twice in the first 19 slots, then "web3d: too many objects"; the first, a middle and the last handle are refused; the kept object still draws; other kinds still create. |
| K1 | pass | 32 live renderers get 32 distinct salts; the 33rd is refused ("32 renderers are live"), and its failed create's loss hook never fires; after one dispose, a create on that canvas works. Chrome loses the oldest contexts past 16 (17 loss events), so 32 is not the binding limit in practice. |

## The ten rewritten exports

`create`, `destroy`, `init`, `error_ptr`, `error_len`, `io`, `stats`, `caps`, `set_cpu_timing` and
`resize` were compared with `src/web.rs` as the fourth critic read it (before the deletion). They
match line for line apart from `web3d_create(salt)`, which the fix round added before the deletion
(null for a salt of 32 or more), and `caps`'s bit 0, written as `if caps.timer_query { CAP_TIMER }
else { 0 }` instead of `u32::from(caps.timer_query)` (the same value). Every null check, refusal and
status code is the same: none weakened.

## Other generic findings

- `docs/WEB3D.md` (Gates) and the CI comment say `src/web.rs` "holds all the crate's unsafe code".
  `src/pack.rs:248` has an `unsafe` block (`f32_bytes`) and `src/testing.rs:369-390` an `unsafe impl
  GlobalAlloc`. Both are sound; the sentence is false.
- `docs/WEB3D.md` says the real three.js comparison is within 2/255 with the stand-in and that
  "without it, the two metal cases differ by up to 8 levels". True, but two dielectric cases also
  exceed 2/255 without it (oblique glossy, blue 73 against 69; grazing 62 against 59,
  `critic2-rerun/report-probe.json`).
- "A failed create leaves nothing behind": it leaves the canvas resized when the failure comes after
  `setWidth` (only `web3d_init` can fail there). Minor.
- `build.sh` is reproducible for a given rustc and wasm-opt; neither version is recorded in `pkg/` or
  checked. Minor.
