//! Simple file manager for task persistence.

use crate::persistence;
use crate::task::Task;
use anyhow::Result;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct FileManager {
    base_dir: PathBuf,
}

impl FileManager {
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: base_dir.into(),
        }
    }

    pub fn task_path(&self, name: &str) -> PathBuf {
        self.base_dir.join(format!("{name}.json"))
    }

    pub fn save_tasks(&self, name: &str, tasks: &[Task]) -> Result<()> {
        let path = self.task_path(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        persistence::write_task_file(path, tasks)
    }

    pub fn load_tasks(&self, name: &str) -> Result<Vec<Task>> {
        let path = self.task_path(name);
        persistence::read_task_file(path)
    }
}
