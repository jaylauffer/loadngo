# Clip / Scissor Rects

## Status and purpose

Status: **renderer-side clipping implemented 2026-09-07**; hardware
scissor still future work (see the revised decision below). Written
2026-09-07 after gamepad/focus work made scrolling behavior worth looking
at closely.

`loadngo` has no clipping primitive at any level of its drawing pipeline.
This document records what that costs, and the design for adding one.

## The gap, as it actually is today

Verified by reading the three op vocabularies — none has a clip concept:

- `ui_core::PaintOp` (`ui-core/src/paint.rs:71`) — what widgets emit.
- `loadngo_renderer::FrameCommand` (`renderer/src/lib.rs:286`) — what the
  renderer converts them into.
- `loadngo_host_core::RenderOp` (`host-core/src/lib.rs:629`) — the simpler
  op list games like `sng-roguelite` write directly.

The only clipping anywhere is `clip_rect` on *text* render requests inside
some host backends (`host-desktop/src/linux.rs:2297`,
`host-desktop/src/ios.rs:995`), which exists so glyphs don't spill past
their own text box. It is a text-rasterization detail, not a general
capability, and nothing else can use it.

### What it costs

Anything scrollable has to cull whole items instead of clipping partial
ones. `sng-roguelite`'s achievements list is the worked example and says so
outright (`crates/game-app/src/lib.rs`, `push_achievement_rows`):

```rust
if y < viewport.y || y + ACHIEVEMENT_ROW_HEIGHT > viewport.y + viewport.height {
    continue;
}
```

with a doc comment explaining that a partially-visible row is skipped
entirely because "nothing else will cut it off", and that the previous
version "learned that the hard way (rows spilling past the panel border
into the close/back area)".

So the list moves in whole-row steps: a row pops into existence once fully
revealed rather than sliding in. That is the "scrolling isn't smooth"
symptom, and it is not a scroll-math bug — the scroll offset is already
continuous and pixel-accurate. It is purely the missing clip.

The same limit blocks anything else wanting to scroll or nest content:
`sng-rusty`'s editor genuinely nests (workspace split → panel → scroll
region), which is where clipping stops being cosmetic and starts being
structural.

## Decision: a push/pop clip stack, not a per-op field

Add two ops rather than a `clip: Option<Rect>` field on every drawing op:

```rust
RenderOp::PushClip { rect: Rect },
RenderOp::PopClip,
```

mirrored as `PaintOp::PushClip`/`PopClip` and
`FrameCommand::PushClip`/`PopClip`.

Why a stack rather than a field on each op:

- **Nesting is real, and a flat field can't express it.** A scroll region
  inside a panel inside a split pane needs the *intersection* of three
  rects. With a stack the renderer intersects on push and nobody has to
  precompute it. With a per-op field every emitter would have to know its
  ancestors' rects.
- It matches what the hardware does. Metal/GLES/DX12 all model scissor as
  *state*, not as per-draw data, so a stack maps 1:1 onto them.
- It keeps the ops small. These vocabularies are already `Vec`s replayed in
  order; adding eight bytes to every variant to serve a minority of draws
  is the wrong trade.

The cost is that pushes and pops must balance. The renderer should treat an
unbalanced stack as a bug it reports rather than silently tolerating: pop
with an empty stack is an error, and a frame that ends with a non-empty
stack is an error. Both are cheap to check where the stack lives.

Clip rects **intersect** on push; they never widen. A child cannot draw
outside its parent's clip by pushing a larger rect. An empty intersection
is legal and means "draw nothing" — it must not be treated as "no clip".

## Decision: all three vocabularies get it

Widgets need to clip their own content (that is the whole point of
`ScrollContainerModel`), so `PaintOp` needs it, not just the lower levels.
`RenderOp` needs it because games like `sng-roguelite` write `RenderOp`
directly and never touch the widget pipeline.

## Coordinate space

Ops carry logical coordinates; scissor rects are integer device pixels.
Backends convert using the same scale they already apply to geometry, then:

- snap the rect **outward** (floor the origin, ceil the far edge), and
- clamp to the render target's bounds.

Outward rather than inward so a clip never shaves a pixel off content the
caller intended to be visible; the cost is that at fractional scales a clip
may leak up to one device pixel, which is the better failure for a scroll
viewport whose edge is usually against a border anyway.

## Decision: clip in the renderer, not per-backend (revised during design)

The first draft of this doc assumed each GPU backend would set a hardware
scissor. Reading `gfx-metal` changed that: it **already clips textured
quads geometrically**, by trimming the quad and remapping its UVs
(`textured_rect_vertices`, `gfx-metal/src/lib.rs:3033`), driven by a
`clip_rect` that `TextRequest` and `ImageRequest` have carried all along
(`renderer/src/lib.rs:696`). No scissor state is involved anywhere.

That plumbing is already end-to-end — `encode_render_ops` just always
passes `clip_rect: None`, because `RenderOp` had no way to express a clip.

So the first implementation belongs in the **renderer**, applying the clip
stack while encoding, not in each backend:

- `FillRect` — intersect the rect with the active clip. A clipped
  rectangle is just a smaller rectangle; exact, and no backend knows.
- `Text`, `BlitImage` — pass the active clip as `clip_rect`, which every
  backend already honors.
- `StrokeRect`, `Line`, `Circle` — **cannot** be clipped by intersecting
  geometry (clipping a stroked rect in half would draw a border along the
  cut; a circle can't become a rect). These **cull** when not fully inside
  the clip, which is exactly today's behavior, so nothing regresses.

This is strictly better than starting per-backend:

- It works on **every** backend at once, including `gfx-dx12`, which
  otherwise could not be verified without Windows hardware.
- It needs no GPU code, no shader change, and no new backend capability
  flag.
- It is exact for the primitives scrolling lists are actually made of
  (filled rects and text).

Hardware scissor stays the eventual answer for `StrokeRect`/`Line`/`Circle`
and for cheap deep nesting, and the op vocabulary above is unchanged by
adding it later — a backend that gains real scissor just stops needing the
cull path. That is a follow-up, gated on a use case that needs a partially
clipped circle or stroke, which no screen has today.

## What this unblocks

- Smooth, pixel-accurate scrolling in `sng-roguelite`'s achievements list,
  and the deletion of its whole-row cull.
- Nested scroll/panel content in `sng-rusty`'s editor.
- `ui-core`'s `ScrollContainerModel` becoming genuinely usable by games
  rather than a geometry helper whose clipping they must fake.

## Non-goals

- Rounded-rect, path, or arbitrary-shape clipping. Rectangles only.
- Layer/opacity groups or offscreen render targets.
- Transform stacks. Clip rects are in the same space as everything else;
  this doc adds no rotation/scale concept.

## Related docs

- [WIDGET_FRAMEWORK.md](WIDGET_FRAMEWORK.md) — the widget/paint ownership
  split this extends, and where `ScrollContainerModel` lives.
- [DRAW_PRIMITIVE_MATRIX.md](DRAW_PRIMITIVE_MATRIX.md) — per-backend
  primitive coverage; clip belongs in that matrix once implemented.
- [TEXT_RENDERING_TROUBLESHOOTING.md](TEXT_RENDERING_TROUBLESHOOTING.md) —
  the existing text-only `clip_rect` behavior this would subsume.
