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

