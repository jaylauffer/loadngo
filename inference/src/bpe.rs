//! Byte-level BPE, the pieces every such tokenizer shares: the byte alphabet tokens are
//! spelled in, merges applied by rank, and Unicode character classes for the pre-split
//! patterns. The patterns themselves differ by model (gpt-oss's o200k, Qwen's), so each
//! tokenizer scans its own.
//!
//! - **Byte alphabet.** A token is text in the GPT-2 byte alphabet: each byte is one
//!   printable character ([`byte_char`]), so any byte string is spelled without spaces or
//!   control characters.
//! - **Merges.** A list of `left right` pairs; a pair's index is its rank, and the lowest
//!   rank present merges first, the leftmost occurrence on a tie ([`Merges::apply`]).

use std::collections::HashMap;

/// A sorted list of inclusive code point ranges, from a regex class.
pub struct Class(Vec<(char, char)>);

impl Class {
    /// The class a regex such as `\p{L}` or `[\p{Ll}\p{M}]` names, from `regex-syntax`'s
    /// own Unicode tables.
    ///
    /// # Panics
    /// When `pattern` is not a Unicode class.
    #[must_use]
    pub fn parse(pattern: &str) -> Self {
        use regex_syntax::hir::{Class as HirClass, HirKind};
        let hir = regex_syntax::parse(pattern).expect("a valid class");
        match hir.kind() {
            HirKind::Class(HirClass::Unicode(class)) => Self(
                class
                    .ranges()
                    .iter()
                    .map(|r| (r.start(), r.end()))
                    .collect(),
            ),
            other => panic!("{pattern} is not a Unicode class: {other:?}"),
        }
    }

    #[must_use]
    pub fn contains(&self, c: char) -> bool {
        self.0
            .binary_search_by(|&(start, end)| {
                if end < c {
                    std::cmp::Ordering::Less
                } else if start > c {
                    std::cmp::Ordering::Greater
                } else {
                    std::cmp::Ordering::Equal
                }
            })
            .is_ok()
    }
}

fn printable(b: u8) -> bool {
    matches!(b, b'!'..=b'~' | 0xa1..=0xac | 0xae..=0xff)
}

/// The character the GPT-2 byte alphabet spells `byte` with: a printable byte is itself;
/// the others, in order, are U+0100 onwards.
#[must_use]
pub fn byte_char(byte: u8) -> char {
    if printable(byte) {
        char::from(byte)
    } else {
        let index = (0..byte).filter(|&b| !printable(b)).count();
        char::from_u32(256 + u32::try_from(index).expect("at most 256")).expect("in range")
    }
}

/// The byte `c` spells in the byte alphabet, if any.
#[must_use]
pub fn char_byte(c: char) -> Option<u8> {
    let code = u32::from(c);
    if let Ok(b) = u8::try_from(code) {
        return printable(b).then_some(b);
    }
    let index = usize::try_from(code.checked_sub(256)?).ok()?;
    (0..=255_u8).filter(|&b| !printable(b)).nth(index)
}

/// The bytes a token spelled in the byte alphabet stands for, or `None` when a character
/// is outside it.
#[must_use]
pub fn token_bytes(spelled: &str) -> Option<Vec<u8>> {
    spelled.chars().map(char_byte).collect()
}

/// Ranked merges over token ids.
pub struct Merges {
    /// `(left, right)` -> `(rank, merged)`.
    pairs: HashMap<(u32, u32), (u32, u32)>,
    /// The id of each single byte.
    byte_ids: [u32; 256],
}

impl Merges {
    /// From the single-byte ids and `(left, right, merged)` triples in rank order.
    #[must_use]
    pub fn new(byte_ids: [u32; 256], ranked: impl IntoIterator<Item = (u32, u32, u32)>) -> Self {
        let mut pairs = HashMap::new();
        for (rank, (l, r, m)) in ranked.into_iter().enumerate() {
            // A pair listed twice keeps its first (lowest) rank.
            pairs
                .entry((l, r))
                .or_insert((u32::try_from(rank).unwrap_or(u32::MAX), m));
        }
        Self { pairs, byte_ids }
    }

    /// The tokens of one pre-split piece, appended to `out`: its bytes, merged lowest
    /// rank first, the leftmost pair on a tie.
    pub fn apply(&self, piece: &[u8], out: &mut Vec<u32>) {
        let mut symbols: Vec<u32> = piece
            .iter()
            .map(|&b| self.byte_ids[usize::from(b)])
            .collect();
        loop {
            let best = symbols
                .windows(2)
                .enumerate()
                .filter_map(|(at, pair)| {
                    self.pairs
                        .get(&(pair[0], pair[1]))
                        .map(|&(rank, merged)| (rank, at, merged))
                })
                .min();
            let Some((_, at, merged)) = best else { break };
            symbols[at] = merged;
            symbols.remove(at + 1);
        }
        out.extend(symbols);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_byte_alphabet_is_a_bijection_and_spells_space_as_g_dot() {
        let chars: std::collections::HashSet<char> = (0..=255).map(byte_char).collect();
        assert_eq!(chars.len(), 256);
        assert_eq!(byte_char(b' '), 'Ġ');
        assert_eq!(byte_char(b'\n'), 'Ċ');
        assert_eq!(byte_char(b'a'), 'a');
        assert_eq!(token_bytes("Ġhi").unwrap(), b" hi");
        assert!((0..=255).all(|b| char_byte(byte_char(b)) == Some(b)));
        assert!(token_bytes("<|x|> ").is_none());
    }

    #[test]
    fn merges_take_the_lowest_rank_then_the_leftmost() {
        let mut byte_ids = [0_u32; 256];
        for (b, id) in byte_ids.iter_mut().enumerate() {
            *id = u32::try_from(b).unwrap();
        }
        let (a, b) = (u32::from(b'a'), u32::from(b'b'));
        // rank 0: b+b -> 300; rank 1: a+b -> 301; rank 2: 301+300 -> 302
        let merges = Merges::new(byte_ids, [(b, b, 300), (a, b, 301), (301, 300, 302)]);
        let mut out = Vec::new();
        merges.apply(b"abbb", &mut out);
        // bb merges first (leftmost of two), then nothing pairs a+300 or 300+b.
        assert_eq!(out, vec![a, 300, b]);
        out.clear();
        merges.apply(b"abbb"[..3].as_ref(), &mut out);
        assert_eq!(out, vec![a, 300]);
        out.clear();
        merges.apply(b"abab", &mut out);
        assert_eq!(out, vec![301, 301]);
    }
}
