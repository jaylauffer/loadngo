# Text Rendering Troubleshooting

This note records the main lessons from the March 2026 macOS text-placement debugging work.

The short version:
- do not debug shared text placement by intuition
- do not trust subtle screenshots alone
- make the harness itself explain what is happening

## What Went Wrong

We churned for too long because several different problems were mixed together:
- typographic line-box behavior vs optical appearance
- baseline math vs final raster-image placement
- runtime/editor regressions vs backend-only behavior
- stale processes vs current builds

We also repeatedly made weak conclusions from low-signal screenshots:
- center guides were too faint
- text labels used to explain metrics were themselves clipped
- multiple windows/processes made it easy to inspect the wrong instance

## Practical Rules

When changing shared text rendering:

1. Use a dedicated harness first.
- `text_harness` for general widget/control text
- `text_input_harness` for editable text behavior
- `text_metrics_harness` for line-box / baseline / vertical-placement work

2. Make the harness visually explicit.
- draw the control center line in a high-contrast color
- draw the logical line-box outline
- draw the logical line-box center line
- prefer obvious overlays over subtle “looks better” judgments

3. Emit numeric placement data to stdout.
- print ascent, descent, leading, baseline-from-top, line-box height, and padding
- print per-sample deltas from the control center
- if the harness can compute the placement, log it

4. Verify one process at a time.
- kill stale harness/runtime/editor windows first
- launch one target
- capture it
- stop it
- verify with `pgrep` before claiming it is gone

5. Separate the layers of the bug.
- first: is the logical line box defined correctly?
- second: is the baseline derived correctly from ascent/descent/leading?
- third: is the raster image placed correctly inside that box after flip/crop/padding?

6. Keep typographic and optical questions separate.
- `LogicalLineBox` is typographic
- if a control later needs optical adjustment, that should be an explicit separate mode
- do not silently redefine `LogicalLineBox` to “looks centered”

## The Important Technical Lesson

For the macOS Metal/CoreGraphics path, the decisive bug was not the font metrics themselves.

The logical line box was centered correctly, but the glyphs were still too high because the baseline was being mapped into the flipped raster as though it were measured from the top of the logical box.

The fix was:
- keep `baseline_from_top` as a typographic metric
- when drawing into the flipped CoreGraphics raster, map that baseline using its offset from the bottom of the logical line box

That is the core reason the final placement started behaving correctly.

## Recommended Debugging Sequence

When text looks wrong:

1. Reproduce in `text_metrics_harness`.
2. Add strong visual guides before changing placement math.
3. Print numeric placement deltas to stdout.
4. Prove whether the line box is wrong or the raster placement is wrong.
5. Fix the backend in isolation.
6. Only then carry the fix into:
- `text_harness`
- `text_input_harness`
- `sng-rusty`
- `sng_rusty_editor`

## Do Not Repeat

Avoid these failure modes:
- “the screenshot looks fixed” without guides or logged deltas
- changing widget padding to compensate for backend placement bugs
- trying to make one text mode satisfy both typographic and optical goals
- claiming processes are stopped without a fresh `pgrep`

## Future Follow-Up

This note is macOS-first, but the same discipline should apply to every backend:
- define the line-box contract once
- instrument the harnesses
- port the same verification method to Android / GLES and later Windows/Linux

## Runtime Memory Troubleshooting

The same discipline applies to runtime stability work, not just placement bugs.

In March 2026, the `sng-rusty` runtime looked like a Metal memory blow-up at first, but the actual sequence was:
- excessive frame demand multiplied renderer work
- text raster diagnostics showed the text cache was already effective
- `vmmap` showed the dominant growth was `MALLOC_SMALL`, not GPU residency
- `heap` then pointed directly at persistent CoreText/CoreFoundation objects like `CTLine`, `CTRun`, `CGFont`, and `CFData`

The decisive fix was not another cache tweak. It was ownership:
- `TextLayout` in `loadngo-gfx-metal` created CoreText/CoreFoundation objects on the success path
- those objects were only released in one raster path, not by ownership
- adding `Drop` to release them collapsed the runtime from multi-GB growth to roughly steady-state memory

Practical rules for renderer/runtime memory debugging:

1. Add counters before optimizing.
- count generated text requests
- count cache hits/misses
- count transient uploads and bytes
- log them under an env flag, not unconditionally

2. Use `vmmap` before blaming Metal.
- distinguish `MALLOC_SMALL` / `MALLOC_LARGE` from `IOSurface` / `IOAccelerator`
- if heap dominates, look for ownership and object-lifetime bugs first

3. Use `heap` to identify object families.
- if the top consumers are CoreText/CoreFoundation classes, inspect retain/release paths
- do not assume the issue is GPU-side just because the renderer is involved

4. Add ownership regressions, not just frame-count regressions.
- if a path creates native text/layout objects, add a test that repeated measure/raster cycles return the live-object count to zero

5. Separate “fewer frames” from “less memory”.
- reduced frame cadence helps
- skipped identical submissions help
- but neither substitutes for correct ownership

## iOS Retina Text Was Rasterized at 1x

In August 2026, HUD text in `sng-zhoenus` was reported as reading thin and
grayish, and specifically worse on iOS than Android for the same build.
Two things were ruled out before finding the real cause:
- not a logical-vs-physical-pixel units bug (both platforms report
  `Viewport` the same logical, density-independent surface size)
- not fully explained by low contrast against a busy background (an
  outline drawn behind the text helped, but iOS still lagged Android
  with it)

The actual bug: `gfx-metal::rasterize_text` shaped and rasterized every
glyph via CoreText at the literal logical `font_size`, in points, with no
Retina-scale multiplication anywhere in the pipeline. The resulting
bitmap's raw pixel dimensions were then used directly as the on-screen
quad's size. `MetalSurface::sync_drawable_size` *did* correctly scale the
drawable itself by `backingScaleFactor`/`contentScaleFactor` — so the GPU
had to upscale a 1x-resolution glyph bitmap to fill a 2x/3x-resolution
quad on every Retina display. Android's renderer already multiplies
`font_size` by display density before rasterizing (`scale_frame_command`
in `host-desktop/src/android.rs`) and never showed the symptom — that
asymmetry was the clue that pointed at rasterization resolution rather
than layout or contrast.

The fix: `rasterize_text`/`cached_text_raster`/`rasterize_text_request`
take a `scale` parameter (`MetalSurface::content_scale`, the same
`backingScaleFactor`/`contentScaleFactor` query the drawable sizing
already used). CoreText shapes and rasterizes at `font_size * scale`, so
the bitmap itself is genuinely higher-resolution; the draw call then
displays that bitmap at its original logical-point footprint (divide the
pixel dimensions back down by `scale`), so nothing about layout,
wrapping, or positioning changes — only sharpness. At `scale = 1.0`
(every pre-existing caller — `debug_text_placement`, every unit test in
this crate) the change is a no-op by construction, which is why it didn't
need new test coverage to be verified safe: the existing suite passing
unchanged after the change *is* the regression check.

Practical rule this adds to the list above: when a symptom is platform-
asymmetric on otherwise-shared rendering code, look for where per-
platform code diverges in exactly the dimension the symptom concerns
(here: pixel density) before reaching for a cross-platform explanation
like contrast or layout.
