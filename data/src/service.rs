//! Service orchestration stub that manages tasks, configuration, and undo.

use crate::config::Configuration;
use crate::file_manager::FileManager;
use crate::task::Task;
use crate::task::TimeEntry;
use crate::undo::{Command, UndoStack};
use crate::value::Value;
use anyhow::Result;
use std::collections::HashMap;

#[derive(Debug)]
pub struct Service {
    pub config: Configuration,
    pub files: FileManager,
    pub tasks: HashMap<u64, Task>,
    pub time_entries: HashMap<u64, TimeEntry>,
    pub undo: UndoStack,
}

impl Service {
    pub fn new(config: Configuration, files: FileManager) -> Self {
        Self {
            config,
            files,
            tasks: HashMap::new(),
            time_entries: HashMap::new(),
            undo: UndoStack::default(),
        }
    }

    pub fn add_task(&mut self, task: Task) {
        let id = task.entity.id;
        self.undo
            .apply(Command::AddTask { task: task.clone() }, &mut self.tasks);
        self.tasks.insert(id, task);
    }

    pub fn remove_task(&mut self, id: u64) {
        if let Some(task) = self.tasks.remove(&id) {
            self.undo
                .apply(Command::RemoveTask { task }, &mut self.tasks);
        }
    }

    /// Apply a property update to a task, tracking undo.
    pub fn update_task_property(&mut self, id: u64, key: String, value: Value) {
        if let Some(task) = self.tasks.get_mut(&id) {
            let old = task.properties.get(&key).cloned().unwrap_or(Value::Null);
            task.properties.insert(key.clone(), value.clone());
            self.undo.apply(
                Command::UpdateProperty {
                    id,
                    key,
                    old,
                    new: value,
                },
                &mut self.tasks,
            );
        }
    }

    /// Apply a move (re-parent) operation.
    pub fn move_task(&mut self, id: u64, new_parent: Option<u64>) {
        if let Some(task) = self.tasks.get_mut(&id) {
            task.parent = new_parent;
        }
    }

    pub fn save(&self, name: &str) -> Result<()> {
        let tasks: Vec<Task> = self.tasks.values().cloned().collect();
        self.files.save_tasks(name, &tasks)
    }

    pub fn save_entries(&self, name: &str) -> Result<()> {
        let entries: Vec<TimeEntry> = self.time_entries.values().cloned().collect();
        self.files.save_time_entries(name, &entries)
    }

    pub fn load(&mut self, name: &str) -> Result<()> {
        let tasks = self.files.load_tasks(name)?;
        self.tasks = tasks.into_iter().map(|t| (t.entity.id, t)).collect();
        Ok(())
    }

    pub fn load_entries(&mut self, name: &str) -> Result<()> {
        let entries = self.files.load_time_entries(name)?;
        self.time_entries = entries.into_iter().map(|e| (e.entity.id, e)).collect();
        Ok(())
    }

    pub fn save_all(&self, name: &str) -> Result<()> {
        self.config.save_to_file(self.files.config_path())?;
        self.save(name)?;
        self.save_entries(name)
    }

    pub fn load_all(&mut self, name: &str) -> Result<()> {
        // Load configuration first, then tasks.
        let _ = self.config.load_from_file(self.files.config_path());
        self.load(name)?;
        let _ = self.load_entries(name);
        Ok(())
    }
}
