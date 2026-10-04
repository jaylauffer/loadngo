//! Rust source read as text, for tools that let a model edit it: whether its brackets
//! balance, and an outline of its items. Neither parses Rust; both read its tokens
//! closely enough (strings, raw strings, characters, lifetimes, nested comments) that
//! brackets inside them are not counted.

use std::fmt::Write as _;

/// Where a file's brackets first fail to balance, as a sentence naming lines; `None`
/// when they balance.
pub fn bracket_problem(text: &str) -> Option<String> {
    let b = text.as_bytes();
    let mut open: Vec<(u8, usize)> = Vec::new();
    let mut line = 1;
    let mut i = 0;
    let lines_in = |from: usize, to: usize| b[from..to].iter().filter(|&&c| c == b'\n').count();
    while i < b.len() {
        let c = b[i];
        match c {
            b'\n' => line += 1,
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                let start = i;
                let mut depth = 0;
                while i < b.len() {
                    if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
                        depth += 1;
                        i += 2;
                    } else if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
                        depth -= 1;
                        i += 2;
                        if depth == 0 {
                            break;
                        }
                    } else {
                        i += 1;
                    }
                }
                line += lines_in(start, i.min(b.len()));
                continue;
            }
            b'r' | b'b' | b'c' if raw_string_start(b, i).is_some() => {
                let (hashes, body) = raw_string_start(b, i).unwrap_or((0, i));
                let start = i;
                i = body;
                loop {
                    if i >= b.len() {
                        break;
                    }
                    if b[i] == b'"'
                        && b[i + 1..]
                            .iter()
                            .take(hashes)
                            .filter(|&&h| h == b'#')
                            .count()
                            == hashes
                    {
                        i += 1 + hashes;
                        break;
                    }
                    i += 1;
                }
                line += lines_in(start, i.min(b.len()));
                continue;
            }
            b'"' => {
                let start = i;
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
                i += 1;
                line += lines_in(start, i.min(b.len()));
                continue;
            }
            b'\'' => {
                // A character ('x', '\n', '\u{..}', or one multi-byte character) or a
                // lifetime or label ('a, 'static), which has no closing quote.
                if b.get(i + 1) == Some(&b'\\') {
                    i += 3;
                    while i < b.len() && b[i] != b'\'' && b[i] != b'\n' {
                        i += 1;
                    }
                    i += 1;
                    continue;
                }
                let width = text[i + 1..].chars().next().map_or(1, char::len_utf8);
                if b.get(i + 1 + width) == Some(&b'\'') {
                    i += width + 2;
                    continue;
                }
            }
            b'(' | b'[' | b'{' => open.push((c, line)),
            b')' | b']' | b'}' => {
                let want = match c {
                    b')' => b'(',
                    b']' => b'[',
                    _ => b'{',
                };
                match open.pop() {
                    Some((o, _)) if o == want => {}
                    Some((o, at)) => {
                        return Some(format!(
                            "the `{}` at line {line} does not close the `{}` opened at line {at}",
                            c as char, o as char
                        ))
                    }
                    None => {
                        return Some(format!(
                            "the `{}` at line {line} closes nothing (one closing bracket too many \
                             somewhere above it)",
                            c as char
                        ))
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }
    open.last().map(|(o, at)| {
        format!(
            "the `{}` opened at line {at} is never closed ({} unclosed in all)",
            *o as char,
            open.len()
        )
    })
}

/// `(hashes, index after the opening quote)` when a raw string (`r"`, `r#"`, `br#"`,
/// `cr"`) starts at `i` and is not the end of an identifier.
fn raw_string_start(b: &[u8], i: usize) -> Option<(usize, usize)> {
    if i > 0 && (b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_') {
        return None;
    }
    let mut j = i;
    if b[j] == b'b' || b[j] == b'c' {
        j += 1;
    }
    if b.get(j) != Some(&b'r') {
        return None;
    }
    j += 1;
    let mut hashes = 0;
    while b.get(j) == Some(&b'#') {
        hashes += 1;
        j += 1;
    }
    (b.get(j) == Some(&b'"')).then_some((hashes, j + 1))
}

/// Item lines, with their numbers and indentation: functions, `impl` blocks, modules,
/// types, traits and `#[cfg(test)]`. At most `max` lines; the most indented go first
/// when there are more.
pub fn outline(text: &str, max: usize) -> String {
    const STARTS: [&str; 8] = [
        "fn ",
        "impl",
        "mod ",
        "struct ",
        "enum ",
        "trait ",
        "type ",
        "macro_rules!",
    ];
    let mut items: Vec<(usize, usize, &str)> = Vec::new();
    for (n, raw) in text.lines().enumerate() {
        let trimmed = raw.trim_start();
        let indent = raw.len() - trimmed.len();
        let mut rest = trimmed;
        for prefix in [
            "pub(crate) ",
            "pub(super) ",
            "pub ",
            "unsafe ",
            "async ",
            "const ",
            "extern \"C\" ",
        ] {
            rest = rest.strip_prefix(prefix).unwrap_or(rest);
        }
        let item = STARTS.iter().any(|s| rest.starts_with(s)) && !rest.starts_with("impl Fn");
        if item || trimmed.starts_with("#[cfg(test)]") {
            items.push((n + 1, indent, raw.trim_end()));
        }
    }
    let mut depth = usize::MAX;
    while items.len() > max {
        depth = items
            .iter()
            .map(|i| i.1)
            .filter(|&d| d < depth)
            .max()
            .unwrap_or(0);
        if depth == 0 {
            items.truncate(max);
            break;
        }
        items.retain(|i| i.1 < depth);
    }
    let mut out = String::new();
    for (n, _, line) in items {
        let shown: String = line.chars().take(110).collect();
        let _ = writeln!(out, "{n:>6}|{shown}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brackets_inside_strings_characters_lifetimes_and_comments_are_not_counted() {
        let ok = r##"
fn f<'a>(x: &'a str) -> char {
    let _ = "}{ \" (";
    let _ = r#"}"# ;
    let _ = br"{";
    let _ = '{';
    let _ = '\'';
    let _ = 'é';
    // }
    /* { /* nested } */ ) */
    'outer: loop { break 'outer; }
    '}'
}
"##;
        assert_eq!(bracket_problem(ok), None);
    }

    #[test]
    fn unbalanced_brackets_are_named_by_line() {
        let extra = "impl A {\n    fn f() {}\n}\n}\n";
        assert!(bracket_problem(extra)
            .unwrap()
            .contains("line 4 closes nothing"));
        let unclosed = "impl A {\n    fn f() {\n}\n";
        assert!(bracket_problem(unclosed)
            .unwrap()
            .contains("opened at line 1 is never closed"));
        let crossed = "fn f() {\n    g(1];\n}\n";
        assert!(bracket_problem(crossed)
            .unwrap()
            .contains("`]` at line 2 does not close the `(`"));
    }

    #[test]
    fn the_outline_lists_items_and_the_test_module() {
        let text = "pub enum Key {\n    A,\n}\n\nimpl Line {\n    pub fn apply(&mut self) {\n        let f = |x| x;\n    }\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn works() {}\n}\n";
        let o = outline(text, 50);
        assert_eq!(
            o,
            "     1|pub enum Key {\n     5|impl Line {\n     6|    pub fn apply(&mut self) {\n    11|#[cfg(test)]\n    12|mod tests {\n    14|    fn works() {}\n"
        );
        // Too many: the deepest go first.
        assert_eq!(outline(text, 4).lines().count(), 4);
        assert!(!outline(text, 4).contains("apply"));
    }
}

#[cfg(test)]
mod workspace_sources {
    /// Every Rust file below the directories in `RUST_TEXT_SCAN` balances (a check that
    /// the bracket reader takes real code as valid; unset, it reads this crate).
    #[test]
    fn real_sources_balance() {
        let roots =
            std::env::var("RUST_TEXT_SCAN").unwrap_or_else(|_| env!("CARGO_MANIFEST_DIR").into());
        let mut stack: Vec<std::path::PathBuf> = std::env::split_paths(&roots).collect();
        let (mut files, mut bad) = (0, Vec::new());
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let name = entry.file_name();
                if path.is_dir() && !name.to_string_lossy().starts_with('.') && name != "target" {
                    stack.push(path);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let Ok(text) = std::fs::read_to_string(&path) else {
                        continue;
                    };
                    files += 1;
                    if let Some(problem) = super::bracket_problem(&text) {
                        bad.push(format!("{}: {problem}", path.display()));
                    }
                }
            }
        }
        assert!(files > 0);
        assert!(
            bad.is_empty(),
            "{files} files, {} unbalanced:\n{}",
            bad.len(),
            bad.join("\n")
        );
    }
}
