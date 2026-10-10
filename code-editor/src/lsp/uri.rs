//! `file://` URIs for paths, as language servers name documents.

use std::path::{Path, PathBuf};

/// Characters a path segment keeps as they are in a URI.
fn keeps(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/')
}

/// `file:///abs/path`, percent-encoding everything else.
pub fn from_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    let text = text.replace('\\', "/");
    let mut uri = String::from("file://");
    if !text.starts_with('/') {
        // A Windows drive path: file:///C:/...
        uri.push('/');
    }
    let drive_colon = text.as_bytes().get(1) == Some(&b':')
        && text.as_bytes().first().is_some_and(u8::is_ascii_alphabetic);
    for (index, byte) in text.bytes().enumerate() {
        if keeps(byte) || (byte == b':' && index == 1 && drive_colon) {
            uri.push(byte as char);
        } else {
            uri.push_str(&format!("%{byte:02X}"));
        }
    }
    uri
}

/// The path a `file://` URI names, if it is one.
pub fn to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let bytes = rest.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
            decoded.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            decoded.push(bytes[i]);
            i += 1;
        }
    }
    let text = String::from_utf8(decoded).ok()?;
    // file:///C:/x on Windows names C:/x.
    let text = match text.as_bytes() {
        [b'/', drive, b':', ..] if drive.is_ascii_alphabetic() && cfg!(windows) => {
            text[1..].to_string()
        }
        _ => text,
    };
    Some(PathBuf::from(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_with_spaces_and_unicode_round_trip() {
        let path = Path::new("/Volumes/Zhoenus II/pudding/ré sumé.rs");
        let uri = from_path(path);
        assert_eq!(
            uri,
            "file:///Volumes/Zhoenus%20II/pudding/r%C3%A9%20sum%C3%A9.rs"
        );
        assert_eq!(to_path(&uri).as_deref(), Some(path));
    }

    #[test]
    fn plain_paths_stay_readable() {
        assert_eq!(
            from_path(Path::new("/Users/jay/pudding/loadngo/src/lib.rs")),
            "file:///Users/jay/pudding/loadngo/src/lib.rs"
        );
        assert_eq!(to_path("https://example.com/x"), None);
    }
}
