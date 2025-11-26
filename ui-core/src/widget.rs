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
    pub input_consumed: bool,
    pub action: Option<WidgetAction>,
}

impl WidgetResponse {
    pub fn redraw() -> Self {
        Self {
            request_redraw: true,
            request_focus: false,
            input_consumed: false,
            action: None,
        }
    }

    pub fn redraw_consumed() -> Self {
        Self {
            request_redraw: true,
            request_focus: false,
            input_consumed: true,
            action: None,
        }
    }

    pub fn activate(widget_id: WidgetId) -> Self {
        Self {
            request_redraw: true,
            request_focus: false,
            input_consumed: true,
            action: Some(WidgetAction::Activate(widget_id)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{WidgetAction, WidgetId, WidgetResponse};

    #[test]
    fn redraw_response_requests_redraw_without_consuming_input() {
        let response = WidgetResponse::redraw();
        assert!(response.request_redraw);
        assert!(!response.request_focus);
        assert!(!response.input_consumed);
        assert_eq!(response.action, None);
    }

    #[test]
    fn redraw_consumed_response_consumes_input_without_action() {
        let response = WidgetResponse::redraw_consumed();
        assert!(response.request_redraw);
        assert!(!response.request_focus);
        assert!(response.input_consumed);
        assert_eq!(response.action, None);
    }

    #[test]
    fn activate_response_consumes_input_and_emits_action() {
        let response = WidgetResponse::activate(WidgetId(42));
        assert!(response.request_redraw);
        assert!(!response.request_focus);
        assert!(response.input_consumed);
        assert_eq!(response.action, Some(WidgetAction::Activate(WidgetId(42))));
    }
}
