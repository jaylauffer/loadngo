# Text texture lifecycle

How a `FrameCommand::Text` becomes a GPU texture on the backends that have
no GPU text path (Windows/D3D12, Linux/GLES, Android/GLES), and — the part
that has bitten us — how those textures are *reclaimed*.

## The pipeline

`prepare_dx12_frame` / `prepare_gles_frame` rewrite each `Text` command into
an `Image` pointing at a texture rasterized on the CPU, keyed by a hash of
everything that affects its pixels: the string, size, colour, alignment,
font. Geometry for that rewrite is decided once, in
`loadngo_renderer::text_texture_layout` — see
[CLIP_AND_SCISSOR.md](CLIP_AND_SCISSOR.md) for why it lives there.

## Why keys churn

**A texture key is per rendered string, so any text containing a live value
mints a brand new key every time that value changes.** A score, a timer, an
FPS counter, a coordinate readout — each produces a stream of keys that will
never be looked up again. This is the single most important fact about this
subsystem, and both bugs below come from ignoring it.

Static labels are the opposite: identical every frame, so they hit cache.

## Two caches, two reclamation rules

**Host side** (`state.generated_texture_cache`): rebuilt each frame to hold
only the keys that frame used. Text still on screen is re-inserted every
frame and stays cached; text that changed falls out immediately. It used to
be carried forward and added to forever.

**Backend side** (`gfx-dx12`'s `textures` + descriptor heap): capped at
`MAX_TEXTURES` (4096) descriptors. Slots are reused via
`DescriptorAllocator`: a freed slot first, then an unused one, then evict
the least-recently-used texture. It used to only ever count upward, so
`next_descriptor_index` reached 4096 and every later frame failed.

Two properties worth keeping:

- **Eviction is safe only because the DX12 backend is fully synchronous** —
  `wait_for_gpu` runs after every Present and every upload, so an evicted
  resource cannot still be in flight. If that backend ever gains frames in
  flight, eviction must be deferred behind the fence.
- **A texture drawn in the current frame is never evicted.** A frame that
  genuinely needs more than `MAX_TEXTURES` distinct textures reports
  exhaustion rather than quietly dropping something it is drawing.

## What went wrong (2026-09-08)

`sng-zhoenus` on Windows:

```
Windows D3D12 render failed, disabling the backend and falling back to
software: backend error: texture descriptor heap exhausted: requested key
'generated://text/6b2a1f31885f4ac0', capacity 4096
```

Its HUD renders `Saved:`/`Speed:` values that change constantly. After 4096
distinct strings the heap was full, and because the host disables a backend
that errors, the session ran on the software renderer from then on — the
visible symptom being a sudden, permanent performance drop rather than
anything that looks like a leak.

The fallback behaviour is worth noting on its own: **one recoverable
resource error permanently demotes the backend for the rest of the run.**
That makes any bounded-resource bug look catastrophic instead of transient,
and is why this was worth fixing at the resource level rather than by
raising the cap.

## Testing

`DescriptorAllocator` and its tests live *outside* `gfx-dx12`'s
`#[cfg(target_os = "windows")]` module precisely so the policy can be
exercised on any host; the `unsafe` D3D12 calls stay thin around it. Run
`./scripts/check-windows.sh` to type-check the Windows-only code from
macOS or Linux. Neither substitutes for running it on Windows hardware.
