# loadngo Renderer Roadmap

First written 2026-03-15. Rewritten 2026-11-20 against the code at `dev`
`fb2f59bb`; "How this was checked" at the end says what was read and run.
The March version described a renderer with no GPU backend. That stopped
being true long before this revision; its four milestones are all met.

## Goal

`loadngo` owns rendering as a first-class subsystem.
Platform backends provide windowing, GPU device/surface integration, and presentation, while the
renderer itself owns:

- frame encoding
- text and image draw planning
- multilingual text contracts
- batching and cache policy
- GPU abstraction boundaries

Related notes:

- [DRAW_PRIMITIVE_MATRIX.md](DRAW_PRIMITIVE_MATRIX.md): which backend draws which primitive, and how
- [CLIP_AND_SCISSOR.md](CLIP_AND_SCISSOR.md): clipping
- [TEXT_TEXTURE_LIFECYCLE.md](TEXT_TEXTURE_LIFECYCLE.md): how text becomes textures
- [PARTICLE_EFFECTS.md](PARTICLE_EFFECTS.md): the particle batch and emitter

## Layering, as built

1. `ui-core`
- widget behavior and `PaintOp`, the full primitive set

2. `host-core`
- platform-neutral input/window/frame contracts
- `RenderOp`, the smaller draw list a game can write directly

3. `renderer` (`loadngo-renderer`)
- encodes `PaintOp` and `RenderOp` into `FrameCommand`s
- resolves the clip stack while encoding
- owns the text-texture layout rules and the `GraphicsBackend` trait
- owns the one software rasterizer, `software::RgbaCanvas`

4. GPU backends
- `loadngo-gfx-metal`: macOS and iOS
- `loadngo-gfx-gles`: Linux (EGL on Wayland and X11) and Android
- `loadngo-gfx-dx12`: Windows

5. Platform hosts, all in `host-desktop`

| Platform | Draws with | When the GPU path cannot draw |
| --- | --- | --- |
| macOS | Metal | no fallback |
| iOS | Metal | no fallback |
| Linux | GLES | software, decided frame by frame |
| Android | GLES | software for the rest of that window |
| Windows | DX12 | software for one frame if a command is refused; for the rest of that window after a render error |

`LOADNGO_DESKTOP_BACKEND=software` asks for the software renderer on Linux
and Windows. The NetBSD wsdisplay host and `fallback.rs` were not reviewed
for this revision.

Every host calls `Renderer::encode_render_ops` or `encode_paint_ops` and
then `Renderer::render(backend, commands)`. No host draws on its own.

## Milestones

All four are met.

| Milestone | Status |
| --- | --- |
| 1. `loadngo-renderer` owns command encoding; backends consume renderer commands | Done. The five hosts above go through `Renderer`. |
| 2. Metal renders rectangles, lines, images and text through renderer-owned commands | Done, and also circles, polylines, arcs and particle batches. |
| 3. The same renderer core is reused by iOS, Android, Linux and Windows | Done. |
| 4. Macroquad removed from `loadngo` and `sng-rusty` | Done. No manifest or lock file in either names it; `host-core/tests/macroquad_removal.rs` keeps it out. |

## Multilingual text: what is built

The March requirements, one by one:

| Requirement | Status |
| --- | --- |
| Shaping for scripts that need it | Apple hosts: CoreText lays out and rasterizes every string, so shaping is the system's. Linux, Windows and Android: `loadngo-text-shaping` shapes Thai and Sinhala with `rustybuzz` and rasterizes with `fontdue`. No other script is shaped there. |
| Font fallback chains | Linux, Windows and Android: bundled Noto faces for Thai, Sinhala and Simplified Chinese (GB2312 subset). Apple hosts: whatever CoreText substitutes. |
| Measurement that matches shaped output | See [TEXT_OVERFLOW_AND_MEASUREMENT.md](TEXT_OVERFLOW_AND_MEASUREMENT.md). |
| Explicit direction (`Auto`, LTR, RTL), script and language per request | **Not built.** `TextRequest` has the three fields, but neither `RenderOp::Text` nor `PaintOp::Text` can set them, so they always hold the renderer's defaults, and no backend lays text out differently because of them (Metal only mixes them into its cache key). `loadngo-text-shaping` always shapes left to right. |
| Glyph atlas and quad generation | **Not built.** Every backend draws a string as one CPU-rasterized texture per distinct string and style. |

## Remaining work

Each item was confirmed in the code on 2026-11-20. Nothing here is a
regression; these are the parts never built.

### What a caller cannot ask for

1. **Sprites.** An image draw is a rectangle and an alpha
   (`ImageRequest`, `RenderOp::BlitImage`); `PaintOp::BlitImage` has no
   alpha either. There is no rotation, flip, source rectangle (so no
   sprite sheet or atlas) and no tint.
2. **Blend modes.** Everything is source-over. Additive blending is listed
   as not done in [PARTICLE_EFFECTS.md](PARTICLE_EFFECTS.md).
3. **Filled polygons.** The only filled shapes are rectangles and circles.
   `sng-zhoenus` fills its ship with stacked `FillRect` scanlines
   (`src/render.rs`, `fill_convex_polygon`).
4. **Two draw lists with different holes.** `RenderOp` has clipping and
   image alpha but no polyline, arc, curve or particle batch. `PaintOp` has
   those but no `PushClip`/`PopClip` (the clip design mirrored them into
   `PaintOp`; that half was not built) and no image alpha.
5. **Partial clipping of strokes.** A stroked rectangle, line or circle
   that crosses a clip edge is dropped whole. Hardware scissor is the fix
   and is not built ([CLIP_AND_SCISSOR.md](CLIP_AND_SCISSOR.md)).

### Quality

6. **Anti-aliasing.** No backend sets up multisampling (DX12 asks for one
   sample; Metal and GLES leave the default, which is none) and no shader
   smooths an edge, so lines, circles and polygon edges are stair-stepped.
7. **Text as whole-string textures.** See the table above. Text that
   changes every frame (a counter, a timer, typed text) makes a new texture
   each change.
8. **Right-to-left text.** See the table above.
9. **Curves and ring strokes** are approximated as polylines by the
   renderer (8 to 64 segments for a quadratic curve, 10 to 96 for a cubic,
   8 to 96 for a ring). No backend draws them natively.

### Cost

10. **Metal issues one draw call per item.** Each rectangle, each
    polyline and each particle is its own `setVertexBytes` and
    `drawPrimitives` in `present_scene_ordered`. GLES puts a whole particle
    batch into its shared solid-geometry buffer and DX12 draws it as one
    item, so an effect is one draw there. Not yet measured as a problem.
11. **Small heap traffic per frame.** Encoding clones each image key
    `String` and each polyline's points every frame. Not measured.

### Proof

12. **No backend can draw off screen or read its pixels back**, so no test
    compares what Metal, GLES, DX12 and the software rasterizer draw for the
    same commands. The backend tests cover vertex math, layout and resource
    bookkeeping. Parity between backends is checked by eye, and DX12 is the
    least looked at.
13. **Two host diagnostics are out of date.**
    `describe_unsupported_gles_command` (`linux.rs`) and
    `describe_unsupported_dx12_command` (`windows.rs`) name commands their
    backend now accepts, so a refused frame can be blamed on the wrong
    command.

### Not in the renderer at all

14. **3D.** The native renderer has no depth buffer, mesh or camera.
    `web3d` (a browser renderer) is on parked branches, not in `dev`; see
    `web3d/evidence/phase2/README.md` on `web3d-phase2`.
15. **Vulkan.** Not started; see the Vulkan section of
    [DRAW_PRIMITIVE_MATRIX.md](DRAW_PRIMITIVE_MATRIX.md).
16. **A native PDF renderer.** Plan only; see [PDF_RENDERING.md](PDF_RENDERING.md).

## Proposed order

A proposal by Claude Code on 2026-11-20. Jay has not decided it.

1. **A parity harness (item 12).** An off-screen target and a pixel
   readback on each backend, a small set of scenes, and a comparison
   against the software rasterizer with a tolerance. Everything below
   changes five implementations at once; today the only check that they
   agree is a person looking at five screens.
2. **One contract pass for items 1 to 4.** Sprites (source rectangle,
   rotation, flip, tint), additive blending, a filled polygon, and the same
   operations in both draw lists. They touch the same enums and the same
   five implementations, so one migration costs less than four
   ([DRAW_PRIMITIVE_MATRIX.md](DRAW_PRIMITIVE_MATRIX.md), "Primitive
   migration checklist"). `sng-ironwake` is the first consumer for sprites,
   `sng-zhoenus` for polygons and additive particles.
3. **Batch solid geometry on Metal (item 10)**, after measuring a
   particle-heavy frame on the Mac and an iPhone. It also removes the 4 KB
   chunking.
4. **Multisampling (item 6)**, measured on `dolores` and a phone before it
   becomes the default.
5. **A glyph atlas (item 7)** when a consumer shows the cost: the code
   editor and any game with per-frame text are the candidates.

Wait for a consumer before doing: hardware scissor (item 5),
right-to-left text (item 8), native curves (item 9), Vulkan (item 15).
Item 13 is a ten-minute fix that can go with any of the above. Item 14 is
a question of scope for Jay, not a task.

## How this was checked

2026-11-20, on the Mac mini, `dev` at `fb2f59bb`:

- Read: the `FrameCommand`, `RenderOp` and `PaintOp` enums; `encode_render_ops`,
  `encode_paint_ops` and `render` in `renderer/src/lib.rs`; the command
  loops and `supports_commands` in `gfx-metal`, `gfx-gles` and `gfx-dx12`;
  `renderer/src/software.rs`; backend selection and frame preparation in
  `host-desktop/src/{macos,ios,linux,windows,android}.rs`;
  `text-shaping/src/lib.rs`.
- Ran: `cargo test -p loadngo-renderer -p loadngo-gfx-metal -p loadngo-gfx-gles
  -p loadngo-gfx-dx12 -p loadngo-text-shaping`: 37, 33, 10, 5 and 17 tests
  pass. On macOS the GLES and DX12 tests cover only their portable code.
- CI run 44299930785 (`73200995`, 2026-11-18) passed on all four platforms;
  the two commits after it change only documents.
- Not done: no frame was looked at on any device for this revision.
