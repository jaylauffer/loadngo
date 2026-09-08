//! Fitting a single line of text to a width, shared by every software text
//! rasterizer in this crate.
//!
//! This exists because it previously did *not*. `RenderTextOverflow` was
//! implemented three separate times — Linux, Windows, and `gfx-metal` (which
//! serves macOS and iOS) — and not at all on Android, whose software text
//! path copied the field into its request and hashed it into the texture key
//! without ever acting on it. The result was silent: `EllipsisEnd` simply did
//! nothing there, an over-wide string rasterized into an over-wide texture,
//! and the caller saw its line raw-clipped at both ends with no ellipsis to
//! signal the truncation. Nothing failed, so nothing was noticed until it was
//! seen on a device.
//!
//! Keeping the policy here, measurement-agnostic and un-gated by
//! `target_os`, means it can be unit-tested from any host and a backend can
//! only *fail* to use it, never quietly disagree about what it means.

use loadngo_host_core::RenderTextOverflow;

/// The ellipsis appended (or inserted) when text must be shortened. Three
/// ASCII periods rather than `…`: this crate renders with whatever default
/// font each platform provides, and a single-glyph ellipsis is not reliably
/// present in those — the same reasoning that keeps ASCII markers in
/// `sng-roguelite`'s achievement list.
pub const ELLIPSIS: &str = "...";

/// Shortens `text` so that it measures no wider than `max_width`, following
/// `overflow`.
///
/// `measure` returns the rendered width of a candidate string. It is called
/// repeatedly, so a backend should pass something cheap or memoized; every
/// caller in this crate passes its own glyph-metrics function.
///
/// Text that already fits is returned unchanged, so this is safe to apply
/// more than once along a rasterization path — which matters on Android,
/// where the texture is sized in one function and drawn in another.
///
/// [`RenderTextOverflow::Clip`] returns the text untouched: clipping is the
/// caller's job (a scissor rect, or a texture bounded to the destination),
/// not something to bake into the string.
pub fn fit_text_to_width(
    text: &str,
    max_width: f32,
    overflow: &RenderTextOverflow,
    mut measure: impl FnMut(&str) -> f32,
) -> String {
    if text.is_empty() || measure(text) <= max_width {
        return text.to_string();
    }
    match overflow {
        RenderTextOverflow::Clip => text.to_string(),
        RenderTextOverflow::EllipsisEnd => {
            fit_with_trailing_ellipsis(text, max_width, &mut measure)
        }
        RenderTextOverflow::EllipsisMiddle => {
            fit_with_middle_ellipsis(text, max_width, &mut measure)
        }
    }
}

/// Builds the candidate that keeps the first `length` characters.
fn trailing_candidate(characters: &[char], length: usize) -> String {
    let mut candidate: String = characters[..length].iter().collect();
    candidate.push_str(ELLIPSIS);
    candidate
}

/// Builds the candidate that removes `removed` characters from each side of
/// the midpoint, keeping both ends.
fn middle_candidate(characters: &[char], removed: usize) -> String {
    let half = characters.len() / 2;
    let left = half.saturating_sub(removed);
    let right = (half + removed).min(characters.len());
    let mut candidate: String = characters[..left].iter().collect();
    candidate.push_str(ELLIPSIS);
    candidate.extend(characters[right..].iter());
    candidate
}

/// Largest `removed` value worth trying: beyond it one side is exhausted.
fn middle_search_bound(length: usize) -> usize {
    let half = length / 2;
    half.min(length - half)
}

fn fit_with_trailing_ellipsis(
    text: &str,
    max_width: f32,
    measure: &mut impl FnMut(&str) -> f32,
) -> String {
    let characters: Vec<char> = text.chars().collect();
    // Binary search the longest prefix that still fits with the ellipsis
    // attached. Growing one character at a time gave the same answer but
    // cost one measurement per surviving character — around ninety CoreText
    // line constructions for a long UI row, against roughly seven here.
    //
    // We only reach this function because the whole string is too wide, so
    // the full length is a known-bad upper bound.
    let mut low = 0usize;
    let mut high = characters.len();
    while low < high {
        let middle = low + (high - low).div_ceil(2);
        if measure(&trailing_candidate(&characters, middle)) <= max_width {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    let length = repair_trailing(&characters, low, max_width, measure);
    if length == 0 {
        // Not even one character plus the ellipsis fits. Returning the
        // ellipsis alone still says "there is more here", which is more
        // honest than an empty rect.
        return ELLIPSIS.to_string();
    }
    trailing_candidate(&characters, length)
}

/// Corrects the binary search for a font whose width is not perfectly
/// monotonic in prefix length.
///
/// The search assumes adding a character never makes a line narrower, which
/// kerning can violate by sub-pixel amounts. Rather than trust that, walk
/// off any error afterwards: in practice both loops run zero times, and the
/// pathological worst case is the linear scan this replaced — so the search
/// can never be *worse* than what came before, only usually far better.
fn repair_trailing(
    characters: &[char],
    found: usize,
    max_width: f32,
    measure: &mut impl FnMut(&str) -> f32,
) -> usize {
    let mut length = found;
    while length > 0 && measure(&trailing_candidate(characters, length)) > max_width {
        length -= 1;
    }
    while length < characters.len()
        && measure(&trailing_candidate(characters, length + 1)) <= max_width
    {
        length += 1;
    }
    length
}

fn fit_with_middle_ellipsis(
    text: &str,
    max_width: f32,
    measure: &mut impl FnMut(&str) -> f32,
) -> String {
    let characters: Vec<char> = text.chars().collect();
    let bound = middle_search_bound(characters.len());
    // Same reasoning as the trailing case, searching instead for the
    // *fewest* characters that have to be removed from the middle.
    let mut low = 0usize;
    let mut high = bound;
    while low < high {
        let middle = low + (high - low) / 2;
        if measure(&middle_candidate(&characters, middle)) <= max_width {
            high = middle;
        } else {
            low = middle + 1;
        }
    }
    let mut removed = low;
    while removed < bound && measure(&middle_candidate(&characters, removed)) > max_width {
        removed += 1;
    }
    while removed > 0 && measure(&middle_candidate(&characters, removed - 1)) <= max_width {
        removed -= 1;
    }
    let candidate = middle_candidate(&characters, removed);
    if measure(&candidate) > max_width {
        return ELLIPSIS.to_string();
    }
    candidate
}

/// Invariants every backend's text fitting must satisfy, checked against
/// that backend's own measurement.
///
/// Backend text paths are `#[cfg(target_os = ...)]`, so no single test run
/// can exercise all of them. This suite is the next best thing: each
/// platform runs it against its own measurement in its own CI job, so a
/// backend that measures differently than it draws, or ignores a style
/// field, fails on the platform that owns it rather than silently shipping.
///
/// The failure this exists to catch was exactly that — Android ignored
/// `RenderTextOverflow` entirely, and then, once it did not, measured a
/// space differently than it drew one. Both are invariant violations here.
#[cfg(test)]
pub(crate) mod conformance {
    use super::{fit_text_to_width, ELLIPSIS};
    use loadngo_host_core::RenderTextOverflow;

    /// Text long enough to need truncating in any plausible rect, with
    /// spaces (the advance rule that diverged on Android), punctuation, and
    /// multi-byte characters.
    pub(crate) const SAMPLES: [&str; 4] = [
        "[x] Discovery: Long Shot — Build a stack combining Ghost Bore and Phase Needle. — earned 2026-09-08",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "spaces     between     words     stretch     a     line     out",
        "ünïcödé wîdth tést with combining accents and a trailing ellipsis",
    ];

    /// Asserts `measure` and the shared fitter agree, for every overflow
    /// mode and a spread of widths. `label` names the backend in failures.
    pub(crate) fn assert_fitting_conformance(label: &str, mut measure: impl FnMut(&str) -> f32) {
        for sample in SAMPLES {
            let full_width = measure(sample);
            assert!(
                full_width > 0.0,
                "{label}: measurement returned {full_width} for {sample:?}"
            );
            for fraction in [0.05_f32, 0.25, 0.5, 0.75, 0.95, 1.5] {
                let max_width = full_width * fraction;
                assert_ellipsis_modes_fit(label, sample, max_width, &mut measure);
                // `Clip` must hand the string back untouched: bounding it is
                // the caller's job, not the fitter's.
                assert_eq!(
                    fit_text_to_width(sample, max_width, &RenderTextOverflow::Clip, &mut measure),
                    sample,
                    "{label}: Clip must not alter the string"
                );
            }
        }
    }

    fn assert_ellipsis_modes_fit(
        label: &str,
        sample: &str,
        max_width: f32,
        measure: &mut impl FnMut(&str) -> f32,
    ) {
        for overflow in [
            RenderTextOverflow::EllipsisEnd,
            RenderTextOverflow::EllipsisMiddle,
        ] {
            let fitted = fit_text_to_width(sample, max_width, &overflow, &mut *measure);
            let fitted_width = measure(&fitted);

            // The invariant that matters: what comes back must fit the width
            // it was fitted to. A backend measuring differently than it draws
            // breaks this only when it is *also* the drawing width, which is
            // why each backend runs this against its own measurement.
            if fitted != ELLIPSIS {
                assert!(
                    fitted_width <= max_width,
                    "{label}: {overflow:?} returned {fitted:?} at {fitted_width} for a {max_width} width"
                );
            }

            // Truncation must be visible, never silent.
            if fitted != sample {
                assert!(
                    fitted.contains(ELLIPSIS),
                    "{label}: {overflow:?} truncated {sample:?} to {fitted:?} without an ellipsis"
                );
            }

            // Applying the result again must be a no-op — backends apply
            // fitting at more than one point along a rasterization path.
            let refitted = fit_text_to_width(&fitted, max_width, &overflow, &mut *measure);
            assert_eq!(
                refitted, fitted,
                "{label}: {overflow:?} fitting is not idempotent for {sample:?}"
            );

            // Whatever survives must be in the original order, not reordered
            // or invented.
            for piece in fitted.split(ELLIPSIS).filter(|piece| !piece.is_empty()) {
                assert!(
                    sample.contains(piece),
                    "{label}: {overflow:?} produced {piece:?}, absent from {sample:?}"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{fit_text_to_width, ELLIPSIS};
    use loadngo_host_core::RenderTextOverflow;

    /// One unit per character — enough to exercise the policy without
    /// dragging a font into the test.
    fn monospace(text: &str) -> f32 {
        text.chars().count() as f32
    }

    #[test]
    fn text_that_already_fits_is_untouched() {
        for overflow in [
            RenderTextOverflow::Clip,
            RenderTextOverflow::EllipsisEnd,
            RenderTextOverflow::EllipsisMiddle,
        ] {
            assert_eq!(
                fit_text_to_width("hello", 10.0, &overflow, monospace),
                "hello"
            );
        }
    }

    #[test]
    fn clip_leaves_the_string_for_the_caller_to_bound() {
        assert_eq!(
            fit_text_to_width("hello world", 5.0, &RenderTextOverflow::Clip, monospace),
            "hello world"
        );
    }

    #[test]
    fn trailing_ellipsis_fits_within_the_width() {
        let fitted = fit_text_to_width(
            "achievement earned 2026-09-08",
            10.0,
            &RenderTextOverflow::EllipsisEnd,
            monospace,
        );
        assert!(monospace(&fitted) <= 10.0, "{fitted:?} is too wide");
        assert!(fitted.ends_with(ELLIPSIS));
        assert!(fitted.starts_with("achi"));
    }

    #[test]
    fn middle_ellipsis_keeps_both_ends_and_fits() {
        let fitted = fit_text_to_width(
            "prototype_branch_floor",
            12.0,
            &RenderTextOverflow::EllipsisMiddle,
            monospace,
        );
        assert!(monospace(&fitted) <= 12.0, "{fitted:?} is too wide");
        assert!(fitted.contains(ELLIPSIS));
        assert!(fitted.starts_with('p'));
        assert!(fitted.ends_with('r'));
    }

    #[test]
    fn a_width_too_small_for_anything_still_signals_truncation() {
        for overflow in [
            RenderTextOverflow::EllipsisEnd,
            RenderTextOverflow::EllipsisMiddle,
        ] {
            assert_eq!(
                fit_text_to_width("hello world", 1.0, &overflow, monospace),
                ELLIPSIS
            );
        }
    }

    #[test]
    fn fitting_is_idempotent_so_a_backend_may_apply_it_twice() {
        // Android sizes its text texture in one function and draws into it
        // in another; both apply this, and the second must be a no-op.
        let once = fit_text_to_width(
            "a considerably longer line than fits",
            12.0,
            &RenderTextOverflow::EllipsisEnd,
            monospace,
        );
        let twice = fit_text_to_width(&once, 12.0, &RenderTextOverflow::EllipsisEnd, monospace);
        assert_eq!(once, twice);
    }

    #[test]
    fn multi_byte_characters_are_never_split() {
        let fitted = fit_text_to_width(
            "ünïcödé wîdth tést",
            8.0,
            &RenderTextOverflow::EllipsisEnd,
            monospace,
        );
        assert!(monospace(&fitted) <= 8.0);
        // Rebuilding from chars proves no partial code point was emitted.
        assert_eq!(fitted.chars().collect::<String>(), fitted);
    }

    #[test]
    fn the_search_is_logarithmic_not_linear() {
        // The whole point of the binary search. A 512-character line used to
        // cost one measurement per surviving character; it must now cost a
        // number of measurements on the order of log2(512) plus a little
        // repair slack, and nothing close to 512.
        let text = "x".repeat(512);
        for overflow in [
            RenderTextOverflow::EllipsisEnd,
            RenderTextOverflow::EllipsisMiddle,
        ] {
            let mut calls = 0usize;
            let fitted = fit_text_to_width(&text, 100.0, &overflow, |candidate| {
                calls += 1;
                monospace(candidate)
            });
            assert!(monospace(&fitted) <= 100.0);
            assert!(
                calls <= 24,
                "{overflow:?} took {calls} measurements for a 512-character line"
            );
        }
    }

    /// A width that is *almost* monotonic in prefix length, the way a real
    /// font with kerning is: adding a character can shave a fraction off.
    /// The binary search assumes monotonicity, so the repair pass has to
    /// hold the invariant on its own.
    fn kerned(text: &str) -> f32 {
        let count = text.chars().count() as f32;
        let kerning = if text.contains("AV") { -0.4 } else { 0.0 };
        count + kerning
    }

    #[test]
    fn a_non_monotonic_width_still_produces_something_that_fits() {
        for overflow in [
            RenderTextOverflow::EllipsisEnd,
            RenderTextOverflow::EllipsisMiddle,
        ] {
            for width in [4.0_f32, 7.5, 12.0, 19.0] {
                let fitted =
                    fit_text_to_width("WAVE AVAILABLE AVAST AVENUE", width, &overflow, kerned);
                if fitted != ELLIPSIS {
                    assert!(
                        kerned(&fitted) <= width,
                        "{overflow:?} returned {fitted:?} at width {width}"
                    );
                }
            }
        }
    }

    #[test]
    fn conformance_holds_for_a_synthetic_measurement() {
        super::conformance::assert_fitting_conformance("monospace", monospace);
        super::conformance::assert_fitting_conformance("kerned", kerned);
    }

    /// Runs the same suite against whichever backend this build actually
    /// compiles — CoreText on macOS/iOS, `fontdue` on Linux, Windows and
    /// Android. One test, but a different implementation under it in each
    /// platform's CI job, which is the only way to cover `cfg`-gated text
    /// paths from a single source.
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "linux",
        target_os = "windows",
        target_os = "android"
    ))]
    #[test]
    fn conformance_holds_for_this_platforms_text_backend() {
        assert_fitting_conformance_for_platform();
    }

    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "linux",
        target_os = "windows",
        target_os = "android"
    ))]
    fn assert_fitting_conformance_for_platform() {
        super::conformance::assert_fitting_conformance(std::env::consts::OS, |candidate| {
            crate::measure_text_metrics(candidate, None, 14, 1.0).width
        });
    }

    #[test]
    fn an_empty_string_stays_empty() {
        assert_eq!(
            fit_text_to_width("", 0.0, &RenderTextOverflow::EllipsisEnd, monospace),
            ""
        );
    }
}
