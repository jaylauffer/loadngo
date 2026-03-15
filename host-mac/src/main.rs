#[cfg(target_os = "macos")]
use loadngo_host_core::ImageRegistry;
#[cfg(target_os = "macos")]
use macroquad::prelude::*;
#[cfg(target_os = "macos")]
use std::collections::HashMap;
#[cfg(target_os = "macos")]
use ui_core::{
    bitmap::BitmapModel,
    button::ButtonModel,
    combo::ListCombo,
    component::Component,
    geometry::{Color as UiColor, Rect},
    input::{Key, Modifiers, PointerButton, PointerState, UiEvent},
    list::{ListInteraction, ListState},
    paint::{PaintOp, TextStyle},
    tabs::TabbedContainer,
    tree::TreeControl,
};

#[cfg(target_os = "macos")]
const FONT_SIZE: f32 = 22.0;
#[cfg(target_os = "macos")]
const LIST_ROW_HEIGHT: i32 = 34;

#[cfg(target_os = "macos")]
#[derive(Clone, Copy, PartialEq, Eq)]
enum FocusTarget {
    Button,
    Tabs,
    Combo,
    Tree,
    List,
}

#[cfg(target_os = "macos")]
struct Layout {
    list_rect: Rect,
    button_rect: Rect,
    tabs_rect: Rect,
    combo_rect: Rect,
    tree_rect: Rect,
}

#[cfg(target_os = "macos")]
struct TextureRegistry {
    textures: HashMap<String, Texture2D>,
    images: ImageRegistry,
}

#[cfg(target_os = "macos")]
impl TextureRegistry {
    fn new() -> Self {
        Self {
            textures: HashMap::new(),
            images: ImageRegistry::new(),
        }
    }

    fn texture_for_key(&mut self, image_key: &str) -> Option<Texture2D> {
        if let Some(texture) = self.textures.get(image_key) {
            return Some(texture.clone());
        }

        let decoded = self
            .images
            .load_path(image_key.to_string(), std::path::Path::new(image_key))
            .ok()?;
        if decoded.width > u16::MAX as u32 || decoded.height > u16::MAX as u32 {
            return None;
        }

        let texture =
            Texture2D::from_rgba8(decoded.width as u16, decoded.height as u16, &decoded.rgba8);
        texture.set_filter(FilterMode::Linear);
        self.textures.insert(image_key.to_string(), texture.clone());
        Some(texture)
    }
}

#[cfg(target_os = "macos")]
struct MacHostApp {
    image: BitmapModel,
    button: ButtonModel,
    list_state: ListState,
    list_items: Vec<String>,
    tabs: TabbedContainer,
    combo: ListCombo,
    tree: TreeControl,
    event_log: Vec<String>,
    focus: FocusTarget,
    images: TextureRegistry,
}

#[cfg(target_os = "macos")]
impl MacHostApp {
    fn new() -> Self {
        let list_items = vec![
            "Geometry".to_string(),
            "Pointer events".to_string(),
            "Keyboard events".to_string(),
            "Paint ops".to_string(),
            "List selection".to_string(),
            "Cross-platform seams".to_string(),
        ];
        let mut list_state = ListState::default();
        list_state.set_item_heights(vec![LIST_ROW_HEIGHT; list_items.len()]);

        let mut tabs = TabbedContainer::new(Rect {
            x: 0,
            y: 0,
            width: 320,
            height: 32,
        });
        tabs.add_page("Widgets", Some(1));
        tabs.add_page("Events", Some(2));
        tabs.add_page("Backend", Some(3));

        let mut combo = ListCombo::new(Rect {
            x: 0,
            y: 0,
            width: 260,
            height: 34,
        });
        combo.add_item("Cycle selection");
        combo.add_item("Keyboard navigation");
        combo.add_item("Scene rendering");

        let mut tree = TreeControl::new(Rect {
            x: 0,
            y: 0,
            width: 320,
            height: 220,
        });
        let widgets = tree.push_root("Shared widgets");
        let _ = tree.push_child(widgets, "Button");
        let _ = tree.push_child(widgets, "Combo");
        let backend = tree.push_root("Backends");
        let _ = tree.push_child(backend, "Win32 shim");
        let _ = tree.push_child(backend, "macOS host");

        Self {
            image: BitmapModel::new(
                "Outline/Web/20080626-loadngo-outline-logo.jpg",
                Rect {
                    x: 0,
                    y: 0,
                    width: 220,
                    height: 140,
                },
            ),
            button: ButtonModel::new(
                "Acknowledge primitive",
                Rect {
                    x: 0,
                    y: 0,
                    width: 200,
                    height: 44,
                },
            ),
            list_state,
            list_items,
            tabs,
            combo,
            tree,
            event_log: vec!["macOS host backend online".to_string()],
            focus: FocusTarget::Button,
            images: TextureRegistry::new(),
        }
    }

    fn update_layout(&mut self) -> Layout {
        let width = screen_width() as i32;
        let height = screen_height() as i32;
        let list_rect = Rect {
            x: 24,
            y: 24,
            width: 280,
            height: height - 48,
        };
        let right_x = list_rect.right() + 24;
        let right_width = (width - right_x - 24).max(320);
        let button_rect = Rect {
            x: right_x,
            y: 24,
            width: right_width,
            height: 44,
        };
        let image_rect = Rect {
            x: right_x,
            y: 88,
            width: 240,
            height: 150,
        };
        let tabs_rect = Rect {
            x: right_x,
            y: 258,
            width: right_width,
            height: 32,
        };
        let combo_rect = Rect {
            x: right_x,
            y: 308,
            width: 280,
            height: 34,
        };
        let tree_rect = Rect {
            x: right_x,
            y: 358,
            width: right_width,
            height: 180,
        };

        self.button.set_bounds(button_rect);
        self.image.set_bounds(image_rect);
        self.tabs.set_bounds(tabs_rect);
        self.combo.set_bounds(combo_rect);
        self.tree.set_bounds(tree_rect);
        self.list_state.layout(list_rect, 0);

        Layout {
            list_rect,
            button_rect,
            tabs_rect,
            combo_rect,
            tree_rect,
        }
    }

    fn process_input(&mut self, layout: &Layout) {
        let (mx, my) = mouse_position();
        let pointer = PointerState::mouse(
            ui_core::geometry::Point {
                x: mx as i32,
                y: my as i32,
            },
            Modifiers::default(),
        );
        let has_touch_input = self.process_touch_input(layout);

        if has_touch_input {
            self.finish_event_log();
            return;
        }

        let _ = self.button.handle_event(UiEvent::PointerMoved(pointer));
        let local_list_pointer = PointerState::new(
            pointer.id,
            ui_core::geometry::Point {
                x: pointer.position.x - layout.list_rect.x,
                y: pointer.position.y - layout.list_rect.y,
            },
            pointer.source,
            pointer.modifiers,
        );
        let _ = self
            .list_state
            .handle_event(UiEvent::PointerMoved(local_list_pointer));

        if is_mouse_button_pressed(MouseButton::Left) {
            let _ = self.button.handle_event(UiEvent::PointerPressed {
                button: PointerButton::Primary,
                state: pointer,
            });
        }

        if is_mouse_button_released(MouseButton::Left) {
            if layout.button_rect.contains(pointer.position) {
                self.focus = FocusTarget::Button;
            } else if layout.tabs_rect.contains(pointer.position) {
                self.focus = FocusTarget::Tabs;
            } else if layout.combo_rect.contains(pointer.position) {
                self.focus = FocusTarget::Combo;
            } else if layout.tree_rect.contains(pointer.position) {
                self.focus = FocusTarget::Tree;
            } else if layout.list_rect.contains(pointer.position) {
                self.focus = FocusTarget::List;
            }

            let response = self.button.handle_event(UiEvent::PointerReleased {
                button: PointerButton::Primary,
                state: pointer,
            });
            if response.action.is_some() {
                self.event_log
                    .push("button activation emitted from ui-core".to_string());
            }

            let (_, interaction) = self.list_state.handle_event(UiEvent::PointerReleased {
                button: PointerButton::Primary,
                state: local_list_pointer,
            });
            if let ListInteraction::Selected(index) = interaction {
                if let Some(item) = self.list_items.get(index) {
                    self.event_log.push(format!("selected list item: {item}"));
                }
            }

            let previous_tab = self.tabs.selected;
            if self
                .tabs
                .handle_event(UiEvent::PointerReleased {
                    button: PointerButton::Primary,
                    state: pointer,
                })
                .request_redraw
                && self.tabs.selected != previous_tab
            {
                if let Some(tab) = self.tabs.selected_page() {
                    self.event_log.push(format!("selected tab: {}", tab.title));
                }
            }

            let previous_combo = self.combo.selected_item().map(str::to_string);
            if self
                .combo
                .handle_event(UiEvent::PointerReleased {
                    button: PointerButton::Primary,
                    state: pointer,
                })
                .request_redraw
                && self.combo.selected_item().map(str::to_string) != previous_combo
            {
                if let Some(item) = self.combo.selected_item() {
                    self.event_log.push(format!("combo: {item}"));
                }
            }

            let previous_tree = self.tree.selected_path.clone();
            if self
                .tree
                .handle_event(UiEvent::PointerReleased {
                    button: PointerButton::Primary,
                    state: pointer,
                })
                .request_redraw
                && self.tree.selected_path != previous_tree
            {
                self.event_log.push(format!(
                    "tree path: {:?}",
                    self.tree.selected_path.clone().unwrap_or_default()
                ));
            }
        }

        let (_, wheel_y) = mouse_wheel();
        if wheel_y.abs() > f32::EPSILON {
            let _ = self.list_state.handle_event(UiEvent::ScrollLines {
                delta: wheel_y.round() as i32,
            });
            self.list_state.layout(layout.list_rect, 0);
        }

        match self.focus {
            FocusTarget::Button => {
                if is_key_pressed(KeyCode::Enter) {
                    let response = self.button.handle_event(UiEvent::KeyPressed {
                        key: Key::Enter,
                        modifiers: Modifiers::default(),
                    });
                    if response.action.is_some() {
                        self.event_log
                            .push("keyboard activation emitted from ui-core".to_string());
                    }
                }
            }
            FocusTarget::Tabs => {
                self.handle_keyboard_widget(KeyCode::Left, Key::Left, |app, event| {
                    app.tabs.handle_event(event)
                });
                self.handle_keyboard_widget(KeyCode::Right, Key::Right, |app, event| {
                    app.tabs.handle_event(event)
                });
            }
            FocusTarget::Combo => {
                self.handle_keyboard_widget(KeyCode::Up, Key::Up, |app, event| {
                    app.combo.handle_event(event)
                });
                self.handle_keyboard_widget(KeyCode::Down, Key::Down, |app, event| {
                    app.combo.handle_event(event)
                });
            }
            FocusTarget::Tree => {
                self.handle_keyboard_widget(KeyCode::Up, Key::Up, |app, event| {
                    app.tree.handle_event(event)
                });
                self.handle_keyboard_widget(KeyCode::Down, Key::Down, |app, event| {
                    app.tree.handle_event(event)
                });
            }
            FocusTarget::List => {}
        }

        if self.event_log.len() > 10 {
            let drain = self.event_log.len() - 10;
            self.event_log.drain(0..drain);
        }
    }

    fn process_touch_input(&mut self, layout: &Layout) -> bool {
        let mut handled_touch = false;
        for touch in touches() {
            handled_touch = true;
            let pointer = PointerState::touch(
                touch.id,
                ui_core::geometry::Point {
                    x: touch.position.x as i32,
                    y: touch.position.y as i32,
                },
            );
            let local_list_pointer = PointerState::new(
                touch.id,
                ui_core::geometry::Point {
                    x: pointer.position.x - layout.list_rect.x,
                    y: pointer.position.y - layout.list_rect.y,
                },
                ui_core::PointerSource::Touch,
                pointer.modifiers,
            );

            match touch.phase {
                TouchPhase::Started => {
                    let _ = self.button.handle_event(UiEvent::PointerPressed {
                        button: PointerButton::Primary,
                        state: pointer,
                    });
                }
                TouchPhase::Moved | TouchPhase::Stationary => {
                    let _ = self.button.handle_event(UiEvent::PointerMoved(pointer));
                    let _ = self
                        .list_state
                        .handle_event(UiEvent::PointerMoved(local_list_pointer));
                }
                TouchPhase::Ended => {
                    self.update_focus_from_pointer(layout, pointer);

                    let response = self.button.handle_event(UiEvent::PointerReleased {
                        button: PointerButton::Primary,
                        state: pointer,
                    });
                    if response.action.is_some() {
                        self.event_log
                            .push("touch activation emitted from ui-core".to_string());
                    }

                    let (_, interaction) = self.list_state.handle_event(UiEvent::PointerReleased {
                        button: PointerButton::Primary,
                        state: local_list_pointer,
                    });
                    if let ListInteraction::Selected(index) = interaction {
                        if let Some(item) = self.list_items.get(index) {
                            self.event_log.push(format!("selected list item: {item}"));
                        }
                    }

                    self.log_tab_selection(pointer);
                    self.log_combo_selection(pointer);
                    self.log_tree_selection(pointer);
                }
                TouchPhase::Cancelled => {
                    let _ = self.button.handle_event(UiEvent::PointerLeft);
                    let _ = self.list_state.handle_event(UiEvent::PointerLeft);
                }
            }
        }

        handled_touch
    }

    fn update_focus_from_pointer(&mut self, layout: &Layout, pointer: PointerState) {
        if layout.button_rect.contains(pointer.position) {
            self.focus = FocusTarget::Button;
        } else if layout.tabs_rect.contains(pointer.position) {
            self.focus = FocusTarget::Tabs;
        } else if layout.combo_rect.contains(pointer.position) {
            self.focus = FocusTarget::Combo;
        } else if layout.tree_rect.contains(pointer.position) {
            self.focus = FocusTarget::Tree;
        } else if layout.list_rect.contains(pointer.position) {
            self.focus = FocusTarget::List;
        }
    }

    fn log_tab_selection(&mut self, pointer: PointerState) {
        let previous_tab = self.tabs.selected;
        if self
            .tabs
            .handle_event(UiEvent::PointerReleased {
                button: PointerButton::Primary,
                state: pointer,
            })
            .request_redraw
            && self.tabs.selected != previous_tab
        {
            if let Some(tab) = self.tabs.selected_page() {
                self.event_log.push(format!("selected tab: {}", tab.title));
            }
        }
    }

    fn log_combo_selection(&mut self, pointer: PointerState) {
        let previous_combo = self.combo.selected_item().map(str::to_string);
        if self
            .combo
            .handle_event(UiEvent::PointerReleased {
                button: PointerButton::Primary,
                state: pointer,
            })
            .request_redraw
            && self.combo.selected_item().map(str::to_string) != previous_combo
        {
            if let Some(item) = self.combo.selected_item() {
                self.event_log.push(format!("combo: {item}"));
            }
        }
    }

    fn log_tree_selection(&mut self, pointer: PointerState) {
        let previous_tree = self.tree.selected_path.clone();
        if self
            .tree
            .handle_event(UiEvent::PointerReleased {
                button: PointerButton::Primary,
                state: pointer,
            })
            .request_redraw
            && self.tree.selected_path != previous_tree
        {
            self.event_log.push(format!(
                "tree path: {:?}",
                self.tree.selected_path.clone().unwrap_or_default()
            ));
        }
    }

    fn finish_event_log(&mut self) {
        if self.event_log.len() > 10 {
            let drain = self.event_log.len() - 10;
            self.event_log.drain(0..drain);
        }
    }

    fn handle_keyboard_widget<F>(&mut self, key_code: KeyCode, key: Key, mut f: F)
    where
        F: FnMut(&mut Self, UiEvent) -> ui_core::WidgetResponse,
    {
        if is_key_pressed(key_code) {
            let before_tab = self.tabs.selected;
            let before_combo = self.combo.selected_item().map(str::to_string);
            let before_tree = self.tree.selected_path.clone();
            let response = f(
                self,
                UiEvent::KeyPressed {
                    key,
                    modifiers: Modifiers::default(),
                },
            );
            if response.request_redraw {
                if self.tabs.selected != before_tab {
                    if let Some(tab) = self.tabs.selected_page() {
                        self.event_log.push(format!("selected tab: {}", tab.title));
                    }
                }
                if self.combo.selected_item().map(str::to_string) != before_combo {
                    if let Some(item) = self.combo.selected_item() {
                        self.event_log.push(format!("combo: {item}"));
                    }
                }
                if self.tree.selected_path != before_tree {
                    self.event_log.push(format!(
                        "tree path: {:?}",
                        self.tree.selected_path.clone().unwrap_or_default()
                    ));
                }
            }
        }
    }

    fn draw(&mut self, layout: &Layout) {
        clear_background(color_u8!(245, 241, 231, 255));

        draw_rectangle(
            layout.list_rect.x as f32,
            layout.list_rect.y as f32,
            layout.list_rect.width as f32,
            layout.list_rect.height as f32,
            color_u8!(221, 231, 217, 255),
        );
        draw_rectangle_lines(
            layout.list_rect.x as f32,
            layout.list_rect.y as f32,
            layout.list_rect.width as f32,
            layout.list_rect.height as f32,
            2.0,
            color_u8!(109, 125, 110, 255),
        );

        let mut scene = Vec::new();
        self.image.paint(&mut scene);
        self.button.paint(&mut scene);
        self.tabs.paint(&mut scene);
        self.combo.paint(&mut scene);
        self.tree.paint(&mut scene);
        render_paint_ops(&scene, &mut self.images);

        for (offset, rect) in self.list_state.item_bounds.iter().enumerate() {
            let index = self.list_state.visible_pos + offset;
            let Some(text) = self.list_items.get(index) else {
                continue;
            };
            let row_rect = Rect {
                x: layout.list_rect.x + rect.x,
                y: layout.list_rect.y + rect.y,
                width: layout.list_rect.width - 12,
                height: rect.height,
            };
            let is_hover = self.list_state.hover_index == Some(index);
            let is_selected = self.list_state.selected_index == Some(index);
            let row_color = if is_selected {
                color_u8!(246, 214, 153, 255)
            } else if is_hover {
                color_u8!(238, 230, 194, 255)
            } else {
                color_u8!(221, 231, 217, 255)
            };
            draw_rectangle(
                row_rect.x as f32 + 6.0,
                row_rect.y as f32 + 4.0,
                (row_rect.width - 12) as f32,
                (row_rect.height - 8) as f32,
                row_color,
            );
            draw_text(
                text,
                (row_rect.x + 18) as f32,
                (row_rect.y + row_rect.height / 2 + 7) as f32,
                FONT_SIZE,
                color_u8!(32, 32, 32, 255),
            );
        }

        draw_text(
            "loadngo macOS host",
            layout.button_rect.x as f32,
            420.0,
            34.0,
            color_u8!(39, 52, 41, 255),
        );
        draw_text(
            "Shared tabs/combo/tree are now rendering from ui-core scene output.",
            layout.button_rect.x as f32,
            454.0,
            22.0,
            color_u8!(78, 90, 77, 255),
        );
        draw_text(
            "Recent events",
            layout.button_rect.x as f32,
            530.0,
            24.0,
            color_u8!(39, 52, 41, 255),
        );
        draw_text(
            &format!("Focus: {}", self.focus_label()),
            layout.button_rect.x as f32,
            560.0,
            20.0,
            color_u8!(58, 65, 57, 255),
        );

        let mut y = 590.0;
        for entry in self.event_log.iter().rev() {
            draw_text(
                entry,
                layout.button_rect.x as f32,
                y,
                20.0,
                color_u8!(58, 65, 57, 255),
            );
            y += 26.0;
        }
    }

    fn focus_label(&self) -> &'static str {
        match self.focus {
            FocusTarget::Button => "Button",
            FocusTarget::Tabs => "Tabs",
            FocusTarget::Combo => "Combo",
            FocusTarget::Tree => "Tree",
            FocusTarget::List => "List",
        }
    }
}

#[cfg(target_os = "macos")]
fn render_paint_ops(ops: &[PaintOp], images: &mut TextureRegistry) {
    for op in ops {
        match op {
            PaintOp::FillRect { rect, color } => draw_rectangle(
                rect.x as f32,
                rect.y as f32,
                rect.width as f32,
                rect.height as f32,
                mq_color(*color),
            ),
            PaintOp::StrokeRect { rect, color } => draw_rectangle_lines(
                rect.x as f32,
                rect.y as f32,
                rect.width as f32,
                rect.height as f32,
                2.0,
                mq_color(*color),
            ),
            PaintOp::Line { from, to, color } => draw_line(
                from.x as f32,
                from.y as f32,
                to.x as f32,
                to.y as f32,
                2.0,
                mq_color(*color),
            ),
            PaintOp::Text { rect, text, style } => draw_text_centered(text, *rect, style),
            PaintOp::BlitImage { rect, image_key } => {
                if let Some(texture) = images.texture_for_key(image_key) {
                    draw_texture_ex(
                        &texture,
                        rect.x as f32,
                        rect.y as f32,
                        WHITE,
                        DrawTextureParams {
                            dest_size: Some(vec2(rect.width as f32, rect.height as f32)),
                            ..Default::default()
                        },
                    );
                }
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn draw_text_centered(text: &str, rect: Rect, style: &TextStyle) {
    let metrics = measure_text(text, None, FONT_SIZE as u16, 1.0);
    let x = if style.centered {
        rect.x as f32 + (rect.width as f32 - metrics.width) * 0.5
    } else {
        rect.x as f32 + 12.0
    };
    let y = rect.y as f32 + (rect.height as f32 + metrics.height) * 0.5 - 6.0;
    draw_text(text, x, y, FONT_SIZE, mq_color(style.color));
}

#[cfg(target_os = "macos")]
fn mq_color(color: UiColor) -> macroquad::prelude::Color {
    macroquad::prelude::Color::from_rgba(color.r, color.g, color.b, color.a)
}

#[cfg(target_os = "macos")]
fn window_conf() -> Conf {
    Conf {
        window_title: "loadngo macOS host".to_string(),
        window_width: 1080,
        window_height: 720,
        high_dpi: true,
        ..Default::default()
    }
}

#[cfg(target_os = "macos")]
#[macroquad::main(window_conf)]
async fn main() {
    simulate_mouse_with_touch(false);
    let mut app = MacHostApp::new();
    loop {
        let layout = app.update_layout();
        app.process_input(&layout);
        app.draw(&layout);
        next_frame().await;
    }
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("host-mac is only available on macOS.");
}
