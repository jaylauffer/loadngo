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
        }
    }

    pub fn len_chars(&self) -> usize {
        self.len_chars
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn to_string(&self) -> String {
        let mut text = String::with_capacity(
            self.pieces
                .iter()
                .map(|piece| self.piece_text(piece).len())
                .sum(),
        );
        for piece in &self.pieces {
            text.push_str(self.piece_text(piece));
        }
        text
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

    pub fn replace_char_range(&mut self, start: usize, end: usize, replacement: &str) {
        let start = start.min(self.len_chars);
        let end = end.min(self.len_chars);
        let (start, end) = if start <= end { (start, end) } else { (end, start) };
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
        self.len_chars = before_len_chars - end.saturating_sub(start) + replacement.chars().count();
        self.revision += 1;

        self.undo_stack.push(HistoryEntry {
            before_pieces,
            after_pieces: self.pieces.clone(),
            before_len_chars,
            after_len_chars: self.len_chars,
            before_revision,
            after_revision: self.revision,
        });
        self.redo_stack.clear();
    }

    pub fn undo(&mut self) -> bool {
        let Some(entry) = self.undo_stack.pop() else {
            return false;
        };
        self.pieces = entry.before_pieces.clone();
        self.len_chars = entry.before_len_chars;
        self.revision = entry.before_revision;
        self.redo_stack.push(entry);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(entry) = self.redo_stack.pop() else {
            return false;
        };
        self.pieces = entry.after_pieces.clone();
        self.len_chars = entry.after_len_chars;
        self.revision = entry.after_revision;
        self.undo_stack.push(entry);
        true
    }

    fn split_pieces_at_char(&self, pieces: &[Piece], char_index: usize) -> (Vec<Piece>, Vec<Piece>) {
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
}
