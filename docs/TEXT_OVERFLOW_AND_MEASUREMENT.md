# Text Overflow and Measurement

Written 2026-09-08, after `RenderTextOverflow` turned out to be unimplemented
on Android and, once implemented, still produced clipped text because the
backend measured differently than it drew.

Companion to [TEXT_RENDERING_TROUBLESHOOTING.md](TEXT_RENDERING_TROUBLESHOOTING.md)
(placement and baselines) and [TEXT_TEXTURE_LIFECYCLE.md](TEXT_TEXTURE_LIFECYCLE.md)
(texture keys and eviction). This one is about *how much text fits*.

## The shape of the problem

Text is rasterized per backend. There is no single text engine: `gfx-metal`
serves macOS and iOS, `host-desktop/src/linux.rs` and `windows.rs` each carry
their own software rasterizer, and `android.rs` carries a third that renders
into a texture. They share the `RenderTextStyle` *vocabulary* but historically
not its *behaviour*.

`RenderTextOverflow` was implemented three times — Metal, Linux, Windows — and
zero times on Android, whose text path copied the field into its request and
hashed it into the texture cache key without ever acting on it. A caller
asking for `EllipsisEnd` got silent truncation instead: the string rasterized
at full width into an over-wide texture, which was then drawn into a smaller
rect and clipped at both ends, losing the leading character and cutting the
tail mid-word with no ellipsis to indicate it.

Nothing errored. Nothing warned. Every test passed, because no test asserted
anything about a platform's *rendered* width. It was found by a person looking
at a phone.

## What was done

1. **One policy, un-gated.** `host-desktop/src/text_overflow.rs` holds
   `fit_text_to_width`, which takes a measurement closure and implements
   `Clip` / `EllipsisEnd` / `EllipsisMiddle`. It is not `cfg`-gated, so it is
   unit-tested on any host. Linux, Windows and Android all call it; each
   supplies only its own glyph metrics. A backend can now fail to *use* the
   policy, but it cannot quietly disagree about what the policy means.
2. **Android implements overflow**, applied before `rasterize_text_command`
   sizes its texture — sizing from unfitted text is what produced the
   over-wide texture in the first place.
3. **Android measures the way it draws.** See below; this was the subtler half.

## Measuring and drawing must be the same walk

Implementing overflow was not enough. Fitting trimmed the string and appended
an ellipsis, the string then measured as fitting — and the rasterizer still
drew it wider than the rect, so the ellipsis that had just been added was
itself clipped off. The truncation was real and invisible, which is worse than
either alone.

The cause was two similar-but-different loops:

| | measuring | drawing |
| --- | --- | --- |
| space | `advance_width` | `advance_width.max(px * 0.3)` |
| glyph | `advance_width.max(ink_width)` | `advance_width`, ink drawn at `cursor + xmin` |

A line with a dozen spaces therefore drew wider than it measured. `android.rs`
now has one `line_rendered_width` that walks a line with exactly the rules
`draw_text` uses, including the space minimum and the fact that a glyph's ink
can reach past its pen advance (`cursor + xmin + width`), so the value it
returns is the rightmost pixel that will actually be lit.

**Rule: anything deciding how much text fits must call the same function the
rasterizer advances by.** Two implementations that agree today will drift, and
the drift is invisible until something is fitted to a width.

## What already exists, and is easy to miss

Games do not need to estimate text widths. Every backend exposes:

- `measure_text_metrics(text, font, font_size, font_scale) -> TextMetrics`
- `wrap_text_lines(text, font, font_size, font_scale, max_width) -> Vec<String>`

`sng-rusty` uses both (`src/runtime/host.rs`), and has its own
`fit_text_to_width` in `src/runtime/mod.rs` predating this work. During this
session `sng-roguelite` was briefly given a *character-budget estimate* for
wrapping, on the incorrect assumption that no measurement API was exposed to
games. It was — check before estimating.

## Backend capability, as of 2026-09-08

| Backend | Text path | Overflow |
| --- | --- | --- |
| macOS / iOS | `gfx-metal` | own implementation |
| Linux | `host-desktop/src/linux.rs` | shared `fit_text_to_width` |
| Windows | `host-desktop/src/windows.rs` | shared `fit_text_to_width` |
| Android | `host-desktop/src/android.rs` | shared `fit_text_to_width` |
| fallback | `host-desktop/src/fallback.rs` | no-op host; renders nothing |

`gfx-metal` still has its own copy. It is correct and tested, and it measures
through CoreText rather than a glyph loop, so it does not share the walk the
software rasterizers do. Folding it in would mean giving it the same
measurement closure shape — worth doing, not done here.

## Fitting is a binary search

`fit_text_to_width` binary-searches the longest prefix (or the fewest
characters removed from the middle) that fits, rather than growing a
candidate one character at a time. A 512-character line costs on the order of
`log2(512)` measurements instead of one per surviving character — for
CoreText that is the difference between roughly seven `CTLine` constructions
and ninety. A test asserts the measurement count stays under 24 for that
line, so a regression to linear scanning fails rather than merely slowing
down.

The search assumes width is monotonic in prefix length. Kerning can violate
that by sub-pixel amounts, so a repair pass walks off any error afterwards.
In practice it runs zero times; its pathological worst case is the linear
scan this replaced, so the search can never be worse than what came before.
A test with a deliberately non-monotonic measurement covers it.

Fitting runs on a **texture-cache miss**, not per frame — `cached_text_raster`
on Metal and `rasterize_text_command` on Android both fit behind their cache
key. Cost is therefore per distinct rendered string. Strings carrying live
values (a health counter, a timer) do churn their keys, so that is where any
fitting cost concentrates.

## The conformance suite, and its one blind spot

`text_overflow::conformance::assert_fitting_conformance` asserts, for a
spread of widths and every overflow mode, that: fitted text measures within
the width it was fitted to; truncation always leaves a visible ellipsis;
`Clip` never alters the string; fitting is idempotent; and surviving text
appears in the original in the original order.

Backend text paths are `#[cfg(target_os = ...)]`, so no single run covers all
of them. `conformance_holds_for_this_platforms_text_backend` runs the suite
against whatever backend the build compiled, so macOS exercises CoreText,
Linux and Windows exercise `fontdue`, and Android would on a device. One
test, a different implementation under it per CI job.

**What it cannot see:** the suite uses one measurement closure for both
fitting and checking, so it is self-consistent by construction. It would have
caught the first half of the Android bug — overflow not implemented at all,
leaving text untruncated and un-ellipsized — but *not* the second half, where
measurement disagreed with rasterization. Nothing that only measures can
detect that.

Closing that gap means a raster-level check: draw into a surface, find the
rightmost lit pixel, assert it falls within the measured width. That is
feasible for the software backends (`linux.rs` and `windows.rs` rasterize
into a plain buffer; Android into a texture) and is the natural next step.
Until then, Android's measurement and drawing agree *by construction* —
`line_rendered_width` exists precisely so there is one walk rather than two —
and the assurance beyond that is a screenshot.

## Rules going forward

1. **A text feature is added to every backend or none.** A field on
   `RenderTextStyle` that one backend ignores is worse than an absent feature,
   because callers will trust it.
2. **Put the policy somewhere un-gated and unit-test it.** Backend code behind
   `#[cfg(target_os = ...)]` cannot be tested from a development machine, so
   anything expressible without platform APIs should live outside the gate.
   (The same `cfg` trap bit the GLES tests: `image_resource_changed` was
   Android/Linux-only but its tests were not, breaking `cargo test` on macOS
   entirely until fixed in this pass.)
3. **Verify text changes by looking at the device.** Every automated check
   passed while this bug was live on a phone. A screenshot is the only thing
   that would have caught it, and it is cheap: `adb exec-out screencap -p`.
4. **Suspect measurement when fitted text still overflows.** If a string was
   trimmed to fit and is still clipped, the fitter is not wrong — the
   measurement it trusted is.
