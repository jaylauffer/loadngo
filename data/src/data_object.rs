//! Data object scaffolding to mirror *DataObject classes.

use crate::entity::Entity;
use crate::value::Value;
use std::collections::HashMap;

/// Generic data object base, capturing an entity plus typed fields.
#[derive(Debug, Clone)]
pub struct DataObject {
    pub entity: Entity,
    pub fields: HashMap<String, Value>,
}

impl DataObject {
    pub fn new(entity: Entity) -> Self {
        Self {
            entity,
            fields: HashMap::new(),
        }
    }

    pub fn set(&mut self, key: impl Into<String>, value: Value) {
        self.fields.insert(key.into(), value);
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.fields.get(key)
    }
}

/// DataObject that models a Task payload.
#[derive(Debug, Clone)]
pub struct TaskDataObject {
    pub data: DataObject,
}

impl TaskDataObject {
    pub fn new(entity: Entity) -> Self {
        Self {
            data: DataObject::new(entity),
        }
    }
}

/// DataObject that models an Annotation payload.
#[derive(Debug, Clone)]
pub struct AnnotationDataObject {
    pub data: DataObject,
}

impl AnnotationDataObject {
    pub fn new(entity: Entity) -> Self {
        Self {
            data: DataObject::new(entity),
        }
    }
}
