# loadngo Draw Primitive Matrix

Date: 2026-03-26. Revised 2026-11-20 against the code at `dev` `fb2f59bb`:
the matrix, the clipping section and the Metal section were out of date.
What is still to build is listed in
[RENDERER_ROADMAP.md](RENDERER_ROADMAP.md), "Remaining work".

## Purpose

`loadngo` needs a clear drawing contract that is larger than:

- rectangles
- text
- image blits

This note defines:

- the fundamental draw elements `loadngo` should own
- the current backend execution state
- the implementation priority
- where future backends like Vulkan fit

This is a coordination note, not a claim that every backend already implements every primitive natively.

## Guiding rule

The renderer language should be richer than any single backend.

That means:

- `ui-core` and `renderer` should define the canonical primitive set
- backends should implement those primitives natively where it matters
- temporary approximation is acceptable
- repeated prerasterize-and-blit should be treated as transitional, not the final model

## Fundamental draw set

These are the primitives `loadngo` should treat as first-class:

- `FillRect`
- `StrokeRect`
- `Line`
- `FillCircle`
- `StrokeCircle`
- `Polyline`
- `Arc`
- `QuadraticBezier`
- `CubicBezier`
- `ParticleBatch`
- `Text`
- `Image`

## Semantic expectations

### Geometry

- `Line`
  - finite-width stroke between two points
- `FillCircle`
  - filled disc
- `StrokeCircle`
  - circular stroke with explicit thickness
- `Polyline`
  - connected stroke path with optional closing segment
- `Arc`
  - circular stroke over a start/sweep interval
- `QuadraticBezier`
  - stroked quadratic curve
- `CubicBezier`
  - stroked cubic curve

### Behavior

- clipping should be backend-consistent
- alpha blending should be backend-consistent
- stroke thickness should be visually similar across platforms
- dynamic effects should prefer live geometry or backend-native execution over prerasterized textures where feasible

## Current renderer contract

Today, [renderer/src/lib.rs](../renderer/src/lib.rs) already carries these `FrameCommand`s directly:

- `Clear`
- `FillRect`
- `StrokeRect`
- `Line`
- `Circle`
- `Polyline`
- `ParticleBatch`
- `Text`
- `Image`
- `PushClip` / `PopClip`

`Circle` is the filled circle. `Arc` is a native `FrameCommand`.

Current limitation:

- `QuadraticBezier`
- `CubicBezier`
- `StrokeCircle`

are still lowered into `Polyline` in the renderer, rather than preserved as native frame commands.
No backend ever sees a curve: each receives the `Polyline` and draws that.

That is acceptable for now, but it should be treated as an intermediate state rather than the permanent end model.

### The two draw lists do not carry the same set

`PaintOp` (`ui-core`) is the full set above except clipping: it has no
`PushClip`/`PopClip`, and its `BlitImage` has no alpha.

`RenderOp` (`host-core`, the list a game can write directly) has `Clear`,
`FillRect`, `StrokeRect`, `Line`, `Circle`, `Text`, `BlitImage` (with
alpha), `PushClip` and `PopClip`. It has no polyline, arc, curve or
particle batch.

Neither has a filled polygon, and an image draw is a rectangle and an
alpha everywhere: no rotation, flip, source rectangle or tint.

## Clipping

Until 2026-09-07 no draw list could clip, and this section described that
gap and the workaround it forced on `sng-roguelite`'s achievements list
(a row was drawn only when the whole row fitted the viewport).

Since then ([CLIP_AND_SCISSOR.md](CLIP_AND_SCISSOR.md)):

- `RenderOp::PushClip { rect }` and `RenderOp::PopClip` exist; pushes nest
  and intersect.
- The renderer resolves the clip stack while encoding. A filled rectangle
  is intersected with the clip. Text and images carry the clip as
  `clip_rect` and every backend trims them: Metal and GLES trim the quad
  and its texture coordinates, DX12 sets a scissor rectangle for that
  draw. A stroked rectangle, a line or a circle that is not wholly inside
  the clip is dropped.
- `FrameCommand::PushClip`/`PopClip` are in the enum, but the encoder
  consumes the clip operations and never emits them, and every backend
  treats one as a no-op.
- The achievements list now lives in `game-menu/src/achievements.rs` and
  clips its rows with `RenderOp::PushClip`.

Still open: `PaintOp` has no clip operations, and no backend uses a
scissor for geometry, so a stroke cannot be partly clipped.

## Precision rule

The draw layer should not switch wholesale to `f64`.

Recommended split:

- use `f32` for draw geometry and vertex-space primitives
- use `f64` for:
  - clocks
  - animation phase
  - long-lived accumulators
  - large absolute-value calculations
- convert to `f32` at the final draw boundary

Reason:

- GPU and shader-facing geometry is naturally `f32` oriented
- current `loadngo` UI/runtime geometry does not need `f64` spatial precision
- the real precision failures we have seen were timing/phase failures from large absolute values, not 2D vertex precision failures

## Primitive migration checklist

When changing a primitive contract, treat it as a full migration, not a local fix.

Required steps:

1. Define the contract first.
   - Example: `Circle.radius` is `f32` everywhere above the final raster boundary.
   - Integer rounding is allowed only at the final pixel-fill or software-raster step.

2. Find every affected site before editing.
   - Search the full repo for:
     - enum definitions
     - constructors
     - scaling logic
     - backend helpers
     - software fallback rasterizers
     - tests

3. Patch the whole slice in one pass.
   - Do not stop after fixing the first compiler error.
   - Update `ui-core`, `renderer`, `host-core`, Metal, GLES, DX12, and host fallback paths together when they participate in the same primitive contract.

4. Use backend parity as the completion gate.
   - A primitive rollout is not complete unless the target backend set is aligned.
   - For current `loadngo` work, that means:
     - Metal
     - GLES
     - DX12

5. Validate the full affected package set.
   - At minimum, run:
     - `cargo check -p loadngo-renderer`
     - `cargo check -p loadngo-gfx-gles`
     - `cargo check -p loadngo-gfx-metal`
     - `cargo check -p loadngo-gfx-dx12`
     - `cargo check -p loadngo-host-desktop`

6. Describe status explicitly.
   - Use per-backend status in summaries:
     - `Metal: done`
     - `GLES: done`
     - `DX12: pending`
   - Do not describe a rollout as complete while one target backend still lags.

## Backend matrix

Status meanings:

- `native`
  - backend draws the primitive directly as backend geometry or backend-owned draw data
- `approx`
  - backend receives the primitive meaningfully, but through approximation such as curve-to-polyline lowering
- `rasterized`
  - backend or host converts the primitive into image textures/bitmaps before presentation
- `unsupported`
  - backend does not meaningfully handle the primitive

### Current practical matrix

| Primitive | Renderer | Metal | GLES (Linux/Android) | Software host | DX12 | Vulkan |
| --- | --- | --- | --- | --- | --- | --- |
| `FillRect` | native | native | native | native | native | future |
| `StrokeRect` | native | native | native | native | native | future |
| `Line` | native | native | native | native | native | future |
| `FillCircle` | native as `Circle` | native | native | native | native | future |
| `StrokeCircle` | approx via `Polyline` | approx via `Polyline` | approx via `Polyline` | approx via `Polyline` | approx via `Polyline` | future |
| `Polyline` | native | native | native | native | native | future |
| `Arc` | native | native | native | approx via `software::arc_points` | native | future |
| `QuadraticBezier` | approx via `Polyline` | approx via `Polyline` | approx via `Polyline` | approx via `Polyline` | approx via `Polyline` | future |
| `CubicBezier` | approx via `Polyline` | approx via `Polyline` | approx via `Polyline` | approx via `Polyline` | approx via `Polyline` | future |
| `ParticleBatch` | native | native (circle geometry, one draw per particle) | native (shared solid batch) | native circles | native (one draw item) | future |
| `Text` | native request | rasterized by CoreText inside the backend, drawn as a texture | rasterized: the host rewrites it into an `Image` (`fontdue` and `loadngo-text-shaping`) | drawn by the host's text closure | rasterized: the host rewrites it into an `Image`, as for GLES | future |
| `Image` | native | native | native | native | native | future |
| `PushClip` / `PopClip` | resolved while encoding, never emitted | no-op | no-op | no-op | no-op | future |

Rows corrected 2026-11-20. The curve rows used to say `rasterized` for
Metal and `unsupported` for DX12, and the `Text` row `unsupported` for
DX12. Neither backend ever receives a curve (the renderer hands every
backend a `Polyline`), and on Windows `prepare_dx12_frame` rewrites each
`Text` into an `Image`, exactly as `prepare_gles_frame` does
([TEXT_TEXTURE_LIFECYCLE.md](TEXT_TEXTURE_LIFECYCLE.md)). `supports_commands`
in `gfx-gles` and `gfx-dx12` refuses only a `Text` that reaches the backend
unrewritten, and that frame goes to the software renderer.

"Software host" is one rasterizer, `loadngo_renderer::software::RgbaCanvas`,
shared by the Linux, Windows and Android software present paths since
2026-09-25. Before that each host carried its own copy, and the copies had
drifted apart (even-width lines one pixel too wide, image `clip_rect`
ignored, an out-of-bounds blit on Linux/Windows). Change software drawing
there, not in a host. Text is still drawn by each host, through the
`draw_command` text closure, because each host owns its fonts.

Software is the fallback, never the default: Linux and Android draw with GLES
and hand a frame to the software renderer only when GLES cannot draw it (no
EGL binding, an unsupported command, a render error), or when
`LOADNGO_DESKTOP_BACKEND=software` asks for it (on Android,
`adb shell setprop debug.loadngo_desktop_backend software`). Linux decides per
frame. Android, since 2026-09-25, stays on software for the rest of that window
once GLES fails, because EGL cannot reconnect to a window the software path has
locked; a new window tries GLES again. Before that, Android had no fallback and
drew nothing when GLES failed.

### Metal: large geometry used to abort the process (fixed 2026-09-11)

Metal's `setVertexBytes` is documented for data under 4 KB, and the AGX
driver enforces it by **aborting the process**, with no Rust panic and
no log line (exit 134). `gfx-metal` uploaded every `SolidGeometry` triangle
list through it in one call, so any `Polyline`/`StrokeCircle`/lowered
curve past 512 vertices (~85 polyline segments, since each segment is a
6-vertex quad) killed the app on macOS **and iOS** (same module). Found by
`sng-bass-blaster`'s ~1,600-point waveform strip; the backtrace ends in
`-[AGXG16XFamilyRenderContext setVertexBytes:length:atIndex:]`.
`present_scene_ordered` now draws geometry in whole-triangle chunks that
each fit (`vertex_byte_chunks`), which needs no per-frame buffer
allocation. The other backends take geometry through buffers and were not
affected.

## The native geometry pass (March 2026)

Kept as history. Particle batches joined this list on 2026-09-25 (next
section).

The backend-native geometry steps are:

- GLES:
  - `Line`
  - `Circle`
  - `Polyline`
  - `Arc`
- Metal:
  - `Line`
  - `Circle`
  - `Polyline`
  - `Arc`
- DX12:
  - `Line`
  - `Circle`
  - `Polyline`
  - `Arc`

Linux and Android host prep no longer rasterize:

- `Line`
- `Circle`
- `Polyline`
- `Arc`

before those commands reach GLES.

This is a real move away from the old flatten-and-blit pattern for dynamic geometry.

## Metal and particle batches (done 2026-09-25)

This section used to argue that Metal must stop drawing `ParticleBatch` as
generated images. It has: since `371d1006`, `gfx-metal` turns each particle
into circle geometry, as GLES and DX12 do.

One difference remains. Metal issues one draw call per particle, and per
rectangle and polyline (`present_scene_ordered`). GLES puts a whole batch
into its shared solid-geometry buffer and DX12 draws it as one item, so an
effect is one draw there. See [RENDERER_ROADMAP.md](RENDERER_ROADMAP.md),
"Remaining work", item 10.

## Vulkan

Vulkan is a valid future backend target.

But it should not be the next immediate implementation step.

Reasons:

- `loadngo` still needs its primitive contract stabilized first
- Metal, GLES, and DX12 already exist and need the same primitive story
- introducing Vulkan before the primitive matrix is settled would multiply backend work before the renderer contract is mature

So the recommendation is:

- do not start Vulkan first
- finish the primitive contract and native geometry rollout across existing backends
- then add Vulkan against a clearer renderer contract

In short:

- Vulkan is a good future backend
- Vulkan is not the next sequencing step

## Recommended implementation order

### Phase 1

- `Polyline`
- `Line`
- `FillCircle`

Reason:

- these immediately improve traces, outlines, simple motion language, and button/scene effects

Status (2026-11-20): done on Metal, GLES and DX12.

### Phase 2

- `StrokeCircle`
- `Arc`

Reason:

- rings, meters, orbit lines, and circular accents become cleaner

Status (2026-11-20): `Arc` is native on all three. `StrokeCircle` is still
lowered to a `Polyline`.

### Phase 3

- `QuadraticBezier`
- `CubicBezier`

Reason:

- curve quality matters after the simpler stroke/fill language is stable

Status (2026-11-20): not started. Both curves are still lowered to a
`Polyline`.

### Phase 4

- `ParticleBatch` semantics

Reason:

- effects quality should come after the geometry substrate is trustworthy
- this is where blend modes, additive light, particle lifetime, and motion semantics belong

Status (2026-11-20): the batch is GPU geometry on all three backends, and
`ui_core::ParticleEmitter` owns lifetime and motion
([PARTICLE_EFFECTS.md](PARTICLE_EFFECTS.md)). Blend modes are not built.

## Backend rollout order

Recommended backend order:

1. GLES
2. Metal
3. DX12
4. Vulkan

Reason:

- GLES matters for Linux and Android immediately
- Metal is the main MacMini development path
- DX12 is the practical Windows-native backend already present in the tree
- Vulkan should come after the primitive model is proven, not before

## Immediate next steps

The March list (decide whether `StrokeCircle` becomes a native command,
promote the two curves, revisit `ParticleBatch` semantics) still stands,
and no screen has needed any of it since. The current list of what is
missing, with a proposed order, is in
[RENDERER_ROADMAP.md](RENDERER_ROADMAP.md), "Remaining work" and "Proposed
order".

## Related notes

- [ARCHITECTURE.md](ARCHITECTURE.md)
- [RENDERER_ROADMAP.md](RENDERER_ROADMAP.md)
- [TEXT_RENDERING_TROUBLESHOOTING.md](TEXT_RENDERING_TROUBLESHOOTING.md)
