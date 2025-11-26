use data::{
    model_utils::now_timestamp,
    task::Task,
    persistence,
    undo::UndoStack,
    value::Value,
};
use std::collections::HashMap;
use tempfile::NamedTempFile;

#[test]
fn task_round_trip_serialization() {
    let created = now_timestamp();
    let mut task = Task::spawn("demo", "owner", 1, 2, created);
    task.description = Some("desc".to_string());
    task.parent = Some(42);
    task.priority = 5;
    task.due_date = 12345;
    task.scheduled_start = 10000;
    task.estimated_duration = 3600;
    task
        .properties
        .insert("title".to_string(), Value::Str("demo".into()));
    task.properties.insert("flag".to_string(), Value::Bool(true));
    task.properties.insert("count".to_string(), Value::U64(7));

    let file = NamedTempFile::new().expect("temp file");
    let path = file.path();

    // Save using deterministic helper
    persistence::write_task_file(path, &[task.clone()]).expect("write");

    // Load
    let loaded: Vec<Task> = persistence::read_task_file(path).expect("read");
    assert_eq!(loaded.len(), 1);
    let t = &loaded[0];

    assert_eq!(t.name, task.name);
    assert_eq!(t.description, task.description);
    assert_eq!(t.parent, task.parent);
    assert_eq!(t.priority, task.priority);
    assert_eq!(t.due_date, task.due_date);
    assert_eq!(t.scheduled_start, task.scheduled_start);
    assert_eq!(t.estimated_duration, task.estimated_duration);
    assert_eq!(t.properties, task.properties);
}

#[test]
fn undo_round_trip_add_remove() {
    let created = now_timestamp();
    let task = Task::spawn("undo", "owner", 1, 2, created);
    let mut tasks: HashMap<u64, Task> = HashMap::new();
    let mut undo = UndoStack::default();

    undo.apply(data::undo::Command::AddTask { task: task.clone() }, &mut tasks);
    assert!(tasks.contains_key(&task.entity.id));

    undo.undo(&mut tasks);
    assert!(!tasks.contains_key(&task.entity.id));

    undo.redo(&mut tasks);
    assert!(tasks.contains_key(&task.entity.id));
}

#[test]
fn deterministic_output_for_sorted_tasks() {
    let created = now_timestamp();
    let mut t1 = Task::spawn("b-task", "owner", 1, 2, created);
    t1.properties.insert("z".into(), Value::U64(1));
    let mut t2 = Task::spawn("a-task", "owner", 1, 2, created);
    t2.properties.insert("a".into(), Value::U64(2));

    let file1 = NamedTempFile::new().unwrap();
    let file2 = NamedTempFile::new().unwrap();

    // Write with tasks in different orders
    persistence::write_task_file(file1.path(), &[t1.clone(), t2.clone()]).unwrap();
    persistence::write_task_file(file2.path(), &[t2, t1]).unwrap();

    let bytes1 = std::fs::read(file1.path()).unwrap();
    let bytes2 = std::fs::read(file2.path()).unwrap();
    assert_eq!(bytes1, bytes2, "deterministic ordering should produce identical output");
}
