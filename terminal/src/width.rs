//! How many cells a character takes: two for East Asian wide and
//! fullwidth characters and most emoji, none for combining marks and
//! other zero-width characters, one otherwise. A compact table of the
//! common ranges, not the whole Unicode database.

/// Zero-width ranges: combining marks, joiners, variation selectors.
const ZERO: &[(u32, u32)] = &[
    (0x0300, 0x036f),
    (0x0483, 0x0489),
    (0x0591, 0x05bd),
    (0x0610, 0x061a),
    (0x064b, 0x065f),
    (0x0e31, 0x0e31),
    (0x0e34, 0x0e3a),
    (0x0e47, 0x0e4e),
    (0x1ab0, 0x1aff),
    (0x1dc0, 0x1dff),
    (0x200b, 0x200f),
    (0x2028, 0x202e),
    (0x2060, 0x2064),
    (0x20d0, 0x20ff),
    (0xfe00, 0xfe0f),
    (0xfe20, 0xfe2f),
    (0xfeff, 0xfeff),
    (0x1f3fb, 0x1f3ff),
    (0xe0000, 0xe0fff),
];

/// Wide ranges: CJK, Hangul, fullwidth forms, emoji.
const WIDE: &[(u32, u32)] = &[
    (0x1100, 0x115f),
    (0x231a, 0x231b),
    (0x2329, 0x232a),
    (0x23e9, 0x23ec),
    (0x23f0, 0x23f0),
    (0x23f3, 0x23f3),
    (0x25fd, 0x25fe),
    (0x2614, 0x2615),
    (0x2648, 0x2653),
    (0x267f, 0x267f),
    (0x2693, 0x2693),
    (0x26a1, 0x26a1),
    (0x26aa, 0x26ab),
    (0x26bd, 0x26be),
    (0x26c4, 0x26c5),
    (0x26ce, 0x26ce),
    (0x26d4, 0x26d4),
    (0x26ea, 0x26ea),
    (0x26f2, 0x26f3),
    (0x26f5, 0x26f5),
    (0x26fa, 0x26fa),
    (0x26fd, 0x26fd),
    (0x2705, 0x2705),
    (0x270a, 0x270b),
    (0x2728, 0x2728),
    (0x274c, 0x274c),
    (0x274e, 0x274e),
    (0x2753, 0x2755),
    (0x2757, 0x2757),
    (0x2795, 0x2797),
    (0x27b0, 0x27b0),
    (0x27bf, 0x27bf),
    (0x2b1b, 0x2b1c),
    (0x2b50, 0x2b50),
    (0x2b55, 0x2b55),
    (0x2e80, 0x303e),
    (0x3041, 0x33ff),
    (0x3400, 0x4dbf),
    (0x4e00, 0x9fff),
    (0xa000, 0xa4cf),
    (0xa960, 0xa97f),
    (0xac00, 0xd7a3),
    (0xf900, 0xfaff),
    (0xfe10, 0xfe19),
    (0xfe30, 0xfe6f),
    (0xff00, 0xff60),
    (0xffe0, 0xffe6),
    (0x16fe0, 0x16fe4),
    (0x17000, 0x18cff),
    (0x1b000, 0x1b2ff),
    (0x1f004, 0x1f004),
    (0x1f0cf, 0x1f0cf),
    (0x1f18e, 0x1f18e),
    (0x1f191, 0x1f19a),
    (0x1f200, 0x1f251),
    (0x1f300, 0x1f3fa),
    (0x1f400, 0x1f64f),
    (0x1f680, 0x1f6ff),
    (0x1f7e0, 0x1f7eb),
    (0x1f90c, 0x1f9ff),
    (0x1fa70, 0x1faff),
    (0x20000, 0x2fffd),
    (0x30000, 0x3fffd),
];

fn in_ranges(code: u32, ranges: &[(u32, u32)]) -> bool {
    ranges
        .binary_search_by(|&(start, end)| {
            if end < code {
                std::cmp::Ordering::Less
            } else if start > code {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

pub fn char_width(ch: char) -> u8 {
    let code = ch as u32;
    if code < 0x300 {
        return 1;
    }
    if in_ranges(code, ZERO) {
        0
    } else if in_ranges(code, WIDE) {
        2
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widths() {
        for (ch, width) in [
            ('a', 1),
            ('é', 1),
            ('─', 1),
            ('日', 2),
            ('한', 2),
            ('😀', 2),
            ('🦀', 2),
            ('\u{301}', 0),
            ('\u{200d}', 0),
            ('Ａ', 2),
        ] {
            assert_eq!(char_width(ch), width, "{ch:?}");
        }
        for table in [ZERO, WIDE] {
            assert!(
                table.windows(2).all(|pair| pair[0].1 < pair[1].0),
                "sorted, apart"
            );
        }
    }
}
