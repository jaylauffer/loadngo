use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WidgetId(pub u64);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WidgetAction {
    Activate(WidgetId),
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WidgetResponse {
    pub request_redraw: bool,
    pub request_focus: bool,
    pub action: Option<WidgetAction>,
}

impl WidgetResponse {
    pub fn redraw() -> Self {
        Self {
            request_redraw: true,
            request_focus: false,
            action: None,
        }
    }

    pub fn activate(widget_id: WidgetId) -> Self {
        Self {
            request_redraw: true,
            request_focus: false,
            action: Some(WidgetAction::Activate(widget_id)),
        }
    }
}
