//! What the editor remembers between launches: the folder, the open tabs
//! and the expanded directories (`session.json`), and unsaved edits
//! (`backups/`), so closing the window or losing power loses no typing.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionTab {
    pub path: PathBuf,
    /// Caret position in characters.
    pub caret: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Session {
    pub root: PathBuf,
    pub tabs: Vec<SessionTab>,
    pub active: Option<usize>,
    pub expanded: Vec<PathBuf>,
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
            tabs: vec![SessionTab {
                path: PathBuf::from("/w/src/lib.rs"),
                caret: 42,
            }],
            active: Some(0),
            expanded: vec![PathBuf::from("/w/src")],
        };
        let bytes = serde_json::to_vec(&session).unwrap();
        assert_eq!(serde_json::from_slice::<Session>(&bytes).unwrap(), session);
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
