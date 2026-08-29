# loadngo Draw Primitive Matrix

Date: 2026-03-26

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

Current limitation:

- `QuadraticBezier`
- `CubicBezier`
- `StrokeCircle`

are still lowered into `Polyline` in the renderer, rather than preserved as native frame commands.

That is acceptable for now, but it should be treated as an intermediate state rather than the permanent end model.

`Arc` is now a native `FrameCommand`.

## Known gap: no clip/scissor primitive in `RenderOp`

`loadngo_host_core::RenderOp` (the lighter, direct-execution contract games
consume when they render `Vec<RenderOp>` themselves instead of going
through the `ui-core`/`renderer` `PaintOp`→`FrameCommand` pipeline — see
`host-desktop` in "Current gap summary" in
[RENDERER_ROADMAP.md](RENDERER_ROADMAP.md)) has no clip/scissor variant at
all: `Clear`/`FillRect`/`StrokeRect`/`Line`/`Circle`/`Text`/`BlitImage`,
none of which carry or consult a clip region. Only `RenderOp::Text.style`'s
`RenderTextOverflow` (`Clip`/`EllipsisEnd`/`EllipsisMiddle`) clips *that one
text op* to its own declared rect — it cannot clip other ops, or clip
content to some other rect (a scrollable panel's viewport, say).

This is a real, confirmed gap, not a theoretical one: `sng-roguelite`
needed a scrollable achievements list inside a fixed panel
(`crates/game-app/src/lib.rs` in that repo, `push_achievement_rows`) and
had no way to clip overflow content to the panel's viewport. The workaround
was to only ever emit a row `RenderOp::Text` when the *entire* row rect
fits inside the viewport, skipping any row that would be even partially
cut off — correct (nothing ever overflows), but it means a row pops into
view once fully revealed rather than sliding in continuously the way a
real clipped scroll view would. See that repo's
`docs/decisions/0005-eab-achievement-persistence-and-viewing.md` and
`docs/ACHIEVEMENTS.md`'s "Viewing achievements" section for the full story.

Note that `ui_core::PaintOp` already has a `clip_rect: Option<Rect>` field
on `PaintOp::Text` (`ui-core/src/paint.rs`), and `ui_core::ScrollRegionModel`
(`ui-core/src/scroll.rs`) already provides the scroll-offset/indicator math
generally — so the *model* half of "scrollable panel" already exists and is
reusable today. What's missing is purely the render-primitive half for
games on the `RenderOp` path: a way to clip arbitrary drawn content
(rects, other text, everything — not just one text op to its own bounds)
to a rect, without requiring a game to adopt the full `PaintOp`/`gui`
widget stack just to get a clipped scroll view.

**For a future agent to pursue:** add a clip/scissor primitive to
`RenderOp` — most likely a paired `PushClip { rect: Rect }` /`PopClip`
(or a scoped `ClipRect { rect: Rect, ops: Vec<RenderOp> }` wrapper) that
every `host-desktop` backend (macOS, iOS, Android, Linux, Windows,
fallback) honors for all op kinds, not just `Text`. Follow the "Primitive
migration checklist" above: define the contract first, find every backend
site, patch the whole slice in one pass, validate per-backend. This is
scoped to the `host-core`/`host-desktop` `RenderOp` contract specifically —
it does not require or block on the separate `PaintOp`/`renderer`
primitive rollout tracked elsewhere in this document.

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
| `StrokeCircle` | approx via `Polyline` | approx via `Polyline` | approx via `Polyline` | approx via polyline/host logic | approx via `Polyline` | future |
| `Polyline` | native | native | native | native | native | future |
| `Arc` | native | native | native | approx via host draw path | native | future |
| `QuadraticBezier` | approx via `Polyline` | rasterized | approx via `Polyline`, then native `Polyline` only where supported | approx via host draw path | unsupported | future |
| `CubicBezier` | approx via `Polyline` | rasterized | approx via `Polyline`, then native `Polyline` only where supported | approx via host draw path | unsupported | future |
| `ParticleBatch` | native | rasterized circles/images | rasterized circles/images | native circles | unsupported | future |
| `Text` | native request | rasterized text image | rasterized text image | native software text path | unsupported | future |
| `Image` | native | native | native | native | native | future |

## What changed in the current pass

The current backend-native geometry steps are now:

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

## Why Metal must be included

Metal should not remain a rasterized exception.

Right now [gfx-metal/src/lib.rs](../gfx-metal/src/lib.rs) still treats:

- `ParticleBatch`

primarily as generated images rather than backend-native geometry.

That contributes directly to:

- the coarse visual feel
- extra dynamic raster work
- backend divergence from Linux/Android

So yes:

- Metal must be updated too
- not later as a vague wish, but as part of the primitive matrix rollout

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

### Phase 2

- `StrokeCircle`
- `Arc`

Reason:

- rings, meters, orbit lines, and circular accents become cleaner

### Phase 3

- `QuadraticBezier`
- `CubicBezier`

Reason:

- curve quality matters after the simpler stroke/fill language is stable

### Phase 4

- `ParticleBatch` semantics

Reason:

- effects quality should come after the geometry substrate is trustworthy
- this is where blend modes, additive light, particle lifetime, and motion semantics belong

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

1. Decide whether `StrokeCircle` should remain renderer-lowered to `Polyline` or become a native `FrameCommand`.
2. Promote `QuadraticBezier` if its curve quality is now the next visible weakness.
3. Promote `CubicBezier` after that.
4. Revisit `ParticleBatch` semantics once the geometry substrate is stable.

## Related notes

- [ARCHITECTURE.md](ARCHITECTURE.md)
- [RENDERER_ROADMAP.md](RENDERER_ROADMAP.md)
- [TEXT_RENDERING_TROUBLESHOOTING.md](TEXT_RENDERING_TROUBLESHOOTING.md)
