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

fn fit_with_trailing_ellipsis(
    text: &str,
    max_width: f32,
    measure: &mut impl FnMut(&str) -> f32,
) -> String {
    // Grow a prefix while it still fits with the ellipsis attached, rather
    // than shrinking the whole string one character at a time: the same
    // answer, but the cost is proportional to what survives instead of to
    // what was thrown away, which matters for a long string in a narrow rect.
    let mut fitted = String::new();
    let mut candidate = String::new();
    for character in text.chars() {
        candidate.clear();
        candidate.push_str(&fitted);
        candidate.push(character);
        candidate.push_str(ELLIPSIS);
        if measure(&candidate) > max_width {
            break;
        }
        fitted.push(character);
    }
    if fitted.is_empty() {
        // Not even one character plus the ellipsis fits. Returning the
        // ellipsis alone still says "there is more here", which is more
        // honest than an empty rect.
        return ELLIPSIS.to_string();
    }
    fitted.push_str(ELLIPSIS);
    fitted
}

fn fit_with_middle_ellipsis(
    text: &str,
    max_width: f32,
    measure: &mut impl FnMut(&str) -> f32,
) -> String {
    let characters: Vec<char> = text.chars().collect();
    let mut left = characters.len() / 2;
    let mut right = left;
    while left > 0 && right < characters.len() {
        let candidate: String = characters[..left]
            .iter()
            .chain(ELLIPSIS.chars().collect::<Vec<_>>().iter())
            .chain(characters[right..].iter())
            .collect();
        if measure(&candidate) <= max_width {
            return candidate;
        }
        left -= 1;
        right += 1;
    }
    ELLIPSIS.to_string()
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
    fn an_empty_string_stays_empty() {
        assert_eq!(
            fit_text_to_width("", 0.0, &RenderTextOverflow::EllipsisEnd, monospace),
            ""
        );
    }
}
