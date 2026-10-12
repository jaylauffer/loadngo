//! Lays out a project as the Win32 Task's hierarchy view draws it: the
//! project at the top, each tier of subtasks below, every task centred over
//! its own subtasks, joined by lines.

use std::collections::HashSet;

use data::types::Id;
use ui_core::Rect;

use crate::plan::Plan;

pub const NODE_HEIGHT: f32 = 30.0;
const TIER_GAP: f32 = 40.0;
const SIBLING_GAP: f32 = 16.0;
const MIN_WIDTH: f32 = 70.0;
const MAX_WIDTH: f32 = 220.0;
const TEXT_PADDING: f32 = 28.0;
/// Room for a priority mark before the name.
pub const PRIORITY_MARK: f32 = 18.0;

#[derive(Debug, Clone, PartialEq)]
pub struct NodeBox {
    pub id: Id,
    /// Relative to the layout's top left.
    pub rect: Rect,
    pub depth: usize,
    /// Index of the parent's box.
    pub parent: Option<usize>,
    pub has_children: bool,
    pub collapsed: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TreeLayout {
    pub nodes: Vec<NodeBox>,
    pub width: f32,
    pub height: f32,
}

struct Subtree {
    id: Id,
    node_width: f32,
    width: f32,
    has_children: bool,
    collapsed: bool,
    children: Vec<Subtree>,
}

/// Lays out `root` and the subtasks of every task not in `collapsed`.
/// `measure` gives a task name's width.
pub fn layout(
    plan: &Plan,
    root: Id,
    collapsed: &HashSet<Id>,
    measure: &mut dyn FnMut(&str) -> f32,
) -> TreeLayout {
    let Some(tree) = build(plan, root, collapsed, measure, &mut HashSet::new()) else {
        return TreeLayout::default();
    };
    let mut layout = TreeLayout {
        width: tree.width,
        ..TreeLayout::default()
    };
    place(&tree, 0.0, 0, None, &mut layout);
    layout.height = layout
        .nodes
        .iter()
        .map(|node| node.rect.bottom())
        .fold(0.0, f32::max);
    layout
}

fn build(
    plan: &Plan,
    id: Id,
    collapsed: &HashSet<Id>,
    measure: &mut dyn FnMut(&str) -> f32,
    seen: &mut HashSet<Id>,
) -> Option<Subtree> {
    // A parent loop in a loaded file must not recurse for ever.
    if !seen.insert(id) {
        return None;
    }
    let task = plan.task(id)?;
    let mark = if task.priority > 0 {
        PRIORITY_MARK
    } else {
        0.0
    };
    let node_width = (measure(&task.name) + TEXT_PADDING + mark).clamp(MIN_WIDTH, MAX_WIDTH);
    let child_ids: Vec<Id> = plan.children(id).iter().map(|t| t.entity.id).collect();
    let has_children = !child_ids.is_empty();
    let is_collapsed = collapsed.contains(&id);
    let children: Vec<Subtree> = if is_collapsed {
        Vec::new()
    } else {
        child_ids
            .into_iter()
            .filter_map(|child| build(plan, child, collapsed, measure, seen))
            .collect()
    };
    let row: f32 = children.iter().map(|child| child.width).sum::<f32>()
        + SIBLING_GAP * children.len().saturating_sub(1) as f32;
    Some(Subtree {
        id,
        node_width,
        width: node_width.max(row),
        has_children,
        collapsed: is_collapsed,
        children,
    })
}

/// Places `tree` in the span starting at `left`: its subtasks first, then
/// the task centred between its first and last subtask (or in its span when
/// it has none shown).
fn place(tree: &Subtree, left: f32, depth: usize, parent: Option<usize>, out: &mut TreeLayout) {
    let y = depth as f32 * (NODE_HEIGHT + TIER_GAP);
    let index = out.nodes.len();
    out.nodes.push(NodeBox {
        id: tree.id,
        rect: Rect {
            x: left + (tree.width - tree.node_width) / 2.0,
            y,
            width: tree.node_width,
            height: NODE_HEIGHT,
        },
        depth,
        parent,
        has_children: tree.has_children,
        collapsed: tree.collapsed,
    });
    let row: f32 = tree.children.iter().map(|child| child.width).sum::<f32>()
        + SIBLING_GAP * tree.children.len().saturating_sub(1) as f32;
    let mut child_left = left + (tree.width - row) / 2.0;
    let mut centres: Option<(f32, f32)> = None;
    for child in &tree.children {
        let child_index = out.nodes.len();
        place(child, child_left, depth + 1, Some(index), out);
        let r = out.nodes[child_index].rect;
        let centre = r.x + r.width / 2.0;
        centres = Some((centres.map_or(centre, |(first, _)| first), centre));
        child_left += child.width + SIBLING_GAP;
    }
    if let Some((first, last)) = centres {
        let x = (first + last) / 2.0 - tree.node_width / 2.0;
        // Stay inside the span, which a wide box may fill.
        out.nodes[index].rect.x = x.clamp(left, left + tree.width - tree.node_width);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed(text: &str) -> f32 {
        text.chars().count() as f32 * 7.0
    }

    #[test]
    fn parents_are_centred_over_their_subtasks() {
        let mut plan = Plan::new("me");
        let root = plan.add_task("Project", None);
        let a = plan.add_task("Alpha", Some(root));
        let b = plan.add_task("Beta", Some(root));
        plan.add_task("Alpha one", Some(a));
        plan.add_task("Alpha two", Some(a));
        let layout = layout(&plan, root, &HashSet::new(), &mut fixed);
        assert_eq!(layout.nodes.len(), 5);
        let centre = |id: Id| {
            let node = layout.nodes.iter().find(|n| n.id == id).unwrap();
            node.rect.x + node.rect.width / 2.0
        };
        let middle = |nodes: Vec<&NodeBox>| {
            let c = |n: &NodeBox| n.rect.x + n.rect.width / 2.0;
            (c(nodes[0]) + c(nodes[nodes.len() - 1])) / 2.0
        };
        let kids: Vec<&NodeBox> = layout.nodes.iter().filter(|n| n.depth == 1).collect();
        assert!((centre(root) - middle(kids)).abs() < 0.01);
        let grandkids: Vec<&NodeBox> = layout.nodes.iter().filter(|n| n.depth == 2).collect();
        assert!((centre(a) - middle(grandkids)).abs() < 0.01);
        assert!(centre(b) > centre(a));
        // Nothing overlaps on a tier.
        for tier in 0..3 {
            let mut row: Vec<&NodeBox> = layout.nodes.iter().filter(|n| n.depth == tier).collect();
            row.sort_by(|x, y| x.rect.x.total_cmp(&y.rect.x));
            for pair in row.windows(2) {
                assert!(pair[0].rect.right() <= pair[1].rect.x);
            }
        }
        assert_eq!(layout.height, 2.0 * (NODE_HEIGHT + TIER_GAP) + NODE_HEIGHT);
    }

    #[test]
    fn collapsed_tasks_hide_their_subtasks() {
        let mut plan = Plan::new("me");
        let root = plan.add_task("Project", None);
        let a = plan.add_task("Alpha", Some(root));
        plan.add_task("Alpha one", Some(a));
        let collapsed: HashSet<Id> = [a].into_iter().collect();
        let layout = layout(&plan, root, &collapsed, &mut fixed);
        assert_eq!(layout.nodes.len(), 2);
        let alpha = &layout.nodes[1];
        assert!(alpha.has_children && alpha.collapsed);
        assert_eq!(alpha.parent, Some(0));
    }
}
