//! What the editor remembers between launches: the folder, the open tabs
//! (blank pages too) and the expanded directories (`session.json`), and unsaved
//! edits (`backups/`), so closing the window or losing power loses no typing.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionTab {
    /// The file; for a blank page, its name ("Untitled 1"), which keys its
    /// backup.
    pub path: PathBuf,
    /// Caret position in characters.
    pub caret: usize,
    /// A blank page not yet saved, by its number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub untitled: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Session {
    pub root: PathBuf,
    pub tabs: Vec<SessionTab>,
    pub active: Option<usize>,
    pub expanded: Vec<PathBuf>,
    /// Where the last page was saved; the next save starts there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_save_dir: Option<PathBuf>,
}

/// Unsaved text for one file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Backup {
    pub path: PathBuf,
    pub text: String,
}

pub fn session_path(state_dir: &Path) -> PathBuf {
    state_dir.join("session.json")
}

/// Where the unsaved text of `file` is kept: named by a hash of its path so
/// any path maps to one plain file name.
pub fn backup_path(state_dir: &Path, file: &Path) -> PathBuf {
    let hash = blake3::hash(file.to_string_lossy().as_bytes());
    state_dir
        .join("backups")
        .join(format!("{}.json", &hash.to_hex()[..32]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_round_trips_through_json() {
        let session = Session {
            root: PathBuf::from("/w"),
            tabs: vec![
                SessionTab {
                    path: PathBuf::from("/w/src/lib.rs"),
                    caret: 42,
                    untitled: None,
                },
                SessionTab {
                    path: PathBuf::from("Untitled 2"),
                    caret: 3,
                    untitled: Some(2),
                },
            ],
            active: Some(0),
            expanded: vec![PathBuf::from("/w/src")],
            last_save_dir: Some(PathBuf::from("/w/notes")),
        };
        let bytes = serde_json::to_vec(&session).unwrap();
        assert_eq!(serde_json::from_slice::<Session>(&bytes).unwrap(), session);
    }

    /// Sessions written before blank pages existed still read.
    #[test]
    fn an_older_session_reads_without_pages() {
        let old = br#"{"root": "/w", "tabs": [{"path": "/w/a.rs", "caret": 1}],
            "active": 0, "expanded": []}"#;
        let session: Session = serde_json::from_slice(old).unwrap();
        assert_eq!(session.tabs[0].untitled, None);
        assert_eq!(session.last_save_dir, None);
    }

    #[test]
    fn each_file_has_its_own_backup_name() {
        let dir = Path::new("/state");
        let a = backup_path(dir, Path::new("/w/a.rs"));
        let b = backup_path(dir, Path::new("/w/b.rs"));
        assert_ne!(a, b);
        assert_eq!(a, backup_path(dir, Path::new("/w/a.rs")));
        assert!(a.starts_with("/state/backups"));
    }
}
