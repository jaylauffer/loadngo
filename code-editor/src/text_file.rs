//! Turning file bytes into editable text and back.
//!
//! The editor edits with `\n` line breaks. A file written with CRLF is
//! edited as `\n` and written back with CRLF, so saving a file never changes
//! its line endings.

/// How a file ends its lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineEnding {
    Lf,
    CrLf,
}

impl LineEnding {
    pub fn label(self) -> &'static str {
        match self {
            LineEnding::Lf => "LF",
            LineEnding::CrLf => "CRLF",
        }
    }
}

/// A file's text as the editor holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedText {
    /// The contents with `\n` line breaks.
    pub text: String,
    pub line_ending: LineEnding,
}

/// Why a file was not opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotText {
    /// A NUL byte in the first 8 KiB: a binary file.
    Binary,
    /// Not valid UTF-8 (the byte offset of the first invalid sequence).
    NotUtf8 { at: usize },
}

impl std::fmt::Display for NotText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NotText::Binary => f.write_str("a binary file"),
            NotText::NotUtf8 { at } => write!(f, "not UTF-8 text (invalid byte at {at})"),
        }
    }
}

/// How many leading bytes are checked for a NUL.
const BINARY_SNIFF_BYTES: usize = 8 * 1024;

/// Decodes `bytes` for editing. A file whose first line break is CRLF is
/// treated as CRLF throughout; any lone `\n` in it stays `\n` when written
/// back only if the file had no CRLF at all.
pub fn decode(bytes: Vec<u8>) -> Result<DecodedText, NotText> {
    if bytes[..bytes.len().min(BINARY_SNIFF_BYTES)].contains(&0) {
        return Err(NotText::Binary);
    }
    let text = String::from_utf8(bytes).map_err(|error| NotText::NotUtf8 {
        at: error.utf8_error().valid_up_to(),
    })?;
    let line_ending = match text.find('\n') {
        Some(index) if index > 0 && text.as_bytes()[index - 1] == b'\r' => LineEnding::CrLf,
        _ => LineEnding::Lf,
    };
    let text = match line_ending {
        LineEnding::Lf => text,
        LineEnding::CrLf => text.replace("\r\n", "\n"),
    };
    Ok(DecodedText { text, line_ending })
}

/// The bytes to write for `text` with `line_ending`.
pub fn encode(text: &str, line_ending: LineEnding) -> Vec<u8> {
    match line_ending {
        LineEnding::Lf => text.as_bytes().to_vec(),
        LineEnding::CrLf => {
            let mut bytes = Vec::with_capacity(text.len() + text.len() / 32);
            for (index, line) in text.split('\n').enumerate() {
                if index > 0 {
                    bytes.extend_from_slice(b"\r\n");
                }
                bytes.extend_from_slice(line.as_bytes());
            }
            bytes
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{decode, encode, LineEnding, NotText};

    #[test]
    fn lf_text_round_trips_unchanged() {
        let decoded = decode(b"fn main() {}\n".to_vec()).unwrap();
        assert_eq!(decoded.line_ending, LineEnding::Lf);
        assert_eq!(decoded.text, "fn main() {}\n");
        assert_eq!(
            encode(&decoded.text, decoded.line_ending),
            b"fn main() {}\n"
        );
    }

    #[test]
    fn crlf_text_is_edited_as_lf_and_written_back_as_crlf() {
        let original = b"a\r\nb\r\n\r\nc".to_vec();
        let decoded = decode(original.clone()).unwrap();
        assert_eq!(decoded.line_ending, LineEnding::CrLf);
        assert_eq!(decoded.text, "a\nb\n\nc");
        assert_eq!(encode(&decoded.text, decoded.line_ending), original);
    }

    #[test]
    fn binary_and_invalid_utf8_are_refused() {
        assert_eq!(decode(vec![b'P', b'K', 0, 3]), Err(NotText::Binary));
        assert_eq!(
            decode(vec![b'o', b'k', 0xff, b'x']),
            Err(NotText::NotUtf8 { at: 2 })
        );
    }

    #[test]
    fn an_empty_file_is_lf_text() {
        let decoded = decode(Vec::new()).unwrap();
        assert_eq!(decoded.text, "");
        assert_eq!(decoded.line_ending, LineEnding::Lf);
    }
}
