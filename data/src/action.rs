//! Action scaffolding (placeholder for future command implementations).

/// A simple action trait to model operations that can be executed.
pub trait Action {
    fn name(&self) -> &'static str;
    fn execute(&mut self);
}

/// No-op action useful for testing.
#[derive(Debug, Default, Clone)]
pub struct NoopAction;

impl Action for NoopAction {
    fn name(&self) -> &'static str {
        "noop"
    }
    fn execute(&mut self) {}
}
