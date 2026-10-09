//! The checkpoint's tokenizer: byte-level BPE as its `tokenizer.json` describes it.
//!
//! - **Added tokens** (`<|endoftext|>`, `<think>`, ...) are found in the raw text first,
//!   longest at each position, and become their ids; text between them is tokenized.
//! - **Normalizer:** NFC.
//! - **Pre-split:** the checkpoint's pattern (Qwen2's; the base model's newer file adds
//!   `\p{M}`, but the decider was trained with, and saved, this one), one alternative
//!   after another, the first that matches at each position:
//!
//!   ```text
//!   1  (?i:'s|'t|'re|'ve|'m|'ll|'d)
//!   2  [^\r\n\p{L}\p{N}]?\p{L}+
//!   3  \p{N}
//!   4   ?[^\s\p{L}\p{N}]+[\r\n]*
//!   5  \s*[\r\n]+
//!   6  \s+(?!\S)
//!   7  \s+
//!   ```
//!
//!   [`pre_split`] scans for these by hand, with the backtracking a regex engine would
//!   do (the lookahead in 6 is beyond Rust's `regex`).
//! - **Model:** BPE over the byte alphabet ([`loadngo_inference::bpe`]); no BOS or other
//!   token is added around the text.
//!
//! Every token carries its character offsets (Unicode scalar values, as Python counts
//! them): a token that holds part of a character's bytes covers the whole character.
//! When NFC changes a stretch of text, offsets inside it count the normalized
//! characters; the prompt normalizes its text before tokenizing, so its offsets are
//! exact.

use std::{collections::HashMap, path::Path};

use icu_normalizer::ComposingNormalizerBorrowed;
use loadngo_inference::bpe::{byte_char, token_bytes, Class, Merges};
use serde_json::Value;

/// The pre-split pattern this tokenizer implements, as `tokenizer.json` spells it.
pub const PATTERN: &str = r"(?i:'s|'t|'re|'ve|'m|'ll|'d)|[^\r\n\p{L}\p{N}]?\p{L}+|\p{N}| ?[^\s\p{L}\p{N}]+[\r\n]*|\s*[\r\n]+|\s+(?!\S)|\s+";

#[derive(Debug, thiserror::Error)]
pub enum TokenizerError {
    #[error("reading {0}: {1}")]
    Read(String, std::io::Error),
    #[error("tokenizer.json: {0}")]
    Invalid(String),
}

/// One token and the characters of the text it covers, `[start, end)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Token {
    pub id: u32,
    pub start: usize,
    pub end: usize,
}

struct Classes {
    letter: Class,
    number: Class,
    space: Class,
}

pub struct Tokenizer {
    /// A token's bytes, by id (an added token: its text).
    bytes: Vec<Vec<u8>>,
    merges: Merges,
    /// Added tokens, longest first.
    added: Vec<(String, u32)>,
    classes: Classes,
}

impl Tokenizer {
    /// Reads a `tokenizer.json`.
    ///
    /// # Errors
    /// When the file cannot be read, or describes anything but this tokenizer.
    pub fn from_file(path: &Path) -> Result<Self, TokenizerError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| TokenizerError::Read(path.display().to_string(), e))?;
        let json: Value =
            serde_json::from_str(&text).map_err(|e| TokenizerError::Invalid(e.to_string()))?;
        Self::from_json(&json)
    }

    /// # Errors
    /// When the description is anything but this tokenizer.
    pub fn from_json(json: &Value) -> Result<Self, TokenizerError> {
        let invalid = |what: String| TokenizerError::Invalid(what);
        if json["normalizer"]["type"] != "NFC" {
            return Err(invalid(format!(
                "normalizer {} is not NFC",
                json["normalizer"]
            )));
        }
        let pattern = json["pre_tokenizer"]["pretokenizers"][0]["pattern"]["Regex"].as_str();
        if pattern != Some(PATTERN) {
            return Err(invalid(format!(
                "pre-split pattern {pattern:?} is not {PATTERN:?}"
            )));
        }
        let model = &json["model"];
        if model["type"] != "BPE" {
            return Err(invalid("model is not BPE".into()));
        }
        let vocab = model["vocab"]
            .as_object()
            .ok_or_else(|| invalid("no vocab".into()))?;
        let mut ids: HashMap<&str, u32> = HashMap::with_capacity(vocab.len());
        let mut bytes: Vec<Vec<u8>> = Vec::new();
        for (text, id) in vocab {
            let id = id
                .as_u64()
                .and_then(|id| u32::try_from(id).ok())
                .ok_or_else(|| invalid(format!("token {text:?} has no id")))?;
            let slot = id as usize;
            if bytes.len() <= slot {
                bytes.resize(slot + 1, Vec::new());
            }
            bytes[slot] = token_bytes(text).unwrap_or_else(|| text.as_bytes().to_vec());
            ids.insert(text.as_str(), id);
        }
        let mut byte_ids = [0_u32; 256];
        for (b, slot) in (0..=255_u8).zip(byte_ids.iter_mut()) {
            *slot = *ids
                .get(byte_char(b).to_string().as_str())
                .ok_or_else(|| invalid(format!("no token for byte {b:#04x}")))?;
        }
        let merge_list = model["merges"]
            .as_array()
            .ok_or_else(|| invalid("no merges".into()))?;
        let mut ranked = Vec::with_capacity(merge_list.len());
        for merge in merge_list {
            let (left, right) = match merge {
                Value::String(s) => s
                    .split_once(' ')
                    .ok_or_else(|| invalid(format!("merge {s:?} has no space")))?,
                Value::Array(pair) if pair.len() == 2 => (
                    pair[0].as_str().unwrap_or_default(),
                    pair[1].as_str().unwrap_or_default(),
                ),
                other => return Err(invalid(format!("merge {other} is not a pair"))),
            };
            let id = |text: &str| {
                ids.get(text).copied().ok_or_else(|| {
                    invalid(format!("merge {left:?} {right:?} names an unknown token"))
                })
            };
            ranked.push((id(left)?, id(right)?, id(&format!("{left}{right}"))?));
        }
        let mut added: Vec<(String, u32)> = json["added_tokens"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|t| {
                Some((
                    t["content"].as_str()?.to_owned(),
                    u32::try_from(t["id"].as_u64()?).ok()?,
                ))
            })
            .collect();
        for (text, id) in &added {
            let slot = *id as usize;
            if bytes.len() <= slot {
                bytes.resize(slot + 1, Vec::new());
            }
            bytes[slot] = text.as_bytes().to_vec();
        }
        added.sort_by_key(|(text, _)| std::cmp::Reverse(text.len()));
        Ok(Self {
            bytes,
            merges: Merges::new(byte_ids, ranked),
            added,
            classes: Classes {
                letter: Class::parse(r"\p{L}"),
                number: Class::parse(r"\p{N}"),
                space: Class::parse(r"\s"),
            },
        })
    }

    pub fn vocab_size(&self) -> usize {
        self.bytes.len()
    }

    /// The tokens of `text`, with their character offsets.
    pub fn encode(&self, text: &str) -> Vec<Token> {
        let mut out = Vec::new();
        let mut plain_start = 0; // byte index
        let mut chars_before = 0; // characters before `plain_start`
        let mut at = 0;
        while at < text.len() {
            let found = self
                .added
                .iter()
                .find(|(content, _)| text[at..].starts_with(content.as_str()));
            if let Some((content, id)) = found {
                let plain = &text[plain_start..at];
                self.encode_plain(plain, chars_before, &mut out);
                chars_before += plain.chars().count();
                let n = content.chars().count();
                out.push(Token {
                    id: *id,
                    start: chars_before,
                    end: chars_before + n,
                });
                chars_before += n;
                at += content.len();
                plain_start = at;
            } else {
                at += text[at..].chars().next().map_or(1, char::len_utf8);
            }
        }
        self.encode_plain(&text[plain_start..], chars_before, &mut out);
        out
    }

    /// Token ids only.
    pub fn ids(&self, text: &str) -> Vec<u32> {
        self.encode(text).into_iter().map(|t| t.id).collect()
    }

    fn encode_plain(&self, text: &str, base: usize, out: &mut Vec<Token>) {
        if text.is_empty() {
            return;
        }
        let normalized = ComposingNormalizerBorrowed::new_nfc().normalize(text);
        let mut ids = Vec::new();
        for (piece, first_char) in pre_split(&self.classes, &normalized) {
            ids.clear();
            self.merges.apply(piece.as_bytes(), &mut ids);
            // The character each byte of the piece belongs to.
            let mut char_of_byte = Vec::with_capacity(piece.len());
            for (k, c) in piece.chars().enumerate() {
                char_of_byte.extend(std::iter::repeat_n(k, c.len_utf8()));
            }
            let mut byte = 0;
            for &id in &ids {
                let len = self.bytes[id as usize].len().max(1);
                let first = char_of_byte[byte];
                let last = char_of_byte[(byte + len - 1).min(piece.len() - 1)];
                out.push(Token {
                    id,
                    start: base + first_char + first,
                    end: base + first_char + last + 1,
                });
                byte += len;
            }
        }
    }

    /// The bytes a token stands for (an added token: its text).
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

/// Splits `text` as the module's pattern does: each piece with the index of its first
/// character.
fn pre_split<'t>(classes: &Classes, text: &'t str) -> Vec<(&'t str, usize)> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let n = chars.len();
    let c = |i: usize| chars[i].1;
    let newline = |i: usize| i < n && matches!(c(i), '\r' | '\n');
    let space = |i: usize| i < n && classes.space.contains(c(i));
    let letter = |i: usize| i < n && classes.letter.contains(c(i));
    let number = |i: usize| i < n && classes.number.contains(c(i));
    // `[^\s\p{L}\p{N}]`
    let punct = |i: usize| i < n && !space(i) && !letter(i) && !number(i);
    // `[^\r\n\p{L}\p{N}]`
    let prefix = |i: usize| i < n && !newline(i) && !letter(i) && !number(i);
    let run = |mut i: usize, keep: &dyn Fn(usize) -> bool| {
        while keep(i) {
            i += 1;
        }
        i
    };
    // `(?i:'s|'t|'re|'ve|'m|'ll|'d)` at `i`: where it ends. Case-insensitive matching folds
    // the long s (U+017F) to s as well.
    let contraction = |i: usize| -> Option<usize> {
        if i >= n || c(i) != '\'' {
            return None;
        }
        let is = |j: usize, letter: char| {
            j < n && {
                let ch = c(j);
                ch.to_ascii_lowercase() == letter || (letter == 's' && ch == '\u{17f}')
            }
        };
        ["s", "t", "re", "ve", "m", "ll", "d"]
            .iter()
            .find(|word| word.chars().enumerate().all(|(k, l)| is(i + 1 + k, l)))
            .map(|word| i + 1 + word.len())
    };
    let byte_at = |i: usize| if i < n { chars[i].0 } else { text.len() };
    let mut pieces = Vec::new();
    let mut i = 0;
    while i < n {
        let end = if let Some(end) = contraction(i) {
            end
        } else if letter(i) {
            run(i, &letter)
        } else if prefix(i) && letter(i + 1) {
            run(i + 1, &letter)
        } else if number(i) {
            i + 1
        } else if c(i) == ' ' && punct(i + 1) {
            run(run(i + 1, &punct), &newline)
        } else if punct(i) {
            run(run(i, &punct), &newline)
        } else {
            // Whitespace: alternatives 5, 6 and 7.
            let end = run(i, &space);
            if let Some(last) = (i..end).rev().find(|&k| newline(k)) {
                last + 1
            } else if end == n || end - i == 1 {
                end
            } else {
                end - 1
            }
        };
        pieces.push((&text[byte_at(i)..byte_at(end)], i));
        i = end;
    }
    pieces
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(text: &str) -> Vec<&str> {
        let classes = Classes {
            letter: Class::parse(r"\p{L}"),
            number: Class::parse(r"\p{N}"),
            space: Class::parse(r"\s"),
        };
        pre_split(&classes, text)
            .into_iter()
            .map(|(p, _)| p)
            .collect()
    }

    #[test]
    fn the_pattern_splits_as_qwen2s_regex_does() {
        assert_eq!(split("Hello world"), ["Hello", " world"]);
        assert_eq!(
            split("I'm don't THEY'LL"),
            ["I", "'m", " don", "'t", " THEY", "'LL"]
        );
        assert_eq!(split("12345 6"), ["1", "2", "3", "4", "5", " ", "6"]);
        assert_eq!(split("a  b"), ["a", " ", " b"]);
        assert_eq!(split("x \n\n y"), ["x", " \n\n", " y"]);
        assert_eq!(split("end   "), ["end", "   "]);
        assert_eq!(split("foo(bar)/\n"), ["foo", "(bar", ")/\n"]);
        assert_eq!(split("\tindent"), ["\tindent"]);
        assert_eq!(split("a\n"), ["a", "\n"]);
        assert_eq!(split(" \u{301}x"), [" \u{301}", "x"]);
    }
}
