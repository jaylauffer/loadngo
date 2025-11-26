use std::sync::{Arc, Mutex};

use crate::event::ComponentEvent;
use crate::listener::ComponentListener;

/// Minimal port of ComponentEventProc: manages a listener list and dispatch.
#[derive(Default, Clone)]
pub struct ComponentEventProc {
    listeners: Arc<Mutex<Vec<Arc<dyn ComponentListener>>>>,
}

impl ComponentEventProc {
    pub fn new() -> Self {
        Self {
            listeners: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn add_listener(&self, listener: Arc<dyn ComponentListener>) {
        let mut lock = self.listeners.lock().unwrap();
        if !lock.iter().any(|l| Arc::ptr_eq(l, &listener)) {
            lock.push(listener);
        }
    }

    pub fn remove_listener(&self, listener: &Arc<dyn ComponentListener>) {
        let mut lock = self.listeners.lock().unwrap();
        lock.retain(|l| !Arc::ptr_eq(l, listener));
    }

    pub fn notify(&self, event: &ComponentEvent) {
        let snapshot = {
            let lock = self.listeners.lock().unwrap();
            lock.clone()
        };
        for listener in snapshot {
            listener.handle_event(event);
        }
    }
}
