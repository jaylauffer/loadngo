//! Reading and writing the plan's files. The app hands these requests to the
//! host's offload workers; their results come back as host-proactor
//! completions, so the window never waits on the disk.
//!
//! The files and their names are the Win32 Task's (`FileManager`): a folder
//! copied from Windows opens here.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// The plan's name; the Win32 Task always opened this one.
pub const PLAN_NAME: &str = "user_plan";

pub fn tasks_path(dir: &Path) -> PathBuf {
    dir.join(format!("{PLAN_NAME}.json"))
}

pub fn entries_path(dir: &Path) -> PathBuf {
    dir.join(format!("{PLAN_NAME}-entries.json"))
}

pub fn config_path(dir: &Path) -> PathBuf {
    dir.join("config.json")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IoRequest {
    /// Read the tasks, time entries and settings in `dir`.
    Load { dir: PathBuf },
    /// Replace each file with its contents, one at a time, each atomically.
    Save {
        revision: u64,
        files: Vec<(PathBuf, String)>,
    },
}

/// A file's text, or `None` when it does not exist yet.
pub type FileText = Option<String>;

#[derive(Debug)]
pub enum IoResponse {
    Loaded {
        tasks: Result<FileText, String>,
        entries: Result<FileText, String>,
        config: Result<FileText, String>,
    },
    Saved {
        revision: u64,
        result: Result<(), String>,
    },
}

/// Does the blocking work of `request`. Runs on an offload worker.
pub fn perform(request: IoRequest) -> IoResponse {
    match request {
        IoRequest::Load { dir } => IoResponse::Loaded {
            tasks: read(&tasks_path(&dir)),
            entries: read(&entries_path(&dir)),
            config: read(&config_path(&dir)),
        },
        IoRequest::Save { revision, files } => {
            let result = files.iter().try_for_each(|(path, text)| {
                loadngo_persistence::replace_atomically(path, text.as_bytes())
                    .map_err(|error| format!("{}: {error}", path.display()))
            });
            IoResponse::Saved { revision, result }
        }
    }
}

fn read(path: &Path) -> Result<FileText, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_then_loads() {
        let dir = tempfile::tempdir().unwrap();
        let files = vec![
            (tasks_path(dir.path()), "tasks".to_string()),
            (entries_path(dir.path()), "entries".to_string()),
        ];
        let IoResponse::Saved { revision, result } =
            perform(IoRequest::Save { revision: 7, files })
        else {
            panic!("expected a save result");
        };
        assert_eq!(revision, 7);
        result.unwrap();
        let IoResponse::Loaded {
            tasks,
            entries,
            config,
        } = perform(IoRequest::Load {
            dir: dir.path().to_path_buf(),
        })
        else {
            panic!("expected a load result");
        };
        assert_eq!(tasks.unwrap().as_deref(), Some("tasks"));
        assert_eq!(entries.unwrap().as_deref(), Some("entries"));
        assert_eq!(config.unwrap(), None);
    }
}
