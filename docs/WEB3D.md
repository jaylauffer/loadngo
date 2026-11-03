# web3d: a lean WebGL2 renderer in Rust and WebAssembly

`web3d/` (crate `loadngo-web3d`) is a small 3D renderer for the browser, written in Rust, compiled
to WebAssembly, and drawing through WebGL2. It is built for small scenes with skinned,
morph-targeted characters, on GPUs as slow as a Raspberry Pi 5's V3D 7.1, at 30 fps. The application
loads its data with three.js, so matrices and light units follow three.js r180.

**Status:** phase 1 of 5, the skeleton, is built (2026-11-03) and has been through five critic
rounds. It draws a procedural test scene and reports fps, GPU ms and CPU ms.

**Sources.** Each figure below names where it comes from:

- an evidence file under `web3d/evidence/`;
- a file in this repository;
- or three.js r180's source.

## The plan

| Phase | What | Status |
| --- | --- | --- |
| 1 | Skeleton: crate, WebGL2 binding, renderer API, lean shaders, GPU and CPU timing, demo, build, gates | **Built** (`web3d/evidence/phase1/`, `phase1-dolores/`) |
| 2 | A character drawn static, from the data three.js's `GLTFLoader` produces: skinned meshes, normal and metallic-roughness maps, alpha-tested hair cards, a richer light model | Next |
| 3 | The character animated: per-frame bone matrices and sparse morph weights | Planned |
| 4 | A room with its lighting baked offline, drawn unlit | Planned |
| 5 | A side-by-side against a three.js baseline on dolores, at the same view and canvas | Planned |

## Architecture

```
the application                       web3d/pkg/                         GPU
three.js GLTFLoader ──typed arrays──▶  web3d.js  Renderer class
                                        │ C-ABI exports (numbers only)
                                        ▼
                                      loadngo_web3d.wasm  (Rust: scene, culling, sorting,
                                        │                  packing, uniform data, state cache)
                                        │ imports, module "web3d_gl" (numbers only)
                                        ▼
                                      gl.js  WebGL2 context + handle tables ─────▶ WebGL2
```

**The crate** has no dependencies. Its modules:

| Module | What |
| --- | --- |
| `math` | Column-major 4x4 maths, laid out as three.js's `Matrix4.elements`: multiply, perspective, look-at, inverse, normal matrix, frustum planes |
| `pack` | Vertex packing, index narrowing, normals for meshes without them, bounding spheres, std140 layouts, the sRGB curve |
| `slots` | Generational handle tables, tagged by kind and renderer: a stale or forged handle, or one of another kind or renderer, is refused, never aliased |
| `shaders` | The GLSL ES 3.00 sources, the program set, and `shade`, the lighting on the CPU for tests |
| `timer` | GPU time from `EXT_disjoint_timer_query_webgl2`, on a ring of 8 reused queries |
| `renderer` | `Renderer<G: Gl>`: meshes, textures, materials, objects, camera, lights, `frame()` |
| `gl` | The `Gl` trait (every GL call the renderer makes) and the WebGL2 enums |
| `web` | wasm32 only: the `web3d_gl` imports, `WebGl` (the `Gl` implementation over them), and the `web3d_*` exports |

- **Everything except `web`** is plain Rust, generic over `Gl`. Its 74 unit tests drive it against a
  recording mock, so no test needs a GL context (`web3d/evidence/phase1/gates.txt`).
- **`web`** compiles only for wasm32. One CI step checks it (see Gates).

**The JS shim, `js/gl.js`,** creates the WebGL2 context and provides the module's 43 imports
(counted from `pkg/loadngo_web3d.wasm`). It keeps the tables that turn small integers into WebGL
objects.

- **Byte data** crosses as a pointer and a length into wasm memory. The shim reads it in place
  through one cached `Uint8Array`, remade only after the memory grows.
- **Pointers and lengths** are read unsigned (`>>> 0`), so a heap past 2 GiB still works.
- **Strings** are decoded only while programs are built, and for error and log messages.

**The wrapper, `js/web3d.js`,** loads the module and wraps its 28 exported functions in a `Renderer`
class (counted from the same file). It checks every input before it crosses into wasm (below).
`js/web3d.d.ts` types it, with a branded type per handle kind, so TypeScript catches a swapped
argument.

**Why no wasm-bindgen.** wasm-bindgen 0.2.130 cannot join this workspace offline:

- the workspace's lock already holds 0.2.106 through other crates' wasm32-only dependencies;
- js-sys pins it exactly;
- pinning 0.2.106 fails the 0.2.130 CLI's schema check.

The resolver's refusal and the version facts are in `web3d/evidence/phase1/wasm-bindgen-conflict.txt`.
The raw ABI that replaces it is plain functions over numbers and slices of memory:

- no generated glue;
- no externref table;
- full control of what the JS side allocates.

## The API

JS (`web3d.js`). Colours are linear, matrices column-major.

| Call | What |
| --- | --- |
| `createRenderer(canvas, { width, height, antialias, cpuTiming, wasm, onContextLost, onContextRestored })` | One renderer per canvas: a second is refused while the first is live, since both would share one WebGL context. A canvas keeps its first WebGL context and that context's antialias setting, whoever made it, so a renderer must ask for the antialias the context has, or is refused |
| `createMesh({ positions, normals, uvs, indices, joints, weights, morphTargets })` | One glTF primitive |
| `createTexture(source, { srgb, mipmaps, wrapS, wrapT })` | From an `ImageBitmap`, image, canvas, `OffscreenCanvas`, video, `ImageData` or `VideoFrame`, so the browser decodes WebP and JPEG; wrap modes `'repeat'`, `'clamp'` or `'mirror'` |
| `createMaterial({ baseColor, texture, alphaMode, alphaCutoff, doubleSided, roughness, metallic })` | `alphaMode` `'opaque'`, `'mask'` or `'blend'` |
| `createObject(mesh, material, matrix)`, `setTransform`, `setVisible` | A mesh placed with a material |
| `setCamera(view, projection)`, `setCameraLookAt(eye, target, fovY, near, far)` | three.js's `matrixWorldInverse` and `projectionMatrix`, or a look-at |
| `setKeyLight(x, y, z, r, g, b, intensity)`, `setAmbient(sky, ground, intensity)` | One directional light and one hemisphere light, in three.js r180's units |
| `setClearColor`, `resize(width, height)`, `frame()` | `frame()` draws; call it from `requestAnimationFrame` |
| `gpuMs`, `cpuMs`, `readStats(out)`, `setCpuTiming(on)` | The last frame's numbers; `readStats` fills a reused `Float64Array(8)` |
| `destroy*`, `dispose` | Destroying something still in use is refused |

### The boundary rules

**Inputs are checked before they cross into wasm** (`js/web3d.js`). Page code that patches
built-ins or reaches into a renderer's private fields is out of scope; a caller's data is not.

- **Typed arrays are recognised by their internal slots,** through the built-in
  `%TypedArray%.prototype[Symbol.toStringTag]` and `length` getters taken when the module loads, never
  by `instanceof` or the array's own properties. A Proxy over a `Float32Array` is refused, and a
  subclass that lies about `length` is staged at its real length (probe Q). Data is copied with the
  built-in `TypedArray.prototype.set`, which reads the source's own storage.
- **A caller's object is read once.** Each field of a descriptor or options bag, and an Array's
  `length` and each of its elements, is read once into a local before anything is checked or staged
  (probe C). `createTexture` reads its options first and the source's size after them, and the shim
  checks the size again just before the upload, refusing a source that has changed (probe T).
- **Numbers must be numbers,** in arguments and in the elements of a matrix, vector or colour given
  as an Array. Nothing is converted, so no `valueOf` runs (probes C and U).
- **Re-entrancy is refused.** A getter or Proxy trap that runs while a method reads its arguments,
  and calls the renderer (`dispose` included), gets "a renderer method was called while the renderer
  was reading another call's arguments", so no export ever runs on a freed renderer (probe U).
- **Handles** must be whole numbers from 1 to 2⁵³−1, and cross into wasm as exact doubles. A
  fraction, a string or a Number object is refused here, anything else by the Rust side (probe H).
- **Floats** must be `Float32Array` where floats are meant: positions, normals, uvs, morph deltas.
  Integer data is refused, not reinterpreted. Positions, normals and uvs must be finite.
- **Indices** are `Uint8Array`, `Uint16Array` or `Uint32Array`, within the vertex count.
- **Joints and weights** are checked now and drawn from phase 3:
  - joints are `Uint8Array` or `Uint16Array`;
  - weights lie in 0 to 1 and sum to 1 within 0.02 per vertex (`WEIGHT_SUM_TOLERANCE`);
  - normalised integer weights are dequantised first.
- **Morph targets:** 1 to 256 (`MAX_MORPH_TARGETS`), with the count's arithmetic checked.
- **A drawing buffer** is a whole number of pixels from 0 to 32,768 a side (`MAX_DRAWING_BUFFER`).
  The viewport follows the drawing buffer the browser actually makes.
- **Colours and light values** must be finite. A NaN clear colour is refused.

**The Rust side checks staged arrays too.** `web3d_alloc` records each staging allocation (zeroed),
and `web3d_mesh_create` refuses an array whose pointer and length do not lie inside one of them.
`web3d_free` frees only an exact live allocation and ignores anything else. So a wrong length from
JS is an error, not a slice past an allocation or a bad free (`src/web.rs`; probe Q calls the export
directly).

**Return codes:**

- calls that create return a handle, as a double, or 0 on failure;
- other fallible calls return 0 on success and 1 on failure;
- either way, the reason is in `web3d_error_ptr` and `web3d_error_len`, and the wrapper throws it;
- a null renderer pointer is refused by every export.

**Numbers past 2³¹.**

- Integers the module sends to JS as `i32` stay below 2³¹, where they would arrive negative:
  drawing-buffer sides are capped at 32,768, and textures at the GPU's maximum.
- Handles cross as doubles, below 2⁵³, so they are exact.
- Pointers and lengths into wasm memory are read unsigned by the shim, so a heap past 2 GiB works.

**Handles name one kind of thing in one renderer, and no handle is issued twice in a page**
(`src/slots.rs`). A handle is `serial << 33 | kind << 30 | generation << 16 | (index + 1)`, below
2⁵³:
- **serial** (20 bits): the renderer's. The wrapper takes it from a page-wide counter, shared by
  every copy of the module, that never repeats, so a page may make 2²⁰−1 renderers;
- **kind** (3 bits): mesh, texture, material or object, never 0, so a bare small number is no handle;
- **generation** (14 bits) and **index** (16 bits: up to 65,535 live values a kind).

A table accepts only its own kind and serial, so a material handle passed as a mesh, or another
renderer's handle, live or disposed, is refused with an error that says which (probes W and H6). A
slot that has served its last generation retires instead of wrapping, so a stale handle stays
refused for the renderer's life; a table issues about a billion handles. Since serials never repeat,
no two renderers in a page issue the same handle: probe H6 makes 102 renderers on one canvas, 406
handles, none twice.

**After `dispose()`** every method throws "web3d: renderer disposed", and the canvas is free for a new
renderer.

**A failed create leaves nothing behind** (probes Y and Y2). Sizes and options are checked before the
WebGL context is made. A failure after that (a module that is not this one, an init that fails)
destroys the renderer, removes the shim's context-loss listeners, releases the canvas and puts its
size back. A canvas whose context, made by the page or by web3d, has another antialias setting
than the one asked for is refused.

**The compiled module is cached per URL, and a failure is not cached** (probe L). A failed fetch is
fetched again by the next create, and a later `wasm` option with another URL compiles that URL.

**Context loss.** Once the WebGL context is lost:

- `frame()` returns false, and every create throws;
- the renderer is finished, because its objects belonged to the lost context;
- dispose it and create a new one after `onContextRestored`;
- disposing the old renderer sends nothing into the restored context; its shim only forgets its
  handles.

Two loss-and-restore cycles on one canvas keep `getError()` at 0
(`web3d/evidence/phase1/critic2-rerun/run-critic.txt`, K1 and K2). Chrome allows `restoreContext()`
only after the lost event's dispatch has returned, which the loss probe in `demo/probe.js` waits for.

## A frame

`frame()` does these steps in order:

1. Collect GPU timer results and begin this frame's query.
2. Clear colour and depth.
3. Upload the `Frame` uniform block and the dirty bytes of the object and material blocks, one
   `bufferSubData` each.
4. Cull objects by bounding sphere against the view frustum.
5. Sort by a 64-bit key:
   - opaque draws, then alpha-tested, by program, then material, then front to back;
   - then blended draws, back to front.
6. Draw, sending only state changes.
7. Invalidate the depth buffer, so a tile-based GPU need not write it back.
8. End the query and flush, when one was begun.

| Uniform block | Size | When written |
| --- | --- | --- |
| `Frame` | 144 bytes | Every frame |
| `Object` | 128 bytes | When the object moves |
| `Material` | 32 bytes | When the material is created |

At start-up the renderer reads each program's uniform offsets back from the driver and refuses to run
if they differ from its std140 packing.

**No per-frame allocation.**

- **Rust.** The unit test `a_frame_allocates_nothing` counts allocations with a counting global
  allocator across 30 frames of moving camera, objects and light: zero (`src/renderer.rs`).
- **JS.** With CPU timing and the readout off, the page's scripts allocated **0.07 sampled bytes a
  frame** at steady state, over 601 frames, while the camera and three objects moved, their matrices
  and vectors plain Arrays as three.js holds them
  (`web3d/evidence/phase1/rendered-check.json`, `allocation`). The check fails:
  - above 1;
  - if a sampling window holds fewer than 90% of the frames 60 Hz would give;
  - **if the profiler is blind.** Chrome accepts an unknown sampling parameter silently, so the
    "objects already collected" flags cannot be trusted by the call succeeding. A positive-control
    window has the demo make one short-lived Array of 8 numbers a frame, and the check fails unless
    the profiler reports at least 40 sampled bytes a frame from it. It read 89.4. With the flags
    left out (`--blind`) it read 0.6 and the check failed (`evidence/phase1/blind-control/`).
- **Cold frames differ.** In the first 10 s the module's call wrappers showed 255 sampled bytes a
  frame, `web3d.js` 30.2 (copying Array elements one by one boxes them until V8 optimises the loop)
  and `gl.js` 2.4 (same file).
- **CPU timing costs boxed numbers:** its two `performance.now()` readings, 20.8 sampled bytes a frame
  in isolation (`web3d/evidence/phase1/cpu-timing-allocation/on.txt`; that line is absent from
  `off.txt`). In the demo, the check's timing window reads more (`rendered-check.json`, reported,
  not gated). So it is off by default; a perf readout turns it on.

## Vertex data

A tile-based GPU bins triangles in a pass that needs only positions. So a mesh is two streams:

- **positions,** `f32 x 3`, 12 bytes, alone;
- **attributes, interleaved:** the normal as `INT_2_10_10_10_REV`, and UVs as normalised `u16` when
  every coordinate of the mesh lies in 0 to 1, else `f32` (chosen per mesh).

Whether the split pays on V3D is to be measured on dolores.

**Indices** are `u16` when the mesh has at most **65,535** vertices, else `u32`. WebGL 2 always has
primitive restart on (the WebGL 2.0 specification, "Primitive Restart is Always Enabled"), so `u16`
index 0xFFFF ends a primitive instead of naming vertex 65,535. The probes cover this:

- they draw meshes of 65,535, 65,536 and 65,537 vertices;
- a raw-WebGL control beside them shows 0xFFFF restarting (`web3d/evidence/phase1/critic3-rerun/run3.txt`).

## Shading

The program set is `plain`, `textured` and `textured-mask`, from one GLSL ES 3.00 source with
`#define` switches, compiled at start-up.

**A fragment is shaded exactly as three.js r180's `MeshPhongMaterial`** would be under one
`DirectionalLight` and one `HemisphereLight`, in three's physically based units:

- **Diffuse:** albedo/π times the irradiance.
- **Specular:** three's `BRDF_BlinnPhong` with Schlick Fresnel.
- **The exponent** comes from roughness by web3d's own match to GGX: `alpha = r²`,
  `n = 2/alpha² - 2`, kept within 1 to 2048.

**What carries over.** A three.js directional or hemisphere light intensity carries over unchanged. No
other three.js light type has an equivalent yet: spot, point, rect-area or environment map. A scene
lit by those does not carry over until phase 2 adds them.

**How the match is checked.** `tools/three-reference.mjs` transcribes the formulas from three's
shader chunks. It first checks that all 20 transcribed lines are in three 0.180.0's source verbatim
(`web3d/evidence/phase1/gates.txt`). Its output, `tools/three-reference.txt`, is checked three ways:

- the Rust test `shading_matches_three_js_r180`;
- the probes in `demo/probe.js` (group R);
- a real three.js `WebGLRenderer`, within 2/255 on 7 cases once web3d's environment stand-in (below)
  is added to three's pixel; without it, four cases differ by more: the two metal cases by up to 8
  levels, the oblique glossy one by 4 and the grazing one by 3
  (`web3d/evidence/phase1/critic2-rerun/report-probe.json`).

**One term is not three's.** The specular colour also reflects the hemisphere in the mirror direction.
It stands in for an environment map, so a metal is not black.

**Precision.** Fragment arithmetic is `mediump`, except the normal, view and half vectors and the
highlight, which are `highp`. At fp16, N.H moves in steps of 1/2048 near 1, so `pow(N.H, 2048)`
would band into rings.

**Output.**

- Textures are `SRGB8_ALPHA8`, and output is encoded with the exact sRGB curve.
- There is no tone mapping yet; output is clamped.
- `discard` lives only in `textured-mask`, so it never turns off early depth testing elsewhere.
- Back faces are culled except on double-sided materials, which turn their normals with
  `gl_FrontFacing`.

## GPU and CPU time

**GPU time** comes from a `TIME_ELAPSED_EXT` query around the frame's GL work:

- the query is flushed when it ends;
- the newest available result is used, and none if it is older than 30 frames;
- the disjoint flag is read only once a result is in, because reading clears it.

**CPU time**, when `cpuTiming` is on, is the module's `frame` work plus its WebGL calls.

## Phases 2 to 4: the design

**Light model.** Phase 2 adds what a three.js character scene usually uses:

- analytic spot lights with three's punctual maths: a `smoothstep` cone, and `1/max(d^decay, 0.01)`
  falloff;
- more directional lights;
- an L2 spherical-harmonic probe for an image light, for diffuse and a blurred reflection.

**Environment intensity.** three r180 overrides a material's `envMapIntensity` with
`scene.environmentIntensity`. It does so for a `MeshStandardMaterial` whose own `envMap` is null in a
scene with an environment (`three/src/renderers/WebGLRenderer.js`, lines 2501-2504). web3d keeps the
per-material value, and says so wherever it is compared with three.

**Programs.**

- A `#define` exists only for work per pixel or for vertex inputs: normal map, alpha test, skinning,
  unlit, lightmap.
- Everything else is a uniform or a default: vertex colour, texture transform, double-sided, and a
  1x1 white texture for untextured materials.

**Streams for a skinned mesh:**

- position, alone, rewritten in place when morphed;
- joints and weights;
- the rest.

**Morphs and skinning:**

- **Morphs run on the CPU** over sparse deltas, and only changed spans are uploaded. All-zero targets
  are dropped at load.
- **Bones** go up as one uniform block of 3x4 matrices a frame.
- **A mesh whose every vertex follows one joint** is drawn as a rigid object on that joint, with no
  skinning.
- **A skinned object** takes its bounds from its joints, or is not culled.

**Alpha-tested cards** (hair) are the expected top cost:

- alpha-to-coverage needs MSAA;
- `discard` defeats early depth.

**The per-pixel budget.** Each feature is measured on dolores, per 1,000 pixels, with MSAA on and
off, and multiplied by the pixels it covers. A feature goes in when what it adds to the picture is
worth its measured cost.

## Results

**Mac mini M4 Pro.** Headless Chrome 156, ANGLE over Metal, 1366x768 window, camera still, CPU timing
on, 10 s at 60 fps after a 3 s warm-up (`web3d/evidence/phase1/rendered-check.json`):

| Canvas | GPU ms mean | p50 | p95 | CPU ms p50 (p95) | Draws | Triangles |
| --- | --- | --- | --- | --- | --- | --- |
| 1024x576, MSAA 4x | 3.076 | 3.108 | 5.347 | 0.055 (0.145) | 64 | 320,000 |
| 1366x768, MSAA 4x | 1.28 | 1.019 | 5.051 | 0.05 (0.185) | 64 | 320,000 |
| 1024x576, no MSAA | 0.463 | 0.303 | 0.667 | 0.065 (0.175) | 64 | 320,000 |

- **These GPU times are not a capacity measure.** Another session's headless Chrome was drawing on
  the same GPU throughout, so the times are inflated and the runs are not comparable with each other
  (the smaller canvas reads slower than the larger one). A quiet re-measure of this load is still to do.
- Every frame took 16.67 ms, the display's 60 Hz.
- The clear colour read exactly (33, 36, 43), and the centre was lit, in every run.
- All **59** probes passed, with `getError` 0 and no `INVALID_OPERATION` (same file, `probes`).
- The critics' scripts pass on this code:

  | Critic | Result | Evidence |
  | --- | --- | --- |
  | First | 29 of 29 probes | `critic-rerun/` |
  | Second | 14 of 14 probes | `critic2-rerun/` |
  | Third | 10 of 10 probes | `critic3-rerun/` |
  | Fourth | X1 to X7 pass; X8 refuses 10 of 11 values, and the eleventh is the handle itself (`o + 2^-30` rounds to `o`) | `critic4-rerun/` (its README) |
  | Fifth | C1, C2, H5, R1, R2, T1, T2, L5, Y5 and B1 pass. X8, K1 and E1 assume the earlier 30-bit handles: X8's fraction rounds to the handle, there is no longer a 32-renderer limit to refuse, and the object table now issues 1,073,709,057 handles, then refuses cleanly | `critic5-rerun/` (its README) |

**dolores.** A Raspberry Pi 5 with Chromium 156 on V3D 7.1, with **the demo's earlier load**, before
it was given its present one (`web3d/evidence/phase1-dolores/README.md`):

| Canvas | MSAA | GPU ms (p95) |
| --- | --- | --- |
| 1024x576 | 4x | 9.47 (12.95) |
| 1024x576 | off | 5.28 |

- MSAA 4x added 4.19 ms.
- Over five sizes with MSAA off, the cost fits 2.32 ms fixed plus 0.0046 ms per 1,000 canvas pixels.
- That run predates two shader changes, three's Schlick Fresnel and the `highp` highlight, and the
  present load. Re-measure it.

## Build, run, gates

```
web3d/build.sh   # cargo (wasm32, release) -> wasm-opt -Os -> web3d/pkg/; prints sizes
PLAYWRIGHT_CORE=/path/to/node_modules/playwright-core/index.mjs \
  node web3d/demo/check.mjs --port 5397 --out web3d/evidence/phase1 --seconds 10 --steady 60
THREE_DIR=/path/to/node_modules/three node web3d/tools/three-reference.mjs --check
```

`build.sh` builds offline. It sets size-oriented release settings for this build only: `-Os`, LTO,
one codegen unit, and panic abort.

**The build is reproducible.** With the rust-src component installed, rustc writes the local
sysroot's path, under the builder's home, into panic locations of standard-library code compiled
into the crate. `build.sh` remaps it to the `/rustc/<commit>` prefix the standard library's own paths
already use, and remaps the workspace and `CARGO_HOME`, through `cargo rustc` so no configured
rustflags are replaced. It fails if a local path remains in the module, and it prints the rustc and
wasm-opt versions, on which the bytes depend (rustc 1.99.0 and wasm-opt 133 here). Two builds and one from an
empty target directory gave byte-identical `pkg/` files
(`web3d/evidence/phase1/critic4-rerun/reproducible.txt`).

**The demo** (`web3d/demo/`) is a round load of its own, 64 draws and 320,000 triangles:

- 58 torus knots of 5,376 triangles;
- a sphere with a generated canvas texture;
- a masked grille and a blended glass pane, both double-sided;
- two glossy spheres;
- a ground grid.

It uses two textures, the canvas label and the grille's mask. Its query parameters are `scale`,
`aa=0`, `still=1`, `cputime=0`, `hud=0`, `arrays=1` (matrices and vectors as plain Arrays, as three.js
holds them; the allocation run uses it), `knots`, `tubular` and `radial`.

**The probes** (`demo/probe.html`, 59 of them) cover:

- pixel-exact rendering, primitive restart, and lighting against three.js r180;
- refusals, forged handles, and use after dispose;
- a second renderer on one canvas;
- context loss and recovery;
- the boundary: lying typed arrays and the Rust staging check (Q), handles of another kind or
  renderer (W), handles across 102 renderers (H6), Array elements and lengths (C), texture sizes
  (T), the compile cache (L), failed creates and context settings (Y, Y2), and re-entrancy (U).

**The rendered check** serves `web3d/` cross-origin isolated, for 5 µs timers. It uses a port it
refuses to share and its own headless Chrome, and aborts on a software renderer. It **fails** unless
all of these hold:

- every demo run draws 64 objects and 320,000 triangles;
- the clear colour and a lit centre read right;
- no run reports a GL error or a page error;
- every probe passes, with no `INVALID_OPERATION`;
- the allocation gate above holds, and its positive control is seen.

**CI** runs `cargo fmt --check`, clippy with `-D warnings`, and the tests, on every platform it
builds. One more step, on `ubuntu-latest`, adds the wasm32 target and runs
`cargo clippy --locked -p loadngo-web3d --target wasm32-unknown-unknown --all-targets -- -D warnings`
(`.github/workflows/ci.yml`). It exists because `src/web.rs` holds most of the crate's unsafe code
(the rest is a byte view in `src/pack.rs` and the tests' counting allocator in `src/testing.rs`) and
compiles only for wasm32.

That step cannot run here, since GitHub Actions runs only on a push. It was checked two ways instead:

- the YAML parses;
- the same command passes locally (`web3d/evidence/phase1/gates.txt`).

**By hand, besides CI:**

- `build.sh`;
- `three-reference.mjs --check`;
- the rendered check;
- the critics' scripts:
  - the first's: `evidence/phase1/critic/`;
  - the second's: `critic2/`, run with `PLAYWRIGHT_CORE` and `THREE_BUILD` set;
  - the third's: `critic3/run3.mjs`;
  - the fourth's: `critic4/run4.mjs`.

**Sizes** (`web3d/evidence/phase1/gates.txt`):

| File | Bytes | gzip -9 |
| --- | --- | --- |
| `pkg/loadngo_web3d.wasm` | 69,172 | 31,654 |
| `pkg/gl.js` | 13,838 | 4,686 |
| `pkg/web3d.js` | 36,363 | 11,286 |

## Limits of phase 1

- One directional light and a hemisphere ambient. There are no spot or area lights, image light,
  shadows, normal maps or tone mapping.
- One UV set, and base colour is the only texture.
- No instancing: every object is a draw.
- A lost context finishes the renderer; it is never rebuilt in place.
- One renderer per canvas, and 2²⁰−1 renderers in a page's life (the serials). Renderers on different
  canvases coexist, each with its own module instance.

## Licence

loadngo is BSD-3-Clause (`LICENSE`), and so is this crate. An application that ships `web3d/pkg/`
must carry:

- loadngo's copyright line;
- the three BSD-3 conditions;
- the disclaimer;
- the notices of Rust's standard library (MIT or Apache-2.0), which the wasm module contains.

`tools/three-reference.mjs` reads three.js at build time and ships none of it.
