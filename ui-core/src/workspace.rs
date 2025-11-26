use crate::{
    geometry::Rect,
    input::UiEvent,
    paint::PaintOp,
    split::{SplitAxis, SplitNodeModel},
    tabs::TabGroupModel,
    widget::WidgetResponse,
};

#[derive(Debug, Clone, PartialEq)]
pub struct WorkspaceLeaf {
    pub panel_id: u64,
    pub title: String,
    pub bounds: Rect,
}

impl WorkspaceLeaf {
    pub fn new(panel_id: u64, title: impl Into<String>) -> Self {
        Self {
            panel_id,
            title: title.into(),
            bounds: Rect::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkspaceTabPage {
    pub title: String,
    pub child: Box<WorkspaceNode>,
}

impl WorkspaceTabPage {
    pub fn new(title: impl Into<String>, child: WorkspaceNode) -> Self {
        Self {
            title: title.into(),
            child: Box::new(child),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkspaceTabGroup {
    pub tabs: TabGroupModel,
    pub pages: Vec<WorkspaceTabPage>,
}

impl WorkspaceTabGroup {
    pub fn new(bounds: Rect) -> Self {
        Self {
            tabs: TabGroupModel::new(bounds),
            pages: Vec::new(),
        }
    }

    pub fn add_page(&mut self, title: impl Into<String>, child: WorkspaceNode) {
        let title = title.into();
        self.tabs.add_page(title.clone(), None);
        self.pages.push(WorkspaceTabPage::new(title, child));
    }

    pub fn content_rect(&self) -> Rect {
        self.tabs.content_rect()
    }

    fn set_bounds(&mut self, bounds: Rect) {
        self.tabs.bounds = bounds;
        let content = self.tabs.content_rect();
        for page in &mut self.pages {
            page.child.set_bounds(content);
        }
    }

    fn handle_event(&mut self, event: UiEvent) -> WidgetResponse {
        let mut response = self.tabs.handle_event(event.clone());
        if !response.input_consumed {
            if let Some(page) = self.pages.get_mut(self.tabs.selected) {
                response = merge_response(response, page.child.handle_event(event));
            }
        }
        response
    }

    fn paint_chrome(&self, scene: &mut Vec<PaintOp>) {
        self.tabs.paint(scene);
        if let Some(page) = self.pages.get(self.tabs.selected) {
            page.child.paint_chrome(scene);
        }
    }

    fn collect_visible_leaves(&self, leaves: &mut Vec<WorkspaceLeafView>) {
        if let Some(page) = self.pages.get(self.tabs.selected) {
            page.child.collect_visible_leaves(leaves);
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkspaceSplitNode {
    pub split: SplitNodeModel,
    pub first: Box<WorkspaceNode>,
    pub second: Box<WorkspaceNode>,
}

impl WorkspaceSplitNode {
    pub fn new(axis: SplitAxis, bounds: Rect, first: WorkspaceNode, second: WorkspaceNode) -> Self {
        Self {
            split: SplitNodeModel::new(axis, bounds),
            first: Box::new(first),
            second: Box::new(second),
        }
    }

    fn set_bounds(&mut self, bounds: Rect) {
        self.split.set_bounds(bounds);
        self.first.set_bounds(self.split.first_rect());
        self.second.set_bounds(self.split.second_rect());
    }

    fn handle_event(&mut self, event: UiEvent) -> WidgetResponse {
        let response = self.split.handle_event(event.clone());
        if response.input_consumed {
            self.first.set_bounds(self.split.first_rect());
            self.second.set_bounds(self.split.second_rect());
            return response;
        }

        let left = self.first.handle_event(event.clone());
        let right = self.second.handle_event(event);
        merge_response(response, merge_response(left, right))
    }

    fn paint_chrome(&self, scene: &mut Vec<PaintOp>) {
        self.first.paint_chrome(scene);
        self.second.paint_chrome(scene);
        self.split.paint(scene);
    }

    fn collect_visible_leaves(&self, leaves: &mut Vec<WorkspaceLeafView>) {
        self.first.collect_visible_leaves(leaves);
        self.second.collect_visible_leaves(leaves);
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum WorkspaceNode {
    Split(WorkspaceSplitNode),
    Tabs(WorkspaceTabGroup),
    Leaf(WorkspaceLeaf),
}

impl WorkspaceNode {
    pub fn leaf(panel_id: u64, title: impl Into<String>) -> Self {
        Self::Leaf(WorkspaceLeaf::new(panel_id, title))
    }

    pub fn set_bounds(&mut self, bounds: Rect) {
        match self {
            WorkspaceNode::Split(split) => split.set_bounds(bounds),
            WorkspaceNode::Tabs(tabs) => tabs.set_bounds(bounds),
            WorkspaceNode::Leaf(leaf) => leaf.bounds = bounds,
        }
    }

    pub fn bounds(&self) -> Rect {
        match self {
            WorkspaceNode::Split(split) => split.split.bounds,
            WorkspaceNode::Tabs(tabs) => tabs.tabs.bounds,
            WorkspaceNode::Leaf(leaf) => leaf.bounds,
        }
    }

    pub fn handle_event(&mut self, event: UiEvent) -> WidgetResponse {
        match self {
            WorkspaceNode::Split(split) => split.handle_event(event),
            WorkspaceNode::Tabs(tabs) => tabs.handle_event(event),
            WorkspaceNode::Leaf(_) => WidgetResponse::default(),
        }
    }

    pub fn paint_chrome(&self, scene: &mut Vec<PaintOp>) {
        match self {
            WorkspaceNode::Split(split) => split.paint_chrome(scene),
            WorkspaceNode::Tabs(tabs) => tabs.paint_chrome(scene),
            WorkspaceNode::Leaf(_) => {}
        }
    }

    pub fn visible_leaves(&self) -> Vec<WorkspaceLeafView> {
        let mut leaves = Vec::new();
        self.collect_visible_leaves(&mut leaves);
        leaves
    }

    fn collect_visible_leaves(&self, leaves: &mut Vec<WorkspaceLeafView>) {
        match self {
            WorkspaceNode::Split(split) => split.collect_visible_leaves(leaves),
            WorkspaceNode::Tabs(tabs) => tabs.collect_visible_leaves(leaves),
            WorkspaceNode::Leaf(leaf) => leaves.push(WorkspaceLeafView {
                panel_id: leaf.panel_id,
                title: leaf.title.clone(),
                bounds: leaf.bounds,
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkspaceLeafView {
    pub panel_id: u64,
    pub title: String,
    pub bounds: Rect,
}

fn merge_response(mut base: WidgetResponse, next: WidgetResponse) -> WidgetResponse {
    base.request_redraw |= next.request_redraw;
    base.request_focus |= next.request_focus;
    base.input_consumed |= next.input_consumed;
    if base.action.is_none() {
        base.action = next.action;
    }
    base
}

#[cfg(test)]
mod tests {
    use super::{WorkspaceLeafView, WorkspaceNode, WorkspaceSplitNode, WorkspaceTabGroup};
    use crate::{
        input::{Modifiers, PointerButton, PointerState, UiEvent},
        Point, Rect, SplitAxis,
    };

    fn pointer(x: f32, y: f32) -> PointerState {
        PointerState::mouse(Point { x, y }, Modifiers::default())
    }

    #[test]
    fn workspace_split_propagates_child_bounds() {
        let mut root = WorkspaceNode::Split(WorkspaceSplitNode::new(
            SplitAxis::Horizontal,
            Rect::default(),
            WorkspaceNode::leaf(1, "left"),
            WorkspaceNode::leaf(2, "right"),
        ));
        root.set_bounds(Rect {
            x: 10.0,
            y: 20.0,
            width: 500.0,
            height: 300.0,
        });

        let leaves = root.visible_leaves();
        assert_eq!(leaves.len(), 2);
        assert!(leaves[0].bounds.width > 0.0);
        assert!(leaves[1].bounds.width > 0.0);
        assert!(leaves[0].bounds.right() <= leaves[1].bounds.x);
    }

    #[test]
    fn workspace_tab_group_only_exposes_selected_leaf() {
        let mut tabs = WorkspaceTabGroup::new(Rect::default());
        tabs.add_page("One", WorkspaceNode::leaf(1, "one"));
        tabs.add_page("Two", WorkspaceNode::leaf(2, "two"));
        let mut root = WorkspaceNode::Tabs(tabs);
        root.set_bounds(Rect {
            x: 0.0,
            y: 0.0,
            width: 320.0,
            height: 200.0,
        });

        assert_eq!(
            root.visible_leaves()
                .iter()
                .map(|leaf| leaf.panel_id)
                .collect::<Vec<_>>(),
            vec![1]
        );

        let response = root.handle_event(UiEvent::PointerReleased {
            button: PointerButton::Primary,
            state: pointer(250.0, 10.0),
        });
        assert!(response.request_redraw);
        assert_eq!(
            root.visible_leaves()
                .iter()
                .map(|leaf| leaf.panel_id)
                .collect::<Vec<_>>(),
            vec![2]
        );
    }

    #[test]
    fn workspace_split_drag_updates_leaf_bounds() {
        let mut root = WorkspaceNode::Split(WorkspaceSplitNode::new(
            SplitAxis::Horizontal,
            Rect::default(),
            WorkspaceNode::leaf(1, "left"),
            WorkspaceNode::leaf(2, "right"),
        ));
        root.set_bounds(Rect {
            x: 0.0,
            y: 0.0,
            width: 600.0,
            height: 320.0,
        });
        let initial = root.visible_leaves();
        let handle_x = initial[0].bounds.right() + 4.0;

        let _ = root.handle_event(UiEvent::PointerPressed {
            button: PointerButton::Primary,
            state: pointer(handle_x, 40.0),
        });
        let moved = root.handle_event(UiEvent::PointerMoved(pointer(280.0, 40.0)));
        let _ = root.handle_event(UiEvent::PointerReleased {
            button: PointerButton::Primary,
            state: pointer(280.0, 40.0),
        });

        let after = root.visible_leaves();
        assert!(moved.input_consumed);
        assert_ne!(initial[0].bounds.width, after[0].bounds.width);
    }

    #[test]
    fn workspace_tab_group_sets_leaf_to_content_rect() {
        let mut tabs = WorkspaceTabGroup::new(Rect::default());
        tabs.add_page("One", WorkspaceNode::leaf(1, "one"));
        let mut root = WorkspaceNode::Tabs(tabs);
        root.set_bounds(Rect {
            x: 20.0,
            y: 30.0,
            width: 400.0,
            height: 260.0,
        });

        assert_eq!(
            root.visible_leaves(),
            vec![WorkspaceLeafView {
                panel_id: 1,
                title: "one".to_string(),
                bounds: Rect {
                    x: 20.0,
                    y: 62.0,
                    width: 400.0,
                    height: 228.0,
                },
            }]
        );
    }
}
