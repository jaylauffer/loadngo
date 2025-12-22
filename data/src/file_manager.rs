//! Simple file manager for task persistence.

use crate::persistence;
use crate::task::Task;
use crate::task::TimeEntry;
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

    pub fn config_path(&self) -> PathBuf {
        self.base_dir.join("config.json")
    }

    pub fn time_entry_path(&self, name: &str) -> PathBuf {
        self.base_dir.join(format!("{name}-entries.json"))
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

    pub fn save_time_entries(&self, name: &str, entries: &[TimeEntry]) -> Result<()> {
        let path = self.time_entry_path(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        persistence::write_time_entry_file(path, entries)
    }

    pub fn load_time_entries(&self, name: &str) -> Result<Vec<TimeEntry>> {
        let path = self.time_entry_path(name);
        persistence::read_time_entry_file(path)
    }
}
