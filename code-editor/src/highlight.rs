//! Syntax colors for a buffer, kept line by line.
//!
//! Lines are lexed only as far down as the screen reaches, each starting in
//! the state the line above ended in. An edit drops what was kept from its
//! line on, so typing re-lexes the lines between the edit and the bottom of
//! the screen, not the file.

use std::path::Path;

use ui_core::{Color, TextAreaModel, TextRun};

use crate::rust_lexer::{lex_line, LexState, TokenKind};
use crate::theme;

#[derive(Debug, Default)]
pub struct Highlighter {
    /// For each lexed line from the top: its runs and the state it ends in.
    lines: Vec<(Vec<TextRun>, LexState)>,
}

impl Highlighter {
    /// A highlighter for `path`'s language, if there is one (Rust for now).
    pub fn for_path(path: &Path) -> Option<Self> {
        (path.extension().and_then(|ext| ext.to_str()) == Some("rs")).then(Self::default)
    }

    /// Brings the colors up to date with `area`, which must have been laid
    /// out since its last change.
    pub fn update(&mut self, area: &mut TextAreaModel) {
        if let Some(changed) = area.take_changed_from_line() {
            self.lines.truncate(changed);
        }
        let wanted = area.visible_lines().end.min(area.line_count());
        while self.lines.len() < wanted {
            let index = self.lines.len();
            let state = self
                .lines
                .last()
                .map_or(LexState::Normal, |(_, state)| *state);
            let Some(text) = area.line_text(index) else {
                break;
            };
            let (tokens, next) = lex_line(&text, state);
            let runs = tokens
                .into_iter()
                .map(|token| TextRun {
                    start: token.start,
                    end: token.end,
                    color: color(token.kind),
                })
                .collect();
            self.lines.push((runs, next));
        }
    }

    /// The colored runs of line `line`, once lexed.
    pub fn runs(&self, line: usize) -> Option<&[TextRun]> {
        self.lines.get(line).map(|(runs, _)| runs.as_slice())
    }

    /// Lines lexed and kept.
    pub fn lexed_lines(&self) -> usize {
        self.lines.len()
    }
}

fn color(kind: TokenKind) -> Color {
    match kind {
        TokenKind::Keyword => theme::SYNTAX_KEYWORD,
        TokenKind::Type => theme::SYNTAX_TYPE,
        TokenKind::Function => theme::SYNTAX_FUNCTION,
        TokenKind::Macro => theme::SYNTAX_MACRO,
        TokenKind::Lifetime => theme::SYNTAX_LIFETIME,
        TokenKind::String | TokenKind::Char => theme::SYNTAX_STRING,
        TokenKind::Number | TokenKind::Constant => theme::SYNTAX_NUMBER,
        TokenKind::Comment => theme::SYNTAX_COMMENT,
        TokenKind::DocComment => theme::SYNTAX_DOC,
        TokenKind::Attribute => theme::SYNTAX_ATTRIBUTE,
        TokenKind::Punctuation => theme::SYNTAX_PUNCTUATION,
        TokenKind::Ident => theme::TEXT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui_core::{Rect, UiEvent};

    fn area(text: &str) -> TextAreaModel {
        let mut area = TextAreaModel::new(
            text,
            Rect {
                x: 0.0,
                y: 0.0,
                width: 600.0,
                height: 200.0,
            },
        );
        area.focused = true;
        area.relayout(|text: &str, size: u16| text.chars().count() as f32 * f32::from(size) * 0.5);
        area
    }

    fn relayout(area: &mut TextAreaModel) {
        area.relayout(|text: &str, size: u16| text.chars().count() as f32 * f32::from(size) * 0.5);
    }

    #[test]
    fn lexes_only_down_to_the_screen_and_reaches_more_when_scrolled() {
        let text: String = (0..500).map(|i| format!("let x{i} = {i};\n")).collect();
        let mut area = area(&text);
        let mut highlighter = Highlighter::default();
        highlighter.update(&mut area);
        let first = highlighter.lexed_lines();
        assert!(first > 3 && first < 20, "{first} lines lexed");
        area.set_caret(text.chars().count());
        relayout(&mut area);
        highlighter.update(&mut area);
        assert_eq!(highlighter.lexed_lines(), 501);
    }

    #[test]
    fn an_edit_relexes_from_its_line_with_the_state_above() {
        let mut area = area("let a = 1;\nlet b = 2;\nlet c = 3;\n");
        let mut highlighter = Highlighter::default();
        highlighter.update(&mut area);
        assert_eq!(highlighter.runs(2).unwrap()[0].color, theme::SYNTAX_KEYWORD);
        // Opening a block comment on line 1 turns line 2 into comment.
        area.set_caret(11);
        let _ = area.handle_event(UiEvent::TextInput {
            text: "/*".to_string(),
        });
        relayout(&mut area);
        highlighter.update(&mut area);
        assert_eq!(highlighter.runs(0).unwrap()[0].color, theme::SYNTAX_KEYWORD);
        let line_two = highlighter.runs(2).unwrap();
        assert_eq!(line_two.len(), 1);
        assert_eq!(line_two[0].color, theme::SYNTAX_COMMENT);
    }

    #[test]
    fn only_rust_files_get_a_highlighter() {
        assert!(Highlighter::for_path(Path::new("src/main.rs")).is_some());
        assert!(Highlighter::for_path(Path::new("Cargo.toml")).is_none());
    }
}
