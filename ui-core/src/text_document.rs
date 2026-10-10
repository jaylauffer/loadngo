#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PieceSource {
    Original,
    Add,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Piece {
    source: PieceSource,
    start_byte: usize,
    end_byte: usize,
    char_len: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HistoryEntry {
    before_pieces: Vec<Piece>,
    after_pieces: Vec<Piece>,
    before_len_chars: usize,
    after_len_chars: usize,
    before_revision: u64,
    after_revision: u64,
    /// The changed range: `start..before_end` in the text before the entry,
    /// `start..after_end` after it. Undo and redo put the caret at its end.
    start: usize,
    before_end: usize,
    after_end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextDocument {
    original: String,
    add_buffer: String,
    pieces: Vec<Piece>,
    len_chars: usize,
    revision: u64,
    undo_stack: Vec<HistoryEntry>,
    redo_stack: Vec<HistoryEntry>,
    /// Whether the newest undo entry may still absorb a touching edit made
    /// through [`TextDocument::replace_char_range_joining`].
    group_open: bool,
}

impl TextDocument {
    pub fn new(text: impl Into<String>) -> Self {
        let original = text.into();
        let len_chars = original.chars().count();
        let pieces = if original.is_empty() {
            Vec::new()
        } else {
            vec![Piece {
                source: PieceSource::Original,
                start_byte: 0,
                end_byte: original.len(),
                char_len: len_chars,
            }]
        };
        Self {
            original,
            add_buffer: String::new(),
            pieces,
            len_chars,
            revision: 0,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            group_open: false,
        }
    }

    pub fn len_chars(&self) -> usize {
        self.len_chars
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn slice_chars(&self, start: usize, end: usize) -> String {
        let start = start.min(self.len_chars);
        let end = end.min(self.len_chars);
        if start >= end {
            return String::new();
        }
        let mut remaining_start = start;
        let mut remaining_end = end;
        let mut out = String::new();
        for piece in &self.pieces {
            if remaining_end == 0 {
                break;
            }
            if remaining_start >= piece.char_len {
                remaining_start -= piece.char_len;
                remaining_end -= piece.char_len.min(remaining_end);
                continue;
            }
            let take_start = remaining_start;
            let take_end = piece.char_len.min(remaining_end);
            if take_end > take_start {
                let text = self.piece_text(piece);
                let start_byte = byte_index_for_char(text, take_start);
                let end_byte = byte_index_for_char(text, take_end);
                out.push_str(&text[start_byte..end_byte]);
            }
            remaining_start = 0;
            remaining_end = remaining_end.saturating_sub(piece.char_len);
        }
        out
    }

    pub fn for_each_chunk(&self, mut visit: impl FnMut(&str)) {
        for piece in &self.pieces {
            visit(self.piece_text(piece));
        }
    }

    /// Replaces `start..end` (characters) with `replacement` as its own undo
    /// step.
    pub fn replace_char_range(&mut self, start: usize, end: usize, replacement: &str) {
        self.group_open = false;
        self.apply_replacement(start, end, replacement, false);
        self.group_open = false;
    }

    /// As [`TextDocument::replace_char_range`], but joins the newest undo
    /// step when that step is still open and this edit touches the range it
    /// changed, so a run of typing or deleting undoes in one step. The
    /// group stays open until [`TextDocument::seal_history`] or a plain
    /// replacement, undo or redo.
    pub fn replace_char_range_joining(&mut self, start: usize, end: usize, replacement: &str) {
        self.apply_replacement(start, end, replacement, true);
        self.group_open = true;
    }

    /// Ends the open undo group, so the next joining edit starts a new step.
    pub fn seal_history(&mut self) {
        self.group_open = false;
    }

    fn apply_replacement(&mut self, start: usize, end: usize, replacement: &str, join: bool) {
        let start = start.min(self.len_chars);
        let end = end.min(self.len_chars);
        let (start, end) = if start <= end {
            (start, end)
        } else {
            (end, start)
        };
        let before_pieces = self.pieces.clone();
        let before_len_chars = self.len_chars;
        let before_revision = self.revision;

        let (before, tail) = self.split_pieces_at_char(&self.pieces, start);
        let (_, after) = self.split_pieces_at_char(&tail, end.saturating_sub(start));
        let replacement_piece = self.append_add_piece(replacement);

        let mut pieces = before;
        if let Some(piece) = replacement_piece {
            pieces.push(piece);
        }
        pieces.extend(after);
        self.pieces = coalesce_pieces(pieces);
        let inserted = replacement.chars().count();
        self.len_chars = before_len_chars - end.saturating_sub(start) + inserted;
        self.revision += 1;
        self.redo_stack.clear();

        if join && self.group_open {
            if let Some(entry) = self.undo_stack.last_mut() {
                // The edit is in the entry's after-text coordinates; join
                // only when it touches the range the entry changed.
                if start <= entry.after_end && end >= entry.start {
                    let shift = entry.after_end as isize - entry.before_end as isize;
                    let before_end = if end > entry.after_end {
                        (end as isize - shift) as usize
                    } else {
                        entry.before_end
                    };
                    let after_end = if entry.after_end >= end {
                        entry.after_end + inserted - (end - start)
                    } else {
                        start + inserted
                    };
                    entry.start = entry.start.min(start);
                    entry.before_end = before_end;
                    entry.after_end = after_end.max(start + inserted);
                    entry.after_pieces = self.pieces.clone();
                    entry.after_len_chars = self.len_chars;
                    entry.after_revision = self.revision;
                    return;
                }
            }
        }

        self.undo_stack.push(HistoryEntry {
            before_pieces,
            after_pieces: self.pieces.clone(),
            before_len_chars,
            after_len_chars: self.len_chars,
            before_revision,
            after_revision: self.revision,
            start,
            before_end: end,
            after_end: start + inserted,
        });
    }

    pub fn undo(&mut self) -> bool {
        self.undo_with_caret().is_some()
    }

    pub fn redo(&mut self) -> bool {
        self.redo_with_caret().is_some()
    }

    /// Undoes the newest step and returns where the caret belongs: the end of
    /// the text the step had replaced.
    pub fn undo_with_caret(&mut self) -> Option<usize> {
        self.group_open = false;
        let entry = self.undo_stack.pop()?;
        self.pieces = entry.before_pieces.clone();
        self.len_chars = entry.before_len_chars;
        self.revision = entry.before_revision;
        let caret = entry.before_end;
        self.redo_stack.push(entry);
        Some(caret)
    }

    /// Redoes the newest undone step and returns where the caret belongs: the
    /// end of the text the step put in.
    pub fn redo_with_caret(&mut self) -> Option<usize> {
        self.group_open = false;
        let entry = self.redo_stack.pop()?;
        self.pieces = entry.after_pieces.clone();
        self.len_chars = entry.after_len_chars;
        self.revision = entry.after_revision;
        let caret = entry.after_end;
        self.undo_stack.push(entry);
        Some(caret)
    }

    fn split_pieces_at_char(
        &self,
        pieces: &[Piece],
        char_index: usize,
    ) -> (Vec<Piece>, Vec<Piece>) {
        let mut before = Vec::new();
        let mut after = Vec::new();
        let mut remaining = char_index;
        let mut split_done = false;

        for piece in pieces {
            if split_done {
                after.push(piece.clone());
                continue;
            }
            if remaining == 0 {
                after.push(piece.clone());
                split_done = true;
                continue;
            }
            if remaining >= piece.char_len {
                before.push(piece.clone());
                remaining -= piece.char_len;
                continue;
            }

            if let Some(left) = self.slice_piece(piece, 0, remaining) {
                before.push(left);
            }
            if let Some(right) = self.slice_piece(piece, remaining, piece.char_len) {
                after.push(right);
            }
            split_done = true;
            remaining = 0;
        }

        (before, after)
    }

    fn append_add_piece(&mut self, text: &str) -> Option<Piece> {
        if text.is_empty() {
            return None;
        }
        let start_byte = self.add_buffer.len();
        self.add_buffer.push_str(text);
        let end_byte = self.add_buffer.len();
        Some(Piece {
            source: PieceSource::Add,
            start_byte,
            end_byte,
            char_len: text.chars().count(),
        })
    }

    fn slice_piece(&self, piece: &Piece, start_char: usize, end_char: usize) -> Option<Piece> {
        if start_char >= end_char || end_char > piece.char_len {
            return None;
        }
        let text = self.piece_text(piece);
        let rel_start = byte_index_for_char(text, start_char);
        let rel_end = byte_index_for_char(text, end_char);
        Some(Piece {
            source: piece.source,
            start_byte: piece.start_byte + rel_start,
            end_byte: piece.start_byte + rel_end,
            char_len: end_char - start_char,
        })
    }

    fn piece_text<'a>(&'a self, piece: &Piece) -> &'a str {
        match piece.source {
            PieceSource::Original => &self.original[piece.start_byte..piece.end_byte],
            PieceSource::Add => &self.add_buffer[piece.start_byte..piece.end_byte],
        }
    }
}

impl std::fmt::Display for TextDocument {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for piece in &self.pieces {
            f.write_str(self.piece_text(piece))?;
        }
        Ok(())
    }
}

fn coalesce_pieces(pieces: Vec<Piece>) -> Vec<Piece> {
    let mut merged: Vec<Piece> = Vec::with_capacity(pieces.len());
    for piece in pieces.into_iter().filter(|piece| piece.char_len > 0) {
        if let Some(last) = merged.last_mut() {
            if last.source == piece.source && last.end_byte == piece.start_byte {
                last.end_byte = piece.end_byte;
                last.char_len += piece.char_len;
                continue;
            }
        }
        merged.push(piece);
    }
    merged
}

fn byte_index_for_char(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .nth(char_index)
        .map(|(byte_index, _)| byte_index)
        .unwrap_or(text.len())
}

#[cfg(test)]
mod tests {
    use super::TextDocument;

    #[test]
    fn replace_range_and_undo_redo_round_trip() {
        let mut document = TextDocument::new("alpha beta gamma");
        document.replace_char_range(6, 10, "delta");
        assert_eq!(document.to_string(), "alpha delta gamma");
        assert!(document.undo());
        assert_eq!(document.to_string(), "alpha beta gamma");
        assert!(document.redo());
        assert_eq!(document.to_string(), "alpha delta gamma");
    }

    #[test]
    fn distributed_edits_preserve_piece_table_content() {
        let mut document = TextDocument::new("abcdef");
        document.replace_char_range(1, 1, "ZZ");
        document.replace_char_range(6, 6, "YY");
        document.replace_char_range(0, 2, "Q");
        assert_eq!(document.to_string(), "QZbcdYYef");
        assert_eq!(document.slice_chars(1, 5), "Zbcd");
    }

    #[test]
    fn chunk_iteration_matches_materialized_text() {
        let mut document = TextDocument::new("alpha");
        document.replace_char_range(5, 5, "\nbeta");
        let mut combined = String::new();
        document.for_each_chunk(|chunk| combined.push_str(chunk));
        assert_eq!(combined, document.to_string());
    }

    #[test]
    fn joined_typing_undoes_in_one_step_and_puts_the_caret_back() {
        let mut document = TextDocument::new("fn x() {}");
        for (offset, ch) in "let".chars().enumerate() {
            document.replace_char_range_joining(8 + offset, 8 + offset, &ch.to_string());
        }
        assert_eq!(document.to_string(), "fn x() {let}");
        assert_eq!(document.undo_with_caret(), Some(8));
        assert_eq!(document.to_string(), "fn x() {}");
        assert_eq!(document.redo_with_caret(), Some(11));
        assert_eq!(document.to_string(), "fn x() {let}");
    }

    #[test]
    fn joined_backspaces_undo_in_one_step() {
        let mut document = TextDocument::new("alpha beta");
        document.replace_char_range_joining(9, 10, "");
        document.replace_char_range_joining(8, 9, "");
        document.replace_char_range_joining(7, 8, "");
        assert_eq!(document.to_string(), "alpha b");
        assert_eq!(document.undo_with_caret(), Some(10));
        assert_eq!(document.to_string(), "alpha beta");
    }

    #[test]
    fn a_sealed_group_or_a_distant_edit_starts_a_new_step() {
        let mut document = TextDocument::new("abc");
        document.replace_char_range_joining(3, 3, "d");
        document.seal_history();
        document.replace_char_range_joining(4, 4, "e");
        document.replace_char_range_joining(0, 0, "Z");
        assert_eq!(document.to_string(), "Zabcde");
        assert!(document.undo());
        assert_eq!(document.to_string(), "abcde");
        assert!(document.undo());
        assert_eq!(document.to_string(), "abcd");
        assert!(document.undo());
        assert_eq!(document.to_string(), "abc");
        assert!(!document.undo());
    }

    #[test]
    fn a_plain_replacement_never_joins() {
        let mut document = TextDocument::new("abc");
        document.replace_char_range_joining(3, 3, "d");
        document.replace_char_range(4, 4, "e");
        document.replace_char_range_joining(5, 5, "f");
        assert!(document.undo());
        assert_eq!(document.to_string(), "abcde");
        assert!(document.undo());
        assert_eq!(document.to_string(), "abcd");
    }

    #[test]
    fn joined_mixed_edits_restore_exactly() {
        // Typing then deleting past the typed text's start, in one group.
        let mut document = TextDocument::new("hello world");
        document.replace_char_range_joining(5, 5, "X");
        document.replace_char_range_joining(6, 6, "Y");
        document.replace_char_range_joining(6, 7, "");
        document.replace_char_range_joining(5, 6, "");
        document.replace_char_range_joining(4, 5, "");
        assert_eq!(document.to_string(), "hell world");
        assert_eq!(document.undo_with_caret(), Some(5));
        assert_eq!(document.to_string(), "hello world");
        assert_eq!(document.redo_with_caret(), Some(4));
        assert_eq!(document.to_string(), "hell world");
    }
}
