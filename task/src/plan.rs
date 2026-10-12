//! The plan: tasks and time entries, the edits the app makes to them, undo,
//! and the JSON the Win32 Task reads and writes (`user_plan.json`,
//! `user_plan-entries.json`).

use std::collections::HashMap;

use anyhow::{Context, Result};
use data::entity::Entity;
use data::model_utils::{generate_id, now_timestamp};
use data::persistence::{tasks_from_json_str, tasks_to_json_string, TimeEntryFile};
use data::task::{EntryKind, Task, TimeEntry};
use data::types::Id;

use crate::clock::{TICKS_PER_DAY, TICKS_PER_SLOT};

/// Edits kept for undo.
const UNDO_LIMIT: usize = 100;
/// The highest priority mark (the Win32 Task drew 1 to 5).
pub const MAX_PRIORITY: i32 = 5;

#[derive(Clone, Default)]
struct Snapshot {
    tasks: HashMap<Id, Task>,
    entries: HashMap<Id, TimeEntry>,
}

/// Tasks and time entries, with undo. Every edit raises `revision`, which
/// the app compares with the last saved revision.
#[derive(Default)]
pub struct Plan {
    current: Snapshot,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    revision: u64,
    owner: String,
}

impl Plan {
    /// An empty plan whose new tasks and entries name `owner`.
    pub fn new(owner: impl Into<String>) -> Self {
        Self {
            owner: owner.into(),
            ..Self::default()
        }
    }

    /// Replaces everything with a loaded plan; undo starts afresh.
    pub fn replace(&mut self, tasks: Vec<Task>, entries: Vec<TimeEntry>) {
        self.current = Snapshot {
            tasks: tasks.into_iter().map(|t| (t.entity.id, t)).collect(),
            entries: entries.into_iter().map(|e| (e.entity.id, e)).collect(),
        };
        self.undo.clear();
        self.redo.clear();
        self.revision += 1;
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn task(&self, id: Id) -> Option<&Task> {
        self.current.tasks.get(&id)
    }

    pub fn entry(&self, id: Id) -> Option<&TimeEntry> {
        self.current.entries.get(&id)
    }

    pub fn task_count(&self) -> usize {
        self.current.tasks.len()
    }

    /// Keeps the plan as it is now for undo. Every edit below does this
    /// itself; a drag calls it once when it starts and then uses the `live`
    /// edits, so one drag is one undo step.
    pub fn checkpoint(&mut self) {
        self.undo.push(self.current.clone());
        if self.undo.len() > UNDO_LIMIT {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo(&mut self) -> bool {
        let Some(previous) = self.undo.pop() else {
            return false;
        };
        self.redo
            .push(std::mem::replace(&mut self.current, previous));
        self.revision += 1;
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop() else {
            return false;
        };
        self.undo.push(std::mem::replace(&mut self.current, next));
        self.revision += 1;
        true
    }

    /// Adds a task named `name` under `parent` (or at the top level).
    pub fn add_task(&mut self, name: &str, parent: Option<Id>) -> Id {
        self.checkpoint();
        // Machine and user 1, as the Win32 Task creates them.
        let mut task = Task::spawn(name.trim(), self.owner.clone(), 1, 1, now_timestamp());
        task.parent = parent.filter(|id| self.current.tasks.contains_key(id));
        let id = task.entity.id;
        self.current.tasks.insert(id, task);
        self.revision += 1;
        id
    }

    /// The task called `name` (ignoring case and surrounding spaces).
    pub fn find_task(&self, name: &str) -> Option<Id> {
        let name = name.trim();
        let mut matches: Vec<&Task> = self
            .current
            .tasks
            .values()
            .filter(|task| task.name.trim().eq_ignore_ascii_case(name))
            .collect();
        matches.sort_by_key(|task| task.entity.id);
        matches.first().map(|task| task.entity.id)
    }

    pub fn rename_task(&mut self, id: Id, name: &str) -> bool {
        let name = name.trim();
        match self.current.tasks.get(&id) {
            Some(task) if !name.is_empty() && task.name != name => {}
            _ => return false,
        }
        self.checkpoint();
        if let Some(task) = self.current.tasks.get_mut(&id) {
            task.name = name.to_string();
        }
        self.revision += 1;
        true
    }

    /// Sets a priority mark, 1 (highest) to `MAX_PRIORITY`, or 0 for none.
    pub fn set_priority(&mut self, id: Id, priority: i32) -> bool {
        let priority = priority.clamp(0, MAX_PRIORITY);
        match self.current.tasks.get(&id) {
            Some(task) if task.priority != priority => {}
            _ => return false,
        }
        self.checkpoint();
        if let Some(task) = self.current.tasks.get_mut(&id) {
            task.priority = priority;
        }
        self.revision += 1;
        true
    }

    /// Moves a task under `parent`, or to the top level. Refused when it
    /// would put a task under itself or one of its own subtasks.
    pub fn set_parent(&mut self, id: Id, parent: Option<Id>) -> bool {
        let Some(task) = self.current.tasks.get(&id) else {
            return false;
        };
        if task.parent == parent {
            return false;
        }
        if let Some(parent) = parent {
            if !self.current.tasks.contains_key(&parent) || self.is_within(parent, id) {
                return false;
            }
        }
        self.checkpoint();
        if let Some(task) = self.current.tasks.get_mut(&id) {
            task.parent = parent;
        }
        self.revision += 1;
        true
    }

    /// Deletes a task. Its subtasks move up to its parent; its time entries
    /// stay, as a record of the time.
    pub fn delete_task(&mut self, id: Id) -> bool {
        let Some(parent) = self.current.tasks.get(&id).map(|task| task.parent) else {
            return false;
        };
        self.checkpoint();
        self.current.tasks.remove(&id);
        for task in self.current.tasks.values_mut() {
            if task.parent == Some(id) {
                task.parent = parent;
            }
        }
        self.revision += 1;
        true
    }

    /// Whether `id` is `ancestor` or below it.
    pub fn is_within(&self, id: Id, ancestor: Id) -> bool {
        let mut current = Some(id);
        // Bounded by the task count, so a cycle in a loaded file cannot hang.
        for _ in 0..=self.current.tasks.len() {
            match current {
                Some(found) if found == ancestor => return true,
                Some(found) => current = self.current.tasks.get(&found).and_then(|t| t.parent),
                None => return false,
            }
        }
        false
    }

    /// Top-level tasks: no parent, or a parent that is not in the plan.
    pub fn roots(&self) -> Vec<&Task> {
        let mut roots: Vec<&Task> = self
            .current
            .tasks
            .values()
            .filter(|task| {
                task.parent
                    .is_none_or(|parent| !self.current.tasks.contains_key(&parent))
            })
            .collect();
        sort_tasks(&mut roots);
        roots
    }

    pub fn children(&self, id: Id) -> Vec<&Task> {
        let mut children: Vec<&Task> = self
            .current
            .tasks
            .values()
            .filter(|task| task.parent == Some(id))
            .collect();
        sort_tasks(&mut children);
        children
    }

    /// Tasks with no subtasks, all of them or those within `context`: what
    /// the day plan's list offers to schedule.
    pub fn leaves(&self, context: Option<Id>) -> Vec<&Task> {
        let parents: std::collections::HashSet<Id> = self
            .current
            .tasks
            .values()
            .filter_map(|task| task.parent)
            .collect();
        let mut leaves: Vec<&Task> = self
            .current
            .tasks
            .values()
            .filter(|task| !parents.contains(&task.entity.id))
            .filter(|task| {
                context
                    .is_none_or(|ctx| task.entity.id != ctx && self.is_within(task.entity.id, ctx))
            })
            .collect();
        sort_tasks(&mut leaves);
        leaves
    }

    /// Task names that contain `text`, those starting with it first.
    pub fn names_matching(&self, text: &str, limit: usize) -> Vec<(Id, String)> {
        let needle = text.trim().to_lowercase();
        let mut found: Vec<(bool, String, Id)> = self
            .current
            .tasks
            .values()
            .filter_map(|task| {
                let lower = task.name.to_lowercase();
                lower
                    .contains(&needle)
                    .then(|| (!lower.starts_with(&needle), lower, task.entity.id))
            })
            .collect();
        found.sort();
        found
            .into_iter()
            .take(limit)
            .filter_map(|(_, _, id)| self.task(id).map(|task| (id, task.name.clone())))
            .collect()
    }

    /// Adds a time entry for `task_id` from `start` to `stop`.
    pub fn add_entry(&mut self, task_id: Id, kind: EntryKind, start: u64, stop: u64) -> Id {
        self.checkpoint();
        let title = self
            .task(task_id)
            .map(|task| task.name.clone())
            .unwrap_or_default();
        let stop = stop.max(start + TICKS_PER_SLOT);
        let entry = TimeEntry {
            entity: Entity::new(
                generate_id(),
                generate_id(),
                self.owner.clone(),
                now_timestamp(),
            ),
            task_id,
            duration: stop - start,
            start,
            stop,
            title,
            kind,
            notes: None,
        };
        let id = entry.entity.id;
        self.current.entries.insert(id, entry);
        self.revision += 1;
        id
    }

    /// Moves or resizes an entry during a drag, without an undo step of its
    /// own (the drag took one when it started).
    pub fn set_entry_time_live(&mut self, id: Id, start: u64, stop: u64) -> bool {
        let Some(entry) = self.current.entries.get_mut(&id) else {
            return false;
        };
        let stop = stop.max(start + TICKS_PER_SLOT);
        if entry.start == start && entry.stop == stop {
            return false;
        }
        entry.start = start;
        entry.stop = stop;
        entry.duration = stop - start;
        self.revision += 1;
        true
    }

    pub fn delete_entry(&mut self, id: Id) -> bool {
        if !self.current.entries.contains_key(&id) {
            return false;
        }
        self.checkpoint();
        self.current.entries.remove(&id);
        self.revision += 1;
        true
    }

    /// Entries of `kind` that overlap the day starting at `day_start`, by
    /// start time.
    pub fn entries_on(&self, day_start: u64, kind: PaneKind) -> Vec<&TimeEntry> {
        let day_end = day_start + TICKS_PER_DAY;
        let mut entries: Vec<&TimeEntry> = self
            .current
            .entries
            .values()
            .filter(|entry| PaneKind::of(&entry.kind) == kind)
            .filter(|entry| entry.stop > day_start && entry.start < day_end)
            .collect();
        entries.sort_by_key(|entry| (entry.start, entry.entity.id));
        entries
    }

    /// The tasks file, as the Win32 Task writes it.
    pub fn tasks_json(&self) -> Result<String> {
        let tasks: Vec<Task> = self.current.tasks.values().cloned().collect();
        tasks_to_json_string(&tasks)
    }

    /// The time entries file, as the Win32 Task writes it.
    pub fn entries_json(&self) -> Result<String> {
        let mut entries: Vec<TimeEntry> = self.current.entries.values().cloned().collect();
        entries.sort_by(|a, b| a.entity.id.cmp(&b.entity.id).then(a.start.cmp(&b.start)));
        let file = TimeEntryFile {
            version: 1,
            generated_at: now_timestamp(),
            entries,
        };
        Ok(serde_json::to_string_pretty(&file)?)
    }
}

/// Reads the two plan files; a missing file is an empty list.
pub fn parse_plan(
    tasks: Option<&str>,
    entries: Option<&str>,
) -> Result<(Vec<Task>, Vec<TimeEntry>)> {
    let tasks = match tasks {
        Some(text) => tasks_from_json_str(text).context("the tasks file")?,
        None => Vec::new(),
    };
    let entries = match entries {
        Some(text) => {
            serde_json::from_str::<TimeEntryFile>(text)
                .context("the time entries file")?
                .entries
        }
        None => Vec::new(),
    };
    Ok((tasks, entries))
}

/// Priority marks first (1 before 5), unmarked last, then by name.
fn sort_tasks(tasks: &mut [&Task]) {
    tasks.sort_by(|a, b| {
        let rank = |task: &Task| {
            if task.priority > 0 {
                task.priority
            } else {
                i32::MAX
            }
        };
        rank(a)
            .cmp(&rank(b))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then(a.entity.id.cmp(&b.entity.id))
    });
}

/// The day planner's two columns: what was planned, and what was done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneKind {
    Planned,
    Actual,
}

impl PaneKind {
    pub fn of(kind: &EntryKind) -> Self {
        match kind {
            EntryKind::Spec => Self::Planned,
            EntryKind::Actual => Self::Actual,
        }
    }

    pub fn entry_kind(self) -> EntryKind {
        match self {
            Self::Planned => EntryKind::Spec,
            Self::Actual => EntryKind::Actual,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Planned => "Planned",
            Self::Actual => "Actual",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(tasks: &[&Task]) -> Vec<String> {
        tasks.iter().map(|task| task.name.clone()).collect()
    }

    #[test]
    fn hierarchy_queries() {
        let mut plan = Plan::new("me");
        let project = plan.add_task("Project", None);
        let milestone = plan.add_task("Milestone", Some(project));
        let a = plan.add_task("Write", Some(milestone));
        let b = plan.add_task("Test", Some(milestone));
        let other = plan.add_task("Errand", None);
        assert_eq!(names(&plan.roots()), ["Errand", "Project"]);
        assert_eq!(names(&plan.leaves(None)), ["Errand", "Test", "Write"]);
        assert_eq!(names(&plan.leaves(Some(project))), ["Test", "Write"]);
        assert!(plan.leaves(Some(other)).is_empty());
        assert!(plan.set_priority(a, 1));
        assert_eq!(names(&plan.leaves(None)), ["Write", "Errand", "Test"]);
        assert!(plan.is_within(b, project));
        assert!(!plan.is_within(project, b));
    }

    #[test]
    fn parent_cannot_move_under_itself() {
        let mut plan = Plan::new("me");
        let top = plan.add_task("Top", None);
        let child = plan.add_task("Child", Some(top));
        assert!(!plan.set_parent(top, Some(child)));
        assert!(!plan.set_parent(top, Some(top)));
        let other = plan.add_task("Other", None);
        assert!(plan.set_parent(top, Some(other)));
        assert!(plan.set_parent(top, None));
    }

    #[test]
    fn deleting_a_task_moves_its_subtasks_up() {
        let mut plan = Plan::new("me");
        let top = plan.add_task("Top", None);
        let middle = plan.add_task("Middle", Some(top));
        let low = plan.add_task("Low", Some(middle));
        assert!(plan.delete_task(middle));
        assert_eq!(plan.task(low).unwrap().parent, Some(top));
    }

    #[test]
    fn undo_and_redo_restore_whole_edits() {
        let mut plan = Plan::new("me");
        let id = plan.add_task("One", None);
        assert!(plan.rename_task(id, "Two"));
        assert!(plan.undo());
        assert_eq!(plan.task(id).unwrap().name, "One");
        assert!(plan.redo());
        assert_eq!(plan.task(id).unwrap().name, "Two");
        assert!(plan.undo());
        assert!(plan.undo());
        assert!(plan.task(id).is_none());
        assert!(!plan.undo());
    }

    #[test]
    fn a_drag_is_one_undo_step() {
        let mut plan = Plan::new("me");
        let task = plan.add_task("Work", None);
        let entry = plan.add_entry(task, EntryKind::Spec, 0, TICKS_PER_SLOT);
        plan.checkpoint();
        for slot in 1..5 {
            plan.set_entry_time_live(entry, slot * TICKS_PER_SLOT, (slot + 1) * TICKS_PER_SLOT);
        }
        assert_eq!(plan.entry(entry).unwrap().start, 4 * TICKS_PER_SLOT);
        assert!(plan.undo());
        assert_eq!(plan.entry(entry).unwrap().start, 0);
    }

    #[test]
    fn entries_are_found_by_day_and_column() {
        let mut plan = Plan::new("me");
        let task = plan.add_task("Work", None);
        let day = 10 * TICKS_PER_DAY;
        plan.add_entry(task, EntryKind::Spec, day + 4 * TICKS_PER_SLOT, day);
        plan.add_entry(task, EntryKind::Actual, day + TICKS_PER_SLOT, 0);
        plan.add_entry(task, EntryKind::Spec, day + TICKS_PER_DAY, 0);
        assert_eq!(plan.entries_on(day, PaneKind::Planned).len(), 1);
        assert_eq!(plan.entries_on(day, PaneKind::Actual).len(), 1);
        let entry = plan.entries_on(day, PaneKind::Planned)[0];
        assert_eq!(entry.stop - entry.start, TICKS_PER_SLOT);
        assert_eq!(entry.title, "Work");
    }

    #[test]
    fn files_round_trip_in_the_win32_format() {
        let mut plan = Plan::new("me");
        let top = plan.add_task("Top", None);
        let child = plan.add_task("Child", Some(top));
        plan.add_entry(child, EntryKind::Actual, 10 * TICKS_PER_SLOT, 0);
        let tasks = plan.tasks_json().unwrap();
        let entries = plan.entries_json().unwrap();
        assert!(entries.contains("\"Actual\""));
        let (tasks, entries) = parse_plan(Some(&tasks), Some(&entries)).unwrap();
        let mut loaded = Plan::new("me");
        loaded.replace(tasks, entries);
        assert_eq!(loaded.task(child).unwrap().parent, Some(top));
        assert_eq!(loaded.entries_on(0, PaneKind::Actual).len(), 1);
    }

    #[test]
    fn matching_names_put_prefixes_first() {
        let mut plan = Plan::new("me");
        plan.add_task("Review code", None);
        plan.add_task("Code review", None);
        plan.add_task("Lunch", None);
        let found: Vec<String> = plan
            .names_matching("code", 5)
            .into_iter()
            .map(|(_, name)| name)
            .collect();
        assert_eq!(found, ["Code review", "Review code"]);
    }
}
