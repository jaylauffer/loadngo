use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WidgetId(pub u64);

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WidgetResponse {
    pub request_redraw: bool,
    pub request_focus: bool,
    pub command: Option<i32>,
}

impl WidgetResponse {
    pub fn redraw() -> Self {
        Self {
            request_redraw: true,
            request_focus: false,
            command: None,
        }
    }

    pub fn command(command: i32) -> Self {
        Self {
            request_redraw: true,
            request_focus: false,
            command: Some(command),
        }
    }
}
