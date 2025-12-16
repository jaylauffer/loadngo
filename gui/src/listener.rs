use crate::event::ComponentEvent;

/// Receives component events (clicks, focus changes, etc.).
pub trait ComponentListener: Send + Sync {
    fn handle_event(&self, event: &ComponentEvent);
}
