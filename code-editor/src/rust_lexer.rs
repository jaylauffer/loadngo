//! A Rust lexer for highlighting, one line at a time.
//!
//! It is not a compiler front end: it only has to tell keywords, names,
//! literals, comments and attributes apart well enough to color them, and
//! never fail. What carries from one line to the next (an open block
//! comment, a string that continues) is a small [`LexState`], so a change
//! is re-lexed from its own line on.

/// What a token is, for its color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Keyword,
    /// A type: a capitalized name or a primitive type.
    Type,
    /// A function being defined or called.
    Function,
    /// A macro name with its `!`.
    Macro,
    Lifetime,
    String,
    Char,
    Number,
    /// `true`, `false` and SCREAMING_CASE names.
    Constant,
    Comment,
    DocComment,
    Attribute,
    Punctuation,
    Ident,
}

/// A token: characters `start..end` of the line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    pub start: usize,
    pub end: usize,
    pub kind: TokenKind,
}

/// What is still open at the end of a line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LexState {
    #[default]
    Normal,
    BlockComment {
        depth: u32,
        doc: bool,
    },
    /// Inside `"..."` (or `b"..."`, `c"..."`).
    Str,
    /// Inside `r#"..."#` with this many `#`.
    RawStr {
        hashes: u32,
    },
}

const KEYWORDS: &[&str] = &[
    "as",
    "async",
    "await",
    "break",
    "const",
    "continue",
    "crate",
    "dyn",
    "else",
    "enum",
    "extern",
    "fn",
    "for",
    "if",
    "impl",
    "in",
    "let",
    "loop",
    "match",
    "mod",
    "move",
    "mut",
    "pub",
    "ref",
    "return",
    "self",
    "Self",
    "static",
    "struct",
    "super",
    "trait",
    "type",
    "unsafe",
    "use",
    "where",
    "while",
    "yield",
    "union",
    "macro_rules",
    "abstract",
    "become",
    "box",
    "do",
    "final",
    "macro",
    "override",
    "priv",
    "try",
    "typeof",
    "unsized",
    "virtual",
    "gen",
];

const PRIMITIVE_TYPES: &[&str] = &[
    "bool", "char", "str", "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64",
    "i128", "isize", "f32", "f64",
];

fn is_ident_start(ch: char) -> bool {
    ch.is_alphabetic() || ch == '_'
}

fn is_ident_continue(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

/// Lexes one line (without its line break), starting in `state`. Returns
/// its tokens (whitespace is not a token) and the state at its end.
pub fn lex_line(line: &str, state: LexState) -> (Vec<Token>, LexState) {
    let c: Vec<char> = line.chars().collect();
    let n = c.len();
    let mut tokens = Vec::new();
    let mut push = |start: usize, end: usize, kind: TokenKind| {
        if end > start {
            tokens.push(Token { start, end, kind });
        }
    };
    let mut i = 0;

    match state {
        LexState::Normal => {}
        LexState::BlockComment { depth, doc } => {
            let (end, depth) = scan_block_comment(&c, 0, depth);
            push(0, end, comment_kind(doc));
            if depth > 0 {
                return (tokens, LexState::BlockComment { depth, doc });
            }
            i = end;
        }
        LexState::Str => {
            let (end, closed) = scan_string(&c, 0);
            push(0, end, TokenKind::String);
            if !closed {
                return (tokens, LexState::Str);
            }
            i = end;
        }
        LexState::RawStr { hashes } => {
            let (end, closed) = scan_raw_string(&c, 0, hashes);
            push(0, end, TokenKind::String);
            if !closed {
                return (tokens, LexState::RawStr { hashes });
            }
            i = end;
        }
    }

    let at = |index: usize| c.get(index).copied();
    let mut previous_word: Option<String> = None;
    while i < n {
        let ch = c[i];
        if ch.is_whitespace() {
            i += 1;
            continue;
        }
        // Comments.
        if ch == '/' && at(i + 1) == Some('/') {
            let doc = (at(i + 2) == Some('/') && at(i + 3) != Some('/')) || at(i + 2) == Some('!');
            push(i, n, comment_kind(doc));
            return (tokens, LexState::Normal);
        }
        if ch == '/' && at(i + 1) == Some('*') {
            let doc = (at(i + 2) == Some('*') && !matches!(at(i + 3), Some('*') | Some('/')))
                || at(i + 2) == Some('!');
            let (end, depth) = scan_block_comment(&c, i + 2, 1);
            push(i, end, comment_kind(doc));
            if depth > 0 {
                return (tokens, LexState::BlockComment { depth, doc });
            }
            i = end;
            continue;
        }
        // Attributes: #[...] and #![...].
        if ch == '#'
            && (at(i + 1) == Some('[') || (at(i + 1) == Some('!') && at(i + 2) == Some('[')))
        {
            let end = scan_attribute(&c, i);
            push(i, end, TokenKind::Attribute);
            i = end;
            continue;
        }
        // Raw strings: r"...", r#"..."#, br"...", cr"...".
        let raw_prefix = match (ch, at(i + 1)) {
            ('r', _) => Some(i + 1),
            ('b' | 'c', Some('r')) => Some(i + 2),
            _ => None,
        };
        if let Some(after_r) = raw_prefix {
            let mut j = after_r;
            while at(j) == Some('#') {
                j += 1;
            }
            if at(j) == Some('"') {
                let hashes = (j - after_r) as u32;
                let (end, closed) = scan_raw_string(&c, j + 1, hashes);
                push(i, end, TokenKind::String);
                if !closed {
                    return (tokens, LexState::RawStr { hashes });
                }
                i = end;
                continue;
            }
        }
        // Strings and byte/C strings.
        if ch == '"' || (matches!(ch, 'b' | 'c') && at(i + 1) == Some('"')) {
            let open = if ch == '"' { i } else { i + 1 };
            let (end, closed) = scan_string(&c, open + 1);
            push(i, end, TokenKind::String);
            if !closed {
                return (tokens, LexState::Str);
            }
            i = end;
            continue;
        }
        // Chars, byte chars and lifetimes.
        if ch == '\'' || (ch == 'b' && at(i + 1) == Some('\'')) {
            let quote = if ch == '\'' { i } else { i + 1 };
            if let Some(end) = scan_char(&c, quote) {
                push(i, end, TokenKind::Char);
                i = end;
                continue;
            }
            if ch == '\'' && at(i + 1).is_some_and(is_ident_start) {
                let mut j = i + 1;
                while at(j).is_some_and(is_ident_continue) {
                    j += 1;
                }
                push(i, j, TokenKind::Lifetime);
                i = j;
                continue;
            }
        }
        if ch.is_ascii_digit() {
            let end = scan_number(&c, i);
            push(i, end, TokenKind::Number);
            i = end;
            continue;
        }
        if is_ident_start(ch) {
            // r#raw identifiers keep their prefix in the token.
            let word_start =
                if ch == 'r' && at(i + 1) == Some('#') && at(i + 2).is_some_and(is_ident_start) {
                    i + 2
                } else {
                    i
                };
            let mut j = word_start;
            while at(j).is_some_and(is_ident_continue) {
                j += 1;
            }
            let word: String = c[word_start..j].iter().collect();
            let mut next = j;
            while at(next).is_some_and(|ch| ch == ' ') {
                next += 1;
            }
            let kind = if word_start == i && KEYWORDS.contains(&word.as_str()) {
                TokenKind::Keyword
            } else if word == "true" || word == "false" {
                TokenKind::Constant
            } else if at(j) == Some('!') && at(j + 1) != Some('=') {
                j += 1;
                TokenKind::Macro
            } else if previous_word.as_deref() == Some("fn") || at(next) == Some('(') {
                TokenKind::Function
            } else if PRIMITIVE_TYPES.contains(&word.as_str()) {
                TokenKind::Type
            } else if word.len() > 1
                && word
                    .chars()
                    .all(|ch| ch.is_uppercase() || ch.is_ascii_digit() || ch == '_')
                && word.chars().any(char::is_uppercase)
            {
                TokenKind::Constant
            } else if word.chars().next().is_some_and(char::is_uppercase) {
                TokenKind::Type
            } else {
                TokenKind::Ident
            };
            push(i, j, kind);
            previous_word = Some(word);
            i = j;
            continue;
        }
        // Punctuation: a run of it, up to anything that starts another token.
        let start = i;
        i += 1;
        while i < n {
            let ch = c[i];
            let starts_other = ch.is_whitespace()
                || is_ident_continue(ch)
                || ch == '"'
                || ch == '\''
                || (ch == '/' && matches!(at(i + 1), Some('/') | Some('*')))
                || (ch == '#' && matches!(at(i + 1), Some('[') | Some('!')));
            if starts_other {
                break;
            }
            i += 1;
        }
        push(start, i, TokenKind::Punctuation);
        previous_word = None;
    }
    (tokens, LexState::Normal)
}

fn comment_kind(doc: bool) -> TokenKind {
    if doc {
        TokenKind::DocComment
    } else {
        TokenKind::Comment
    }
}

/// From inside a block comment at nesting `depth`: where the comment ends
/// on this line (or the line's end) and the depth still open there.
fn scan_block_comment(c: &[char], mut i: usize, mut depth: u32) -> (usize, u32) {
    while i < c.len() {
        if c[i] == '*' && c.get(i + 1) == Some(&'/') {
            depth -= 1;
            i += 2;
            if depth == 0 {
                return (i, 0);
            }
        } else if c[i] == '/' && c.get(i + 1) == Some(&'*') {
            depth += 1;
            i += 2;
        } else {
            i += 1;
        }
    }
    (c.len(), depth)
}

/// From just inside a `"`: the end of the string (past its closing quote)
/// and whether it closed on this line.
fn scan_string(c: &[char], mut i: usize) -> (usize, bool) {
    while i < c.len() {
        match c[i] {
            '\\' => i += 2,
            '"' => return (i + 1, true),
            _ => i += 1,
        }
    }
    (c.len(), false)
}

/// From just inside a raw string's quote: its end and whether it closed.
fn scan_raw_string(c: &[char], mut i: usize, hashes: u32) -> (usize, bool) {
    let hashes = hashes as usize;
    while i < c.len() {
        if c[i] == '"'
            && c[i + 1..]
                .iter()
                .take(hashes)
                .filter(|&&ch| ch == '#')
                .count()
                == hashes
            && c.len() >= i + 1 + hashes
        {
            return (i + 1 + hashes, true);
        }
        i += 1;
    }
    (c.len(), false)
}

/// A char literal starting at the quote `quote`, if there is one: `'a'`,
/// `'\n'`, `'\u{1F600}'`, `'\''`.
fn scan_char(c: &[char], quote: usize) -> Option<usize> {
    match c.get(quote + 1)? {
        '\\' => {
            let mut j = quote + 2;
            // The escaped character, then up to the closing quote.
            j += 1;
            while j < c.len() && c[j] != '\'' && j - quote < 12 {
                j += 1;
            }
            (c.get(j) == Some(&'\'')).then_some(j + 1)
        }
        '\'' => None,
        _ => (c.get(quote + 2) == Some(&'\'')).then_some(quote + 3),
    }
}

/// A number from `start`: digits, `_`, a base prefix, a fraction (not a
/// range's `..`), an exponent and a type suffix.
fn scan_number(c: &[char], start: usize) -> usize {
    let hex = c.get(start) == Some(&'0') && matches!(c.get(start + 1), Some('x' | 'X'));
    let mut i = start;
    while i < c.len() {
        let ch = c[i];
        let digit_or_suffix = ch.is_alphanumeric() || ch == '_';
        let fraction = ch == '.'
            && c.get(i + 1).is_some_and(char::is_ascii_digit)
            && !c[start..i].contains(&'.');
        let exponent_sign = matches!(ch, '+' | '-')
            && !hex
            && matches!(c.get(i - 1), Some('e' | 'E'))
            && c.get(i + 1).is_some_and(char::is_ascii_digit);
        if !(digit_or_suffix || fraction || exponent_sign) {
            break;
        }
        i += 1;
    }
    i
}

/// An attribute from its `#`: to the matching `]`, or the line's end.
fn scan_attribute(c: &[char], start: usize) -> usize {
    let mut depth = 0u32;
    let mut i = start;
    while i < c.len() {
        match c[i] {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            '"' => {
                let (end, closed) = scan_string(c, i + 1);
                if !closed {
                    return c.len();
                }
                i = end;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    c.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use TokenKind::*;

    /// (text, kind) for each token of `line`.
    fn tokens(line: &str) -> Vec<(std::string::String, TokenKind)> {
        let chars: Vec<char> = line.chars().collect();
        lex_line(line, LexState::Normal)
            .0
            .into_iter()
            .map(|token| (chars[token.start..token.end].iter().collect(), token.kind))
            .collect()
    }

    fn t(text: &str, kind: TokenKind) -> (std::string::String, TokenKind) {
        (text.to_string(), kind)
    }

    #[test]
    fn a_function_signature() {
        assert_eq!(
            tokens("pub fn lex_line(line: &str, state: LexState) -> Vec<Token> {"),
            vec![
                t("pub", Keyword),
                t("fn", Keyword),
                t("lex_line", Function),
                t("(", Punctuation),
                t("line", Ident),
                t(":", Punctuation),
                t("&", Punctuation),
                t("str", Type),
                t(",", Punctuation),
                t("state", Ident),
                t(":", Punctuation),
                t("LexState", Type),
                t(")", Punctuation),
                t("->", Punctuation),
                t("Vec", Type),
                t("<", Punctuation),
                t("Token", Type),
                t(">", Punctuation),
                t("{", Punctuation),
            ]
        );
    }

    #[test]
    fn macros_calls_constants_and_numbers() {
        assert_eq!(
            tokens(r#"println!("{x}", x = MAX_ROWS.min(0x1F) + 1_000.5e-3f64);"#),
            vec![
                t("println!", Macro),
                t("(", Punctuation),
                t(r#""{x}""#, String),
                t(",", Punctuation),
                t("x", Ident),
                t("=", Punctuation),
                t("MAX_ROWS", Constant),
                t(".", Punctuation),
                t("min", Function),
                t("(", Punctuation),
                t("0x1F", Number),
                t(")", Punctuation),
                t("+", Punctuation),
                t("1_000.5e-3f64", Number),
                t(");", Punctuation),
            ]
        );
        assert_eq!(
            tokens("a != b && true"),
            vec![
                t("a", Ident),
                t("!=", Punctuation),
                t("b", Ident),
                t("&&", Punctuation),
                t("true", Constant)
            ]
        );
    }

    #[test]
    fn ranges_and_tuple_fields_are_not_fractions() {
        assert_eq!(
            tokens("0..10"),
            vec![t("0", Number), t("..", Punctuation), t("10", Number)]
        );
        assert_eq!(
            tokens("pair.0"),
            vec![t("pair", Ident), t(".", Punctuation), t("0", Number)]
        );
    }

    #[test]
    fn chars_lifetimes_and_escapes() {
        assert_eq!(
            tokens(r"fn f<'a>(x: &'a str) -> char { '\n' }"),
            vec![
                t("fn", Keyword),
                t("f", Function),
                t("<", Punctuation),
                t("'a", Lifetime),
                t(">(", Punctuation),
                t("x", Ident),
                t(":", Punctuation),
                t("&", Punctuation),
                t("'a", Lifetime),
                t("str", Type),
                t(")", Punctuation),
                t("->", Punctuation),
                t("char", Type),
                t("{", Punctuation),
                t(r"'\n'", Char),
                t("}", Punctuation),
            ]
        );
        assert_eq!(tokens(r"b'x'"), vec![t("b'x'", Char)]);
        assert_eq!(tokens(r"'\''"), vec![t(r"'\''", Char)]);
        assert_eq!(tokens(r"'\u{1F600}'"), vec![t(r"'\u{1F600}'", Char)]);
        assert_eq!(tokens("'static"), vec![t("'static", Lifetime)]);
    }

    #[test]
    fn comments_and_doc_comments() {
        assert_eq!(
            tokens("let x = 1; // note"),
            vec![
                t("let", Keyword),
                t("x", Ident),
                t("=", Punctuation),
                t("1", Number),
                t(";", Punctuation),
                t("// note", Comment)
            ]
        );
        assert_eq!(tokens("/// docs"), vec![t("/// docs", DocComment)]);
        assert_eq!(
            tokens("//! crate docs"),
            vec![t("//! crate docs", DocComment)]
        );
        assert_eq!(tokens("//// rule"), vec![t("//// rule", Comment)]);
        assert_eq!(
            tokens("a /* b */ c"),
            vec![t("a", Ident), t("/* b */", Comment), t("c", Ident)]
        );
    }

    #[test]
    fn block_comments_nest_and_carry_across_lines() {
        let (first, state) = lex_line("x /* one /* two */", LexState::Normal);
        assert_eq!(first.last().unwrap().kind, Comment);
        assert_eq!(
            state,
            LexState::BlockComment {
                depth: 1,
                doc: false
            }
        );
        let (second, state) = lex_line("still */ y", state);
        assert_eq!(state, LexState::Normal);
        assert_eq!(
            second[0],
            Token {
                start: 0,
                end: 8,
                kind: Comment
            }
        );
        assert_eq!(second[1].kind, Ident);
    }

    #[test]
    fn strings_carry_across_lines() {
        let (_, state) = lex_line(r#"let s = "first \"line"#, LexState::Normal);
        assert_eq!(state, LexState::Str);
        let (tokens, state) = lex_line(r#"second" ;"#, state);
        assert_eq!(state, LexState::Normal);
        assert_eq!(
            tokens[0],
            Token {
                start: 0,
                end: 7,
                kind: String
            }
        );
        let (_, state) = lex_line(r##"let r = r#"raw "quoted""##, LexState::Normal);
        assert_eq!(state, LexState::RawStr { hashes: 1 });
        let (tokens, state) = lex_line(r##"end"# + 1"##, state);
        assert_eq!(state, LexState::Normal);
        assert_eq!(tokens[0].end, 5);
        assert_eq!(tokens[1].kind, Punctuation);
    }

    #[test]
    fn attributes_and_raw_identifiers() {
        assert_eq!(
            tokens(r#"#[derive(Debug, Clone)] #![allow(dead_code)] r#type"#),
            vec![
                t("#[derive(Debug, Clone)]", Attribute),
                t("#![allow(dead_code)]", Attribute),
                t("r#type", Ident),
            ]
        );
        assert_eq!(
            tokens(r#"#[doc = "a ] b"]"#),
            vec![t(r#"#[doc = "a ] b"]"#, Attribute)]
        );
    }

    #[test]
    fn every_line_of_this_file_lexes_without_overlap_or_gaps_outside_whitespace() {
        let source = include_str!("rust_lexer.rs");
        let mut state = LexState::Normal;
        for line in source.lines() {
            let chars: Vec<char> = line.chars().collect();
            let (tokens, next) = lex_line(line, state);
            let mut covered = vec![false; chars.len()];
            for token in &tokens {
                assert!(
                    token.start < token.end && token.end <= chars.len(),
                    "{line}"
                );
                for slot in &mut covered[token.start..token.end] {
                    assert!(!*slot, "overlap in {line:?}");
                    *slot = true;
                }
            }
            for (index, ch) in chars.iter().enumerate() {
                assert!(
                    covered[index] || ch.is_whitespace(),
                    "gap at {index} in {line:?}"
                );
            }
            state = next;
        }
        assert_eq!(state, LexState::Normal);
    }

    /// Every Rust file in the loadngo workspace: no panic, every
    /// non-whitespace character in exactly one token, and every file ends
    /// outside comments and strings.
    #[test]
    fn the_whole_workspace_lexes_cleanly() {
        fn visit(dir: &std::path::Path, files: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                let name = entry.file_name();
                if path.is_dir() {
                    if name != "target" && name != ".git" {
                        visit(&path, files);
                    }
                } else if path.extension().is_some_and(|ext| ext == "rs") {
                    files.push(path);
                }
            }
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap();
        let mut files = Vec::new();
        visit(root, &mut files);
        assert!(files.len() > 100, "{} files", files.len());
        for file in files {
            let Ok(source) = std::fs::read_to_string(&file) else {
                continue;
            };
            let mut state = LexState::Normal;
            for (number, line) in source.lines().enumerate() {
                let chars: Vec<char> = line.chars().collect();
                let (tokens, next) = lex_line(line, state);
                let mut covered = vec![false; chars.len()];
                for token in &tokens {
                    for slot in &mut covered[token.start..token.end] {
                        assert!(!*slot, "{}:{} overlap", file.display(), number + 1);
                        *slot = true;
                    }
                }
                for (index, ch) in chars.iter().enumerate() {
                    assert!(
                        covered[index] || ch.is_whitespace(),
                        "{}:{} gap at {index}",
                        file.display(),
                        number + 1
                    );
                }
                state = next;
            }
            assert_eq!(state, LexState::Normal, "{} ends open", file.display());
        }
    }
}
