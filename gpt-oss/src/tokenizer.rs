//! The o200k byte-level BPE tokenizer, as a gpt-oss GGUF describes it.
//!
//! - **Vocabulary.** `tokenizer.ggml.tokens` holds every token as text in the GPT-2 byte
//!   alphabet: each byte is one printable character, so any byte string is spelled
//!   without spaces or control characters ([`byte_char`]).
//! - **Merges.** `tokenizer.ggml.merges` lists `"left right"` pairs; a pair's index is its
//!   rank, and the lowest rank present merges first.
//! - **Pre-split.** Before merging, text is split as the o200k pattern splits it (the
//!   GGUF names it `gpt-4o`), one alternative after another, the first that matches:
//!
//!   ```text
//!   1  [^\r\n\p{L}\p{N}]?[\p{Lu}\p{Lt}\p{Lm}\p{Lo}\p{M}]*[\p{Ll}\p{Lm}\p{Lo}\p{M}]+(?i:'s|'t|'re|'ve|'m|'ll|'d)?
//!   2  [^\r\n\p{L}\p{N}]?[\p{Lu}\p{Lt}\p{Lm}\p{Lo}\p{M}]+[\p{Ll}\p{Lm}\p{Lo}\p{M}]*(?i:'s|'t|'re|'ve|'m|'ll|'d)?
//!   3  \p{N}{1,3}
//!   4   ?[^\s\p{L}\p{N}]+[\r\n/]*
//!   5  \s*[\r\n]+
//!   6  \s+(?!\S)
//!   7  \s+
//!   ```
//!
//!   [`pre_split`] scans for these by hand, with the backtracking a regex engine would
//!   do, because the lookahead in 6 is beyond Rust's `regex` crate. The Unicode classes
//!   come from `regex-syntax`, so they are the regex engines' own tables.
//!
//! Control tokens (`<|start|>`, `<|message|>`, ...) never come out of [`Tokenizer::encode`]:
//! text that spells one is ordinary text. The chat format inserts them by id.

use std::collections::HashMap;

use loadngo_inference::bpe::{byte_char, Class, Merges};
use loadngo_weights::gguf::{Gguf, Value};

/// `tokenizer.ggml.token_type` of a control token.
const CONTROL: u64 = 3;

#[derive(Debug, thiserror::Error)]
pub enum TokenizerError {
    #[error("GGUF tokenizer: {0}")]
    Invalid(String),
}

/// The character classes the pattern uses.
struct Classes {
    letter: Class,
    number: Class,
    /// `[\p{Lu}\p{Lt}\p{Lm}\p{Lo}\p{M}]`
    upper: Class,
    /// `[\p{Ll}\p{Lm}\p{Lo}\p{M}]`
    lower: Class,
    space: Class,
}

impl Classes {
    fn new() -> Self {
        Self {
            letter: Class::parse(r"\p{L}"),
            number: Class::parse(r"\p{N}"),
            upper: Class::parse(r"[\p{Lu}\p{Lt}\p{Lm}\p{Lo}\p{M}]"),
            lower: Class::parse(r"[\p{Ll}\p{Lm}\p{Lo}\p{M}]"),
            space: Class::parse(r"\s"),
        }
    }
}

/// Splits `text` as the o200k pattern does (see the module documentation).
fn pre_split<'t>(classes: &Classes, text: &'t str) -> Vec<&'t str> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let n = chars.len();
    let c = |i: usize| chars[i].1;
    let is_newline = |ch: char| ch == '\r' || ch == '\n';
    let space = |i: usize| i < n && classes.space.contains(c(i));
    // `[^\r\n\p{L}\p{N}]`
    let prefix = |i: usize| {
        i < n
            && !is_newline(c(i))
            && !classes.letter.contains(c(i))
            && !classes.number.contains(c(i))
    };
    let upper = |i: usize| i < n && classes.upper.contains(c(i));
    let lower = |i: usize| i < n && classes.lower.contains(c(i));
    // `(?i:'s|'t|'re|'ve|'m|'ll|'d)?` from `i`: where it ends. Case-insensitive matching
    // folds the long s (U+017F) to s as well.
    let contraction = |i: usize| {
        if i >= n || c(i) != '\'' {
            return i;
        }
        let is = |j: usize, letter: char| {
            j < n && {
                let ch = c(j);
                ch.to_ascii_lowercase() == letter || (letter == 's' && ch == '\u{17f}')
            }
        };
        for word in ["s", "t", "re", "ve", "m", "ll", "d"] {
            if word
                .chars()
                .enumerate()
                .all(|(k, letter)| is(i + 1 + k, letter))
            {
                return i + 1 + word.len();
            }
        }
        i
    };
    let run = |mut i: usize, keep: &dyn Fn(usize) -> bool| {
        while i < n && keep(i) {
            i += 1;
        }
        i
    };

    let first = |i: usize| -> Option<usize> {
        for with_prefix in [true, false] {
            let j = if with_prefix {
                if !prefix(i) {
                    continue;
                }
                i + 1
            } else {
                i
            };
            // `U*` takes all it can, then gives back until `L+` can start.
            let most = run(j, &upper);
            if let Some(start) = (j..=most).rev().find(|&m| lower(m)) {
                return Some(contraction(run(start, &lower)));
            }
        }
        None
    };
    let second = |i: usize| -> Option<usize> {
        for with_prefix in [true, false] {
            let j = if with_prefix {
                if !prefix(i) {
                    continue;
                }
                i + 1
            } else {
                i
            };
            let end = run(j, &upper);
            if end > j {
                return Some(contraction(run(end, &lower)));
            }
        }
        None
    };
    let digits = |i: usize| -> Option<usize> {
        let mut end = i;
        while end < n && end - i < 3 && classes.number.contains(c(end)) {
            end += 1;
        }
        (end > i).then_some(end)
    };
    let punctuation = |i: usize| -> Option<usize> {
        let other = |k: usize| {
            !space(k) && !classes.letter.contains(c(k)) && !classes.number.contains(c(k))
        };
        for with_space in [true, false] {
            let j = if with_space {
                if i >= n || c(i) != ' ' {
                    continue;
                }
                i + 1
            } else {
                i
            };
            let end = run(j, &other);
            if end > j {
                return Some(run(end, &|k| matches!(c(k), '\r' | '\n' | '/')));
            }
        }
        None
    };
    let spaces = |i: usize| -> Option<usize> {
        let end = run(i, &space);
        if end == i {
            return None;
        }
        // 5: through the last line break in the run.
        if let Some(last) = (i..end).rev().find(|&k| is_newline(c(k))) {
            return Some(last + 1);
        }
        // 6: a run not followed by anything but its end; else leave its last character.
        if end == n {
            return Some(end);
        }
        if end - i >= 2 {
            return Some(end - 1);
        }
        // 7
        Some(end)
    };

    let mut pieces = Vec::new();
    let mut i = 0;
    while i < n {
        let end = first(i)
            .or_else(|| second(i))
            .or_else(|| digits(i))
            .or_else(|| punctuation(i))
            .or_else(|| spaces(i))
            .expect("every character starts some alternative");
        let from = chars[i].0;
        let to = if end < n { chars[end].0 } else { text.len() };
        pieces.push(&text[from..to]);
        i = end;
    }
    pieces
}

pub struct Tokenizer {
    /// A token's bytes, by id (control tokens: their text).
    bytes: Vec<Vec<u8>>,
    merges: Merges,
    control: HashMap<String, u32>,
    is_control: Vec<bool>,
    classes: Classes,
}

impl Tokenizer {
    /// Reads the vocabulary, token types and merges from a gpt-oss GGUF header.
    pub fn from_gguf(gguf: &Gguf) -> Result<Self, TokenizerError> {
        let invalid = |what: &str| TokenizerError::Invalid(what.to_owned());
        let array = |key: &str| {
            gguf.get(key)
                .and_then(Value::as_array)
                .ok_or_else(|| invalid(&format!("{key} is missing")))
        };
        if gguf.get("tokenizer.ggml.model").and_then(Value::as_str) != Some("gpt2")
            || gguf.get("tokenizer.ggml.pre").and_then(Value::as_str) != Some("gpt-4o")
        {
            return Err(invalid(
                "only tokenizer.ggml.model gpt2 with pre gpt-4o is implemented",
            ));
        }
        let tokens = array("tokenizer.ggml.tokens")?;
        let types = array("tokenizer.ggml.token_type")?;
        if types.len() != tokens.len() {
            return Err(invalid("token_type and tokens differ in length"));
        }
        let mut unbyte = HashMap::new();
        for b in 0..=255_u8 {
            unbyte.insert(byte_char(b), b);
        }
        let mut bytes = Vec::with_capacity(tokens.len());
        let mut ids: HashMap<&str, u32> = HashMap::with_capacity(tokens.len());
        let mut control = HashMap::new();
        let mut is_control = Vec::with_capacity(tokens.len());
        for (id, (token, kind)) in tokens.iter().zip(types).enumerate() {
            let text = token
                .as_str()
                .ok_or_else(|| invalid("a token is not a string"))?;
            let id = u32::try_from(id).map_err(|_| invalid("too many tokens"))?;
            let kind = kind
                .as_u64()
                .ok_or_else(|| invalid("a token type is not an integer"))?;
            if kind == CONTROL {
                control.insert(text.to_owned(), id);
                bytes.push(text.as_bytes().to_vec());
                is_control.push(true);
                continue;
            }
            // Padding and other unusable tokens keep their text but are never produced.
            let decoded: Option<Vec<u8>> =
                text.chars().map(|ch| unbyte.get(&ch).copied()).collect();
            bytes.push(decoded.unwrap_or_else(|| text.as_bytes().to_vec()));
            is_control.push(false);
            ids.entry(text).or_insert(id);
        }
        let mut byte_ids = [0_u32; 256];
        for (b, slot) in byte_ids.iter_mut().enumerate() {
            *slot = *ids
                .get(byte_char(b as u8).to_string().as_str())
                .ok_or_else(|| invalid(&format!("no token for byte {b:#04x}")))?;
        }
        let merge_list = array("tokenizer.ggml.merges")?;
        let mut ranked = Vec::with_capacity(merge_list.len());
        for merge in merge_list {
            let merge = merge
                .as_str()
                .ok_or_else(|| invalid("a merge is not a string"))?;
            let (left, right) = merge
                .split_once(' ')
                .ok_or_else(|| invalid(&format!("merge {merge:?} has no space")))?;
            let id = |text: &str| {
                ids.get(text)
                    .copied()
                    .ok_or_else(|| invalid(&format!("merge {merge:?} names an unknown token")))
            };
            ranked.push((id(left)?, id(right)?, id(&format!("{left}{right}"))?));
        }
        Ok(Self {
            bytes,
            merges: Merges::new(byte_ids, ranked),
            control,
            is_control,
            classes: Classes::new(),
        })
    }

    pub fn vocab_size(&self) -> usize {
        self.bytes.len()
    }

    /// The id of a control token such as `<|start|>`.
    pub fn control(&self, name: &str) -> Option<u32> {
        self.control.get(name).copied()
    }

    pub fn is_control(&self, id: u32) -> bool {
        self.is_control.get(id as usize).copied().unwrap_or(false)
    }

    /// Ordinary text to tokens; never produces a control token.
    pub fn encode(&self, text: &str) -> Vec<u32> {
        let mut out = Vec::new();
        for piece in pre_split(&self.classes, text) {
            self.merges.apply(piece.as_bytes(), &mut out);
        }
        out
    }

    /// The bytes a token stands for (a control token: its text).
    pub fn token_bytes(&self, id: u32) -> &[u8] {
        self.bytes.get(id as usize).map_or(&[], Vec::as_slice)
    }

    /// Tokens back to text; invalid UTF-8 becomes U+FFFD.
    pub fn decode(&self, ids: &[u32]) -> String {
        let bytes: Vec<u8> = ids
            .iter()
            .flat_map(|&id| self.token_bytes(id).to_vec())
            .collect();
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(text: &str) -> Vec<&str> {
        pre_split(&Classes::new(), text)
    }

    #[test]
    fn the_byte_alphabet_is_gpt2s() {
        assert_eq!(byte_char(b'!'), '!');
        assert_eq!(byte_char(b' '), '\u{120}'); // Ġ
        assert_eq!(byte_char(b'\n'), '\u{10a}'); // Ċ
        assert_eq!(byte_char(0), '\u{100}');
        assert_eq!(byte_char(0xad), '\u{143}');
        let all: std::collections::HashSet<char> = (0..=255).map(byte_char).collect();
        assert_eq!(all.len(), 256);
    }

    #[test]
    fn the_pattern_splits_words_numbers_punctuation_and_spaces() {
        assert_eq!(split("Hello world"), ["Hello", " world"]);
        assert_eq!(split("I'm don't THEY'LL"), ["I'm", " don't", " THEY'LL"]);
        assert_eq!(split("12345 67"), ["123", "45", " ", "67"]);
        assert_eq!(split("a  b"), ["a", " ", " b"]);
        assert_eq!(split("x \n\n y"), ["x", " \n\n", " y"]);
        assert_eq!(split("end   "), ["end", "   "]);
        assert_eq!(split("foo(bar)/\n"), ["foo", "(bar", ")/\n"]);
        assert_eq!(
            split("CamelCase HTTPServer"),
            ["Camel", "Case", " HTTPServer"]
        );
        assert_eq!(split("\tindent"), ["\tindent"]);
    }
}
