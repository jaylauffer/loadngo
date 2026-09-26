//! Memory a local model keeps across sessions: short notes it saves, searches, lists
//! and forgets through tools, in one append-only file of its own.
//!
//! The file is JSON lines. A note is `{"id", "at", "text"}`; forgetting appends
//! `{"forget": id}` rather than rewriting the file, so the history of what was
//! remembered and dropped stays readable. Everything is bounded: note length, results
//! returned, and how much of the memory opens a conversation ([`MemoryStore::recall`]).
//! This is the only thing the model writes, and only to this file.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::tools::{str_arg, usize_arg, Tool};

/// Longest note, in bytes.
pub const MAX_NOTE_BYTES: usize = 2048;
/// Most notes one search or listing returns.
pub const MAX_NOTES_RETURNED: usize = 20;

/// One remembered note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub id: u64,
    pub at_unix_secs: u64,
    pub text: String,
}

/// A model's memory file.
#[derive(Debug, Clone)]
pub struct MemoryStore {
    path: PathBuf,
}

impl MemoryStore {
    /// The memory at `path`, created (with its directory) on the first save.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Every note still remembered, oldest first.
    ///
    /// # Errors
    /// When the file exists but cannot be read.
    pub fn notes(&self) -> Result<Vec<Note>, String> {
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(format!("{}: {e}", self.path.display())),
        };
        let mut notes = Vec::new();
        let mut forgotten = BTreeSet::new();
        for line in text.lines() {
            let Ok(value) = serde_json::from_str::<Value>(line) else {
                continue; // a torn last line after a crash is skipped, not fatal
            };
            if let Some(id) = value.get("forget").and_then(Value::as_u64) {
                forgotten.insert(id);
            } else if let (Some(id), Some(at), Some(text)) = (
                value.get("id").and_then(Value::as_u64),
                value.get("at").and_then(Value::as_u64),
                value.get("text").and_then(Value::as_str),
            ) {
                notes.push(Note {
                    id,
                    at_unix_secs: at,
                    text: text.to_string(),
                });
            }
        }
        notes.retain(|n| !forgotten.contains(&n.id));
        Ok(notes)
    }

    fn append(&self, line: &Value) -> Result<(), String> {
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|e| format!("{}: {e}", self.path.display()))?;
        writeln!(file, "{line}").map_err(|e| format!("{}: {e}", self.path.display()))?;
        file.sync_data()
            .map_err(|e| format!("{}: {e}", self.path.display()))
    }

    /// Saves `text` as a new note and returns its id.
    ///
    /// # Errors
    /// Empty or over-long text, or a write failure.
    pub fn save(&self, text: &str) -> Result<u64, String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("the note is empty".into());
        }
        if text.len() > MAX_NOTE_BYTES {
            return Err(format!(
                "the note is {} bytes; keep notes under {MAX_NOTE_BYTES}",
                text.len()
            ));
        }
        let id = self.next_id()?;
        let at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        self.append(&json!({"id": id, "at": at, "text": text}))?;
        Ok(id)
    }

    /// Forgets note `id`.
    ///
    /// # Errors
    /// An unknown id, or a write failure.
    pub fn forget(&self, id: u64) -> Result<(), String> {
        if !self.notes()?.iter().any(|n| n.id == id) {
            return Err(format!("no remembered note {id}"));
        }
        self.append(&json!({"forget": id}))
    }

    fn next_id(&self) -> Result<u64, String> {
        // Ids are never reused, even for forgotten notes: count every line with one.
        let text = fs::read_to_string(&self.path).unwrap_or_default();
        let max = text
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter_map(|v| v.get("id").and_then(Value::as_u64))
            .max()
            .unwrap_or(0);
        Ok(max + 1)
    }

    /// Notes matching every word of `query` (case-insensitive), newest first.
    ///
    /// # Errors
    /// When the file cannot be read.
    pub fn search(&self, query: &str) -> Result<Vec<Note>, String> {
        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        let mut found: Vec<Note> = self
            .notes()?
            .into_iter()
            .filter(|n| {
                let text = n.text.to_lowercase();
                words.iter().all(|w| text.contains(w.as_str()))
            })
            .collect();
        found.reverse();
        found.truncate(MAX_NOTES_RETURNED);
        Ok(found)
    }

    /// The newest notes that fit in `budget_bytes`, oldest of them first: what opens a
    /// conversation so the model starts where it left off.
    ///
    /// # Errors
    /// When the file cannot be read.
    pub fn recall(&self, budget_bytes: usize) -> Result<Vec<Note>, String> {
        let mut picked = Vec::new();
        let mut used = 0;
        for note in self.notes()?.into_iter().rev() {
            if used + note.text.len() > budget_bytes {
                break;
            }
            used += note.text.len();
            picked.push(note);
        }
        picked.reverse();
        Ok(picked)
    }

    pub fn into_tools(self) -> Vec<Box<dyn Tool>> {
        let store = Arc::new(self);
        vec![
            Box::new(Save(Arc::clone(&store))),
            Box::new(Search(Arc::clone(&store))),
            Box::new(List(Arc::clone(&store))),
            Box::new(Forget(store)),
        ]
    }
}

/// Notes as numbered lines for the model.
pub fn format_notes(notes: &[Note]) -> String {
    let mut out = String::new();
    for n in notes {
        let _ = writeln!(out, "[{}] {}", n.id, n.text);
    }
    out
}

struct Save(Arc<MemoryStore>);
struct Search(Arc<MemoryStore>);
struct List(Arc<MemoryStore>);
struct Forget(Arc<MemoryStore>);

impl Tool for Save {
    fn name(&self) -> &'static str {
        "memory_save"
    }
    fn description(&self) -> &'static str {
        "Remember a short note across sessions (a fact, a decision, a task and its state). At most 2 KB. Returns the note's id."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "text": {"type": "string", "description": "the note, self-contained"}},
            "required": ["text"]})
    }
    fn call(&self, args: &Value) -> Result<String, String> {
        let id = self.0.save(str_arg(args, "text")?)?;
        Ok(format!("remembered as note {id}"))
    }
}

impl Tool for Search {
    fn name(&self) -> &'static str {
        "memory_search"
    }
    fn description(&self) -> &'static str {
        "Search your notes from earlier sessions for all the given words; newest first, at most 20."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "query": {"type": "string"}},
            "required": ["query"]})
    }
    fn call(&self, args: &Value) -> Result<String, String> {
        let query = str_arg(args, "query")?;
        let notes = self.0.search(query)?;
        if notes.is_empty() {
            return Ok(format!("no notes match {query:?}"));
        }
        Ok(format_notes(&notes))
    }
}

impl Tool for List {
    fn name(&self) -> &'static str {
        "memory_list"
    }
    fn description(&self) -> &'static str {
        "List your most recent notes (default 20)."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "count": {"type": "integer", "description": "how many, at most 20"}}})
    }
    fn call(&self, args: &Value) -> Result<String, String> {
        let count = usize_arg(args, "count", MAX_NOTES_RETURNED).clamp(1, MAX_NOTES_RETURNED);
        let notes = self.0.notes()?;
        if notes.is_empty() {
            return Ok("no notes yet".into());
        }
        Ok(format_notes(&notes[notes.len().saturating_sub(count)..]))
    }
}

impl Tool for Forget {
    fn name(&self) -> &'static str {
        "memory_forget"
    }
    fn description(&self) -> &'static str {
        "Forget one of your notes by its id (for example when it is wrong or out of date)."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "id": {"type": "integer"}},
            "required": ["id"]})
    }
    fn call(&self, args: &Value) -> Result<String, String> {
        let id = args
            .get("id")
            .and_then(Value::as_u64)
            .ok_or("`id` must be a whole number")?;
        self.0.forget(id)?;
        Ok(format!("forgot note {id}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, MemoryStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = MemoryStore::new(dir.path().join("kimi/memory.jsonl"));
        (dir, store)
    }

    #[test]
    fn notes_are_saved_searched_forgotten_and_ids_never_reused() {
        let (_dir, memory) = store();
        assert!(memory.notes().unwrap().is_empty());
        let a = memory.save("Jay prefers PDFs for every resume.").unwrap();
        let b = memory
            .save("The Mac mini drive had 239 GiB free on 27 September.")
            .unwrap();
        assert_eq!((a, b), (1, 2));
        assert_eq!(memory.search("pdfs RESUME").unwrap()[0].id, 1);
        assert!(memory.search("nothing like this").unwrap().is_empty());
        memory.forget(a).unwrap();
        assert!(memory.forget(a).is_err(), "already forgotten");
        let c = memory.save("A third note.").unwrap();
        assert_eq!(c, 3, "a forgotten id is not reused");
        let ids: Vec<u64> = memory.notes().unwrap().iter().map(|n| n.id).collect();
        assert_eq!(ids, [2, 3]);
    }

    #[test]
    fn empty_and_oversized_notes_are_refused() {
        let (_dir, memory) = store();
        assert!(memory.save("   ").is_err());
        assert!(memory.save(&"x".repeat(MAX_NOTE_BYTES + 1)).is_err());
    }

    #[test]
    fn recall_takes_the_newest_notes_that_fit_oldest_first() {
        let (_dir, memory) = store();
        for i in 0..10 {
            memory.save(&format!("note number {i:02}")).unwrap();
        }
        // Each note is 14 bytes; a 45-byte budget holds the newest three.
        let recalled: Vec<String> = memory
            .recall(45)
            .unwrap()
            .into_iter()
            .map(|n| n.text)
            .collect();
        assert_eq!(
            recalled,
            ["note number 07", "note number 08", "note number 09"]
        );
    }

    #[test]
    fn a_torn_last_line_does_not_lose_the_memory() {
        let (_dir, memory) = store();
        memory.save("kept").unwrap();
        let mut file = OpenOptions::new().append(true).open(memory.path()).unwrap();
        write!(file, "{{\"id\": 2, \"at\": 1, \"te").unwrap();
        let notes = memory.notes().unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].text, "kept");
    }
}
