# Particle effects

Status: 2026-09-25. First consumer: sng-zhoenus's ship thrusters.

## What exists

- **Draw primitive:** `PaintOp::ParticleBatch { particles }` in `ui-core`,
  carried through the renderer as `FrameCommand::ParticleBatch`. Each
  `Particle` is a filled circle: center, radius, RGBA color. Alpha blends
  normally; there is no additive blend mode yet.
- **Emitter:** `ui_core::ParticleEmitter` (`ui-core/src/particles.rs`), a
  CPU emitter for exhaust, sparks, smoke and similar effects. The game
  describes the look once in a `ParticleEmitterConfig` (lifetime, speed,
  cone, drag, radius over life, color gradient over life, capacity). Then,
  each frame, it calls `update(dt, Some(Emission { .. }))` while the effect
  is on, or `update(dt, None)` while it is off, and `append_particles` into
  the batch it paints.

The emitter:

- allocates once. The pool is reserved at `capacity`, and spawns beyond it
  are dropped;
- spreads each frame's spawns along the path the source moved that frame
  and pre-ages them, so a fast source leaves a continuous trail instead of
  clumps at frame intervals;
- keeps a spawn rate below the frame rate correct on average;
- is deterministic for a given seed (a small xorshift, no `rand`
  dependency);
- reports `is_active()` while any particle is alive. A demand-driven app
  should keep requesting frames until it turns false, then return
  `FrameDemand::Idle`.

## How each backend draws a batch

Every GPU backend draws particles as the same triangle-fan geometry it
uses for `Circle`:

| Backend | Path |
|---|---|
| Metal (macOS, iOS) | one `SolidGeometry` per particle |
| GLES (Android, Linux) | pushed into the shared solid batch, so one draw call per run |
| DX12 (Windows) | one `DrawItem` for the whole batch (vertices carry color) |
| Software fallbacks | `fill_circle` per particle in the shared `loadngo_renderer::software::RgbaCanvas` |

Until 2026-09-25, every host rasterized each particle on the CPU into its
own texture, every frame. GLES then dropped the batch entirely
(`ParticleBatch => {}`), so on Android and Linux particles only appeared
through that per-particle texture path. A 400-particle effect meant 400
texture uploads a frame, which is the churn
[`PROACTOR_ENGINE_ADOPTION.md`](PROACTOR_ENGINE_ADOPTION.md) rules out.
Fully transparent particles are skipped at draw time.

## Open

- Additive blending, the usual look for fire and glows. It needs a
  blend-mode field and a second pipeline state in each backend.
- Metal still issues one draw per particle. Per-vertex color in its solid
  pipeline would make a batch a single draw, as it already is on GLES and
  DX12.
- Particles are circles only. There are no textured or streaked sprites.
