use std::time::Duration;

use loadngo_host_core::{FrameDemand, HostKey, WindowDescriptor};
use ui_core::{
    ButtonModel, Color, HorizontalAlign, Insets, LabelModel, PanelModel, Point, PointerButton,
    PointerState, Rect, SplitAxis, TextBlockModel, TextOverflow, UiEvent, VerticalAlign,
    WorkspaceLeafView, WorkspaceNode, WorkspaceSplitNode, WorkspaceTabGroup,
};

const WINDOW_WIDTH: i32 = 1480;
const WINDOW_HEIGHT: i32 = 940;
const OUTER_GUTTER: f32 = 22.0;
const PANEL_INSET: Insets = Insets {
    left: 16.0,
    top: 14.0,
    right: 16.0,
    bottom: 14.0,
};
const TITLE_FONT: u16 = 22;
const BODY_FONT: u16 = 18;
const CAPTION_FONT: u16 = 15;

const PANEL_ASSETS: u64 = 1;
const PANEL_OUTLINE: u64 = 2;
const PANEL_PREVIEW: u64 = 3;
const PANEL_SELECTION: u64 = 4;
const PANEL_VALIDATION: u64 = 5;
const PANEL_DICTATION: u64 = 6;

fn main() {
    loadngo_host_desktop::launch(window_descriptor(), None, async move {
        run_workspace_harness().await;
    });
}

fn window_descriptor() -> WindowDescriptor {
    WindowDescriptor {
        title: "loadngo workspace harness".to_string(),
        width: Some(WINDOW_WIDTH),
        height: Some(WINDOW_HEIGHT),
        high_dpi: true,
        linux_wm_class: Some("loadngo-workspace-harness"),
    }
}

fn build_workspace() -> WorkspaceNode {
    let mut left_tabs = WorkspaceTabGroup::new(Rect::default());
    left_tabs.add_page("Assets", WorkspaceNode::leaf(PANEL_ASSETS, "Assets"));
    left_tabs.add_page("Outline", WorkspaceNode::leaf(PANEL_OUTLINE, "Outline"));

    let mut inspector_tabs = WorkspaceTabGroup::new(Rect::default());
    inspector_tabs.add_page(
        "Selection",
        WorkspaceNode::leaf(PANEL_SELECTION, "Selection"),
    );
    inspector_tabs.add_page(
        "Validation",
        WorkspaceNode::leaf(PANEL_VALIDATION, "Validation"),
    );
    inspector_tabs.add_page(
        "Dictation",
        WorkspaceNode::leaf(PANEL_DICTATION, "Dictation"),
    );

    let mut right_split = WorkspaceSplitNode::new(
        SplitAxis::Horizontal,
        Rect::default(),
        WorkspaceNode::leaf(PANEL_PREVIEW, "Preview"),
        WorkspaceNode::Tabs(inspector_tabs),
    );
    right_split.split.split_ratio = 0.64;
    right_split.split.min_first = 420.0;
    right_split.split.min_second = 280.0;
    right_split.split.handle_size = 14.0;
    right_split.split.hit_size = 24.0;

    let mut root = WorkspaceSplitNode::new(
        SplitAxis::Horizontal,
        Rect::default(),
        WorkspaceNode::Tabs(left_tabs),
        WorkspaceNode::Split(right_split),
    );
    root.split.split_ratio = 0.26;
    root.split.min_first = 240.0;
    root.split.min_second = 720.0;
    root.split.handle_size = 16.0;
    root.split.hit_size = 26.0;

    WorkspaceNode::Split(root)
}

async fn run_workspace_harness() {
    let mut workspace = build_workspace();

    loop {
        let frame = loadngo_host_desktop::capture_frame();
        if frame.input.key_pressed(HostKey::Escape) {
            break;
        }

        let outer = Rect {
            x: OUTER_GUTTER,
            y: OUTER_GUTTER,
            width: (frame.surface.width - OUTER_GUTTER * 2.0).max(640.0),
            height: (frame.surface.height - OUTER_GUTTER * 2.0).max(480.0),
        };
        workspace.set_bounds(outer);
        route_pointer(&mut workspace, &frame.input);

        let mut ops = Vec::new();
        workspace.paint_chrome(&mut ops);
        for leaf in workspace.visible_leaves() {
            paint_leaf(&mut ops, &leaf);
        }

        loadngo_host_desktop::clear(Color::rgba(0x0f, 0x13, 0x1c, 0xff));
        loadngo_host_desktop::render_widget_paint_ops(&ops);
        loadngo_host_desktop::next_frame(FrameDemand::after(Duration::from_millis(16))).await;
    }
}

fn route_pointer(workspace: &mut WorkspaceNode, input: &loadngo_host_core::InputSnapshot) {
    let pointer = PointerState::mouse(
        Point {
            x: input.mouse_x,
            y: input.mouse_y,
        },
        ui_core::Modifiers::default(),
    );
    let _ = workspace.handle_event(UiEvent::PointerMoved(pointer));
    if input.mouse_pressed {
        let _ = workspace.handle_event(UiEvent::PointerPressed {
            button: PointerButton::Primary,
            state: pointer,
        });
    }
    if input.mouse_released {
        let _ = workspace.handle_event(UiEvent::PointerReleased {
            button: PointerButton::Primary,
            state: pointer,
        });
    }
}

fn paint_leaf(scene: &mut Vec<ui_core::PaintOp>, leaf: &WorkspaceLeafView) {
    let mut panel = PanelModel::new(leaf.bounds);
    panel.background = Some(Color::rgba(0x1a, 0x21, 0x2e, 0xf2));
    panel.border = Some(Color::rgba(0x78, 0x86, 0xa0, 0xff));
    panel.padding = PANEL_INSET;
    panel.paint(scene);

    let content = panel.content_rect();
    let title_height = ui_core::single_line_text_box_height(TITLE_FONT);
    let mut title = LabelModel::new(
        leaf.title.clone(),
        Rect {
            x: content.x,
            y: content.y,
            width: content.width,
            height: title_height,
        },
    );
    title.style.font_size = TITLE_FONT;
    title.style.color = Color::rgba(0xf5, 0xf7, 0xfb, 0xff);
    title.style.vertical_align = VerticalAlign::Middle;
    title.style.overflow = TextOverflow::EllipsisEnd;
    title.paint(scene);

    let body = Rect {
        x: content.x,
        y: content.y + title_height + 10.0,
        width: content.width,
        height: (content.height - title_height - 10.0).max(0.0),
    };

    match leaf.panel_id {
        PANEL_ASSETS => paint_assets_leaf(scene, body),
        PANEL_OUTLINE => paint_outline_leaf(scene, body),
        PANEL_PREVIEW => paint_preview_leaf(scene, body),
        PANEL_SELECTION => paint_selection_leaf(scene, body),
        PANEL_VALIDATION => paint_validation_leaf(scene, body),
        PANEL_DICTATION => paint_dictation_leaf(scene, body),
        _ => {}
    }
}

fn paint_assets_leaf(scene: &mut Vec<ui_core::PaintOp>, body: Rect) {
    let mut note = TextBlockModel::new(
        "Workspace harness\n\n\
Drag the splitters.\n\
Click tabs.\n\
Use this surface to verify workspace layout behavior independently from the editor.",
        body,
    );
    note.style.font_size = BODY_FONT;
    note.style.color = Color::rgba(0xe6, 0xeb, 0xf6, 0xff);
    note.paint(scene);
}

fn paint_outline_leaf(scene: &mut Vec<ui_core::PaintOp>, body: Rect) {
    let mut block = TextBlockModel::new(
        "Visible tree:\n\
- left tab group\n\
- preview leaf\n\
- inspector tab group\n\n\
This page exists to exercise tab switching while the surrounding split layout stays stable.",
        body,
    );
    block.style.font_size = BODY_FONT;
    block.style.color = Color::rgba(0xd6, 0xe6, 0xff, 0xff);
    block.paint(scene);
}

fn paint_preview_leaf(scene: &mut Vec<ui_core::PaintOp>, body: Rect) {
    let hero = Rect {
        x: body.x,
        y: body.y,
        width: body.width,
        height: (body.height * 0.56).max(140.0),
    };
    let mut hero_panel = PanelModel::new(hero);
    hero_panel.background = Some(Color::rgba(0x23, 0x2a, 0x3a, 0xff));
    hero_panel.border = Some(Color::rgba(0x96, 0xa7, 0xc5, 0xff));
    hero_panel.paint(scene);

    let mut hero_label = LabelModel::new(
        "Preview Pane",
        Rect {
            x: hero.x,
            y: hero.y + hero.height * 0.5 - 18.0,
            width: hero.width,
            height: ui_core::single_line_text_box_height(TITLE_FONT),
        },
    );
    hero_label.style.font_size = TITLE_FONT;
    hero_label.style.horizontal_align = HorizontalAlign::Center;
    hero_label.style.vertical_align = VerticalAlign::Middle;
    hero_label.style.color = Color::rgba(0xf6, 0xf8, 0xfc, 0xff);
    hero_label.paint(scene);

    let mut note = TextBlockModel::new(
        "The preview leaf is plain app-owned content.\n\
The workspace model only owns split/tab layout, input, and chrome.",
        Rect {
            x: body.x,
            y: hero.y + hero.height + 14.0,
            width: body.width,
            height: (body.height - hero.height - 14.0).max(0.0),
        },
    );
    note.style.font_size = CAPTION_FONT;
    note.style.color = Color::rgba(0xcc, 0xd7, 0xea, 0xff);
    note.paint(scene);
}

fn paint_selection_leaf(scene: &mut Vec<ui_core::PaintOp>, body: Rect) {
    let mut block = TextBlockModel::new(
        "Selection tab\n\n\
Use tabs for related tool panes that should share a region while preserving the larger workspace split tree.",
        body,
    );
    block.style.font_size = BODY_FONT;
    block.style.color = Color::rgba(0xe8, 0xec, 0xf7, 0xff);
    block.paint(scene);
}

fn paint_validation_leaf(scene: &mut Vec<ui_core::PaintOp>, body: Rect) {
    let mut block = TextBlockModel::new(
        "Validation tab\n\n\
This is where editor checks, warnings, and derived story-map diagnostics would fit naturally.",
        body,
    );
    block.style.font_size = BODY_FONT;
    block.style.color = Color::rgba(0xd7, 0xf0, 0xd6, 0xff);
    block.paint(scene);
}

fn paint_dictation_leaf(scene: &mut Vec<ui_core::PaintOp>, body: Rect) {
    let button_rect = Rect {
        x: body.x,
        y: body.bottom() - 52.0,
        width: body.width,
        height: 44.0,
    };
    let button = ButtonModel::new("Hold To Talk", button_rect);
    button.paint(scene);

    let mut block = TextBlockModel::new(
        "Dictation tab\n\n\
This demonstrates a tab page hosting normal widget content inside the workspace content rect.",
        Rect {
            x: body.x,
            y: body.y,
            width: body.width,
            height: (body.height - 62.0).max(0.0),
        },
    );
    block.style.font_size = BODY_FONT;
    block.style.color = Color::rgba(0xf2, 0xe6, 0xb4, 0xff);
    block.paint(scene);
}

#[cfg(test)]
mod tests {
    use super::{build_workspace, WINDOW_HEIGHT, WINDOW_WIDTH};
    use ui_core::Rect;

    #[test]
    fn workspace_harness_exposes_expected_visible_leaves() {
        let mut workspace = build_workspace();
        workspace.set_bounds(Rect {
            x: 22.0,
            y: 22.0,
            width: WINDOW_WIDTH as f32 - 44.0,
            height: WINDOW_HEIGHT as f32 - 44.0,
        });

        let leaves = workspace.visible_leaves();
        assert_eq!(leaves.len(), 3);
        assert!(leaves.iter().any(|leaf| leaf.title == "Assets"));
        assert!(leaves.iter().any(|leaf| leaf.title == "Preview"));
        assert!(leaves.iter().any(|leaf| leaf.title == "Selection"));
    }
}
