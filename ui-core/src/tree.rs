use crate::{
    component::Component,
    geometry::{Color, Rect},
    input::{Key, UiEvent},
    paint::{PaintOp, TextStyle},
    widget::WidgetResponse,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeNode {
    pub text: String,
    pub children: Vec<TreeNode>,
}

impl TreeNode {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            children: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeControl {
    pub bounds: Rect,
    pub roots: Vec<TreeNode>,
    pub selected_path: Option<Vec<usize>>,
}

impl TreeControl {
    pub fn new(bounds: Rect) -> Self {
        Self {
            bounds,
            roots: Vec::new(),
            selected_path: None,
        }
    }

    pub fn push_root(&mut self, text: impl Into<String>) -> usize {
        self.roots.push(TreeNode::new(text));
        self.roots.len() - 1
    }

    pub fn push_child(&mut self, root_index: usize, text: impl Into<String>) -> bool {
        if let Some(root) = self.roots.get_mut(root_index) {
            root.children.push(TreeNode::new(text));
            true
        } else {
            false
        }
    }

    pub fn select_path(&mut self, path: &[usize]) -> bool {
        if self.node_at_path(path).is_some() {
            self.selected_path = Some(path.to_vec());
            true
        } else {
            false
        }
    }

    pub fn node_at_path(&self, path: &[usize]) -> Option<&TreeNode> {
        let (first, rest) = path.split_first()?;
        let mut node = self.roots.get(*first)?;
        for index in rest {
            node = node.children.get(*index)?;
        }
        Some(node)
    }

    pub fn visible_rows(&self) -> Vec<(Vec<usize>, usize, &TreeNode)> {
        let mut rows = Vec::new();
        for (root_index, root) in self.roots.iter().enumerate() {
            rows.push((vec![root_index], 0, root));
            for (child_index, child) in root.children.iter().enumerate() {
                rows.push((vec![root_index, child_index], 1, child));
            }
        }
        rows
    }

    pub fn handle_event(&mut self, event: UiEvent) -> WidgetResponse {
        let row_paths: Vec<Vec<usize>> = self
            .visible_rows()
            .into_iter()
            .map(|(path, _, _)| path)
            .collect();
        match event {
            UiEvent::PointerReleased { state, .. } => {
                if !self.bounds.contains(state.position) {
                    return WidgetResponse::default();
                }
                let row = ((state.position.y - self.bounds.y) / 28).max(0) as usize;
                if let Some(path) = row_paths.get(row) {
                    if self.select_path(path) {
                        return WidgetResponse::redraw();
                    }
                }
            }
            UiEvent::KeyPressed { key: Key::Down, .. } => {
                if row_paths.is_empty() {
                    return WidgetResponse::default();
                }
                let current = self
                    .selected_path
                    .as_ref()
                    .and_then(|path| row_paths.iter().position(|row_path| row_path == path))
                    .unwrap_or(0);
                let next = (current + 1).min(row_paths.len() - 1);
                if self.select_path(&row_paths[next]) {
                    return WidgetResponse::redraw();
                }
            }
            UiEvent::KeyPressed { key: Key::Up, .. } => {
                if row_paths.is_empty() {
                    return WidgetResponse::default();
                }
                let current = self
                    .selected_path
                    .as_ref()
                    .and_then(|path| row_paths.iter().position(|row_path| row_path == path))
                    .unwrap_or(0);
                let next = current.saturating_sub(1);
                if self.select_path(&row_paths[next]) {
                    return WidgetResponse::redraw();
                }
            }
            _ => {}
        }

        WidgetResponse::default()
    }

    pub fn paint(&self, scene: &mut Vec<PaintOp>) {
        scene.push(PaintOp::FillRect {
            rect: self.bounds,
            color: Color::rgba(0xf5, 0xf1, 0xe7, 0xff),
        });
        scene.push(PaintOp::StrokeRect {
            rect: self.bounds,
            color: Color::rgba(0x6d, 0x7d, 0x6e, 0xff),
        });
        for (row_index, (path, depth, node)) in self.visible_rows().into_iter().enumerate() {
            let row_rect = Rect {
                x: self.bounds.x,
                y: self.bounds.y + (row_index as i32 * 28),
                width: self.bounds.width,
                height: 28,
            };
            if self.selected_path.as_ref() == Some(&path) {
                scene.push(PaintOp::FillRect {
                    rect: row_rect,
                    color: Color::rgba(0xee, 0xe6, 0xc2, 0xff),
                });
            }
            scene.push(PaintOp::Text {
                rect: Rect {
                    x: row_rect.x + 12 + (depth as i32 * 18),
                    y: row_rect.y,
                    width: row_rect.width - 12,
                    height: row_rect.height,
                },
                text: node.text.clone(),
                style: TextStyle::default(),
            });
        }
    }
}

impl Component for TreeControl {
    fn bounds(&self) -> Rect {
        self.bounds
    }

    fn set_bounds(&mut self, rect: Rect) {
        self.bounds = rect;
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeCombo {
    pub bounds: Rect,
    pub tree: TreeControl,
}

impl TreeCombo {
    pub fn new(bounds: Rect) -> Self {
        Self {
            bounds,
            tree: TreeControl::new(bounds),
        }
    }

    pub fn handle_event(&mut self, event: UiEvent) -> WidgetResponse {
        self.tree.handle_event(event)
    }

    pub fn paint(&self, scene: &mut Vec<PaintOp>) {
        self.tree.paint(scene);
    }
}

impl Component for TreeCombo {
    fn bounds(&self) -> Rect {
        self.bounds
    }

    fn set_bounds(&mut self, rect: Rect) {
        self.bounds = rect;
        self.tree.set_bounds(rect);
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::{TreeCombo, TreeControl};
    use crate::{Modifiers, PointerButton, PointerState, Rect, UiEvent};

    #[test]
    fn tree_tracks_inserted_nodes_and_selection() {
        let mut tree = TreeControl::new(Rect {
            x: 0,
            y: 0,
            width: 200,
            height: 200,
        });

        let root = tree.push_root("root");
        assert!(tree.push_child(root, "child"));
        assert!(tree.select_path(&[0, 0]));
        assert_eq!(
            tree.node_at_path(&[0, 0]).map(|node| node.text.as_str()),
            Some("child")
        );
    }

    #[test]
    fn tree_combo_owns_shared_tree_model() {
        let combo = TreeCombo::new(Rect {
            x: 0,
            y: 0,
            width: 160,
            height: 26,
        });

        assert!(combo.tree.roots.is_empty());
    }

    #[test]
    fn tree_pointer_release_selects_row() {
        let mut tree = TreeControl::new(Rect {
            x: 0,
            y: 0,
            width: 200,
            height: 200,
        });
        let root = tree.push_root("root");
        assert!(tree.push_child(root, "child"));

        let response = tree.handle_event(UiEvent::PointerReleased {
            button: PointerButton::Primary,
            state: PointerState::mouse(crate::Point { x: 10, y: 35 }, Modifiers::default()),
        });

        assert!(response.request_redraw);
        assert_eq!(tree.selected_path, Some(vec![0, 0]));
    }
}
