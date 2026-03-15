use ui_core::component::Component;

/// Lightweight event used by components to notify listeners about actions.
#[derive(Debug, Clone)]
pub struct ComponentEvent {
    /// Numeric code describing the event (aligns with legacy `m_code`).
    pub code: i32,
    /// Optional component identifier.
    pub component_id: i32,
}

impl ComponentEvent {
    pub fn new(component: &dyn Component, code: i32) -> Self {
        Self {
            code,
            component_id: component.id(),
        }
    }
}
