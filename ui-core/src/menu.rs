//! Application menus: the menu bar an app describes, and a drawn menu bar
//! for hosts that have no system one.
//!
//! An app describes its menus once as a [`MenuBar`] and hands it to the
//! host (`loadngo_host_desktop::set_menu_bar`). On macOS the host shows it in
//! the system menu bar, adds the standard application and Window menus, and
//! reports a chosen item, by menu or by its key equivalent, as a
//! [`MenuCommand`] in the frame's input. A host with no system menu bar
//! returns `false` from `set_menu_bar`; the app then draws a
//! [`MenuBarModel`] at the top of its window, which turns clicks and shortcut
//! keys into the same commands. Either way the app acts on commands, not on
//! key chords, so a shortcut cannot run twice.

use serde::{Deserialize, Serialize};

use crate::{
    geometry::{Color, Point, Rect},
    input::{Key, Modifiers, PointerButton, UiEvent},
    paint::{HorizontalAlign, PaintOp, TextLayoutMode, TextOverflow, TextStyle, VerticalAlign},
};

/// An app-chosen number naming what a menu item does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MenuCommand(pub u32);

/// A key equivalent, always with the platform's primary modifier: Cmd on
/// macOS, Ctrl elsewhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shortcut {
    /// A lowercase letter, digit or punctuation character.
    pub key: char,
    pub shift: bool,
    pub alt: bool,
}

impl Shortcut {
    /// Primary modifier + `key`.
    pub const fn primary(key: char) -> Self {
        Self {
            key,
            shift: false,
            alt: false,
        }
    }

    /// Primary modifier + Shift + `key`.
    pub const fn primary_shift(key: char) -> Self {
        Self {
            key,
            shift: true,
            alt: false,
        }
    }

    /// Whether a key press is this shortcut. Ctrl or Cmd counts as the
    /// primary modifier, so the drawn menu bar works with either.
    pub fn matches(&self, key: Key, modifiers: Modifiers) -> bool {
        let Key::Character(pressed) = key else {
            return false;
        };
        (modifiers.ctrl || modifiers.meta)
            && modifiers.shift == self.shift
            && modifiers.alt == self.alt
            && pressed.to_ascii_lowercase() == self.key
    }

    /// The shortcut as a drawn menu shows it, e.g. `Ctrl+Shift+S`.
    pub fn label(&self) -> String {
        let mut label = String::from("Ctrl+");
        if self.shift {
            label.push_str("Shift+");
        }
        if self.alt {
            label.push_str("Alt+");
        }
        label.push(self.key.to_ascii_uppercase());
        label
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuItem {
    Command {
        command: MenuCommand,
        title: String,
        shortcut: Option<Shortcut>,
        enabled: bool,
    },
    Separator,
}

impl MenuItem {
    /// An enabled item with no shortcut.
    pub fn command(command: MenuCommand, title: impl Into<String>) -> Self {
        Self::Command {
            command,
            title: title.into(),
            shortcut: None,
            enabled: true,
        }
    }

    #[must_use]
    pub fn with_shortcut(mut self, key: Shortcut) -> Self {
        if let Self::Command { shortcut, .. } = &mut self {
            *shortcut = Some(key);
        }
        self
    }

    #[must_use]
    pub fn enabled(mut self, on: bool) -> Self {
        if let Self::Command { enabled, .. } = &mut self {
            *enabled = on;
        }
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Menu {
    pub title: String,
    pub items: Vec<MenuItem>,
}

impl Menu {
    pub fn new(title: impl Into<String>, items: Vec<MenuItem>) -> Self {
        Self {
            title: title.into(),
            items,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MenuBar {
    pub menus: Vec<Menu>,
    /// What the system's own Quit (the macOS application menu) sends. With
    /// `None` the host quits by itself; with a command the app decides, for
    /// example after asking about unsaved work. A drawn menu bar has no
    /// application menu, so an app that wants Quit there lists its own item.
    pub quit: Option<MenuCommand>,
}

impl MenuBar {
    /// The enabled command whose shortcut `key` + `modifiers` is.
    pub fn shortcut_command(&self, key: Key, modifiers: Modifiers) -> Option<MenuCommand> {
        self.menus
            .iter()
            .flat_map(|menu| &menu.items)
            .find_map(|item| match item {
                MenuItem::Command {
                    command,
                    shortcut: Some(shortcut),
                    enabled: true,
                    ..
                } if shortcut.matches(key, modifiers) => Some(*command),
                _ => None,
            })
    }
}

/// What the drawn menu bar did with an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MenuBarResponse {
    /// The event belonged to the menu bar; nothing else should see it.
    pub consumed: bool,
    pub command: Option<MenuCommand>,
}

const BAR_HEIGHT: f32 = 28.0;
const TITLE_PADDING: f32 = 12.0;
const ITEM_HEIGHT: f32 = 26.0;
const SEPARATOR_HEIGHT: f32 = 9.0;
const ITEM_PADDING: f32 = 14.0;
const SHORTCUT_GAP: f32 = 32.0;
const FONT_SIZE: u16 = 14;

const BAR_FILL: Color = Color::rgba(0x1b, 0x20, 0x29, 0xff);
const BORDER: Color = Color::rgba(0x2f, 0x37, 0x45, 0xff);
const DROPDOWN_FILL: Color = Color::rgba(0x20, 0x26, 0x31, 0xff);
const HIGHLIGHT: Color = Color::rgba(0x2f, 0x4f, 0x80, 0xff);
const TEXT: Color = Color::rgba(0xdc, 0xe2, 0xec, 0xff);
const DIM_TEXT: Color = Color::rgba(0x7d, 0x87, 0x98, 0xff);

/// A menu bar drawn in the window, for hosts with no system menu bar.
///
/// Per frame: `relayout`, then events through `handle_event` *before* the
/// rest of the app (it takes shortcut keys and, while a menu is open, all
/// pointer input), then `paint` *after* the rest, so an open menu draws on
/// top.
#[derive(Debug, Clone, Default)]
pub struct MenuBarModel {
    bar: MenuBar,
    bounds: Rect,
    titles: Vec<Rect>,
    open: Option<usize>,
    dropdown: Rect,
    items: Vec<Rect>,
    shortcut_width: f32,
    highlighted: Option<usize>,
}

impl MenuBarModel {
    pub fn new(bar: MenuBar) -> Self {
        Self {
            bar,
            ..Self::default()
        }
    }

    pub fn menu_bar(&self) -> &MenuBar {
        &self.bar
    }

    pub fn set_menu_bar(&mut self, bar: MenuBar) {
        if self.open.is_some_and(|index| index >= bar.menus.len()) {
            self.open = None;
        }
        self.bar = bar;
    }

    /// The bar's height; lay the app out below it.
    pub fn height() -> f32 {
        BAR_HEIGHT
    }

    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// Lays the bar out across the top of `surface`.
    pub fn relayout<F>(&mut self, surface: Rect, mut measure_width: F)
    where
        F: FnMut(&str, u16) -> f32,
    {
        self.bounds = Rect {
            x: surface.x,
            y: surface.y,
            width: surface.width,
            height: BAR_HEIGHT,
        };
        self.titles.clear();
        let mut x = surface.x + 4.0;
        for menu in &self.bar.menus {
            let width = measure_width(&menu.title, FONT_SIZE) + TITLE_PADDING * 2.0;
            self.titles.push(Rect {
                x,
                y: surface.y,
                width,
                height: BAR_HEIGHT,
            });
            x += width;
        }
        self.items.clear();
        let Some(open) = self.open else {
            return;
        };
        let menu = &self.bar.menus[open];
        let mut title_width = 0.0f32;
        let mut shortcut_width = 0.0f32;
        for item in &menu.items {
            if let MenuItem::Command {
                title, shortcut, ..
            } = item
            {
                title_width = title_width.max(measure_width(title, FONT_SIZE));
                if let Some(shortcut) = shortcut {
                    shortcut_width =
                        shortcut_width.max(measure_width(&shortcut.label(), FONT_SIZE));
                }
            }
        }
        self.shortcut_width = shortcut_width;
        let width = (ITEM_PADDING * 2.0
            + title_width
            + if shortcut_width > 0.0 {
                SHORTCUT_GAP + shortcut_width
            } else {
                0.0
            })
        .max(160.0);
        let mut y = surface.y + BAR_HEIGHT + 4.0;
        for item in &menu.items {
            let height = match item {
                MenuItem::Command { .. } => ITEM_HEIGHT,
                MenuItem::Separator => SEPARATOR_HEIGHT,
            };
            self.items.push(Rect {
                x: self.titles[open].x,
                y,
                width,
                height,
            });
            y += height;
        }
        self.dropdown = Rect {
            x: self.titles[open].x,
            y: surface.y + BAR_HEIGHT,
            width,
            height: y - (surface.y + BAR_HEIGHT) + 4.0,
        };
    }

    fn command_at(&self, index: usize) -> Option<MenuCommand> {
        match self.bar.menus.get(self.open?)?.items.get(index)? {
            MenuItem::Command {
                command,
                enabled: true,
                ..
            } => Some(*command),
            _ => None,
        }
    }

    fn item_at(&self, point: Point) -> Option<usize> {
        self.items.iter().position(|rect| rect.contains(point))
    }

    fn open_menu(&mut self, index: Option<usize>) {
        if self.open != index {
            self.open = index;
            self.highlighted = None;
            self.items.clear();
        }
    }

    /// Moves the keyboard highlight to the next enabled item in `step`'s
    /// direction.
    fn step_highlight(&mut self, step: isize) {
        let Some(open) = self.open else {
            return;
        };
        let count = self.bar.menus[open].items.len() as isize;
        let mut index = self
            .highlighted
            .map_or(if step > 0 { -1 } else { count }, |i| i as isize);
        for _ in 0..count {
            index = (index + step).rem_euclid(count);
            if self.command_at(index as usize).is_some() {
                self.highlighted = Some(index as usize);
                return;
            }
        }
    }

    pub fn handle_event(&mut self, event: &UiEvent) -> MenuBarResponse {
        let consumed = MenuBarResponse {
            consumed: true,
            command: None,
        };
        match event {
            UiEvent::PointerMoved(state) => {
                let point = state.position;
                if self.open.is_none() {
                    return MenuBarResponse::default();
                }
                if let Some(title) = self.titles.iter().position(|rect| rect.contains(point)) {
                    self.open_menu(Some(title));
                }
                // Only a pointer over the menu moves the highlight, so the
                // keyboard's survives a pointer resting elsewhere.
                if self.dropdown.contains(point) {
                    self.highlighted = self
                        .item_at(point)
                        .filter(|&i| self.command_at(i).is_some());
                }
                consumed
            }
            UiEvent::PointerPressed {
                button: PointerButton::Primary,
                state,
            } => {
                let point = state.position;
                if let Some(title) = self.titles.iter().position(|rect| rect.contains(point)) {
                    let next = (self.open != Some(title)).then_some(title);
                    self.open_menu(next);
                    return consumed;
                }
                if self.open.is_none() {
                    return MenuBarResponse::default();
                }
                if self.dropdown.contains(point) {
                    // Chosen on release, like a system menu.
                    return consumed;
                }
                self.open_menu(None);
                consumed
            }
            UiEvent::PointerReleased {
                button: PointerButton::Primary,
                state,
            } => {
                if self.open.is_none() {
                    return MenuBarResponse::default();
                }
                if let Some(command) = self
                    .item_at(state.position)
                    .and_then(|i| self.command_at(i))
                {
                    self.open_menu(None);
                    return MenuBarResponse {
                        consumed: true,
                        command: Some(command),
                    };
                }
                consumed
            }
            UiEvent::KeyPressed { key, modifiers } => {
                if let Some(open) = self.open {
                    match key {
                        Key::Escape => self.open_menu(None),
                        Key::Down => self.step_highlight(1),
                        Key::Up => self.step_highlight(-1),
                        Key::Left | Key::Right => {
                            let count = self.bar.menus.len();
                            let next = if *key == Key::Left {
                                (open + count - 1) % count
                            } else {
                                (open + 1) % count
                            };
                            self.open_menu(Some(next));
                        }
                        Key::Enter | Key::Space => {
                            let command = self.highlighted.and_then(|i| self.command_at(i));
                            self.open_menu(None);
                            return MenuBarResponse {
                                consumed: true,
                                command,
                            };
                        }
                        _ => {}
                    }
                    return consumed;
                }
                match self.bar.shortcut_command(*key, *modifiers) {
                    Some(command) => MenuBarResponse {
                        consumed: true,
                        command: Some(command),
                    },
                    None => MenuBarResponse::default(),
                }
            }
            UiEvent::TextInput { .. } | UiEvent::ScrollLines { .. } if self.open.is_some() => {
                consumed
            }
            UiEvent::FocusChanged(false) => {
                self.open_menu(None);
                MenuBarResponse::default()
            }
            _ => MenuBarResponse::default(),
        }
    }

    pub fn paint(&self, scene: &mut Vec<PaintOp>) {
        scene.push(PaintOp::FillRect {
            rect: self.bounds,
            color: BAR_FILL,
        });
        scene.push(PaintOp::Line {
            from: Point {
                x: self.bounds.x,
                y: self.bounds.bottom() - 0.5,
            },
            to: Point {
                x: self.bounds.right(),
                y: self.bounds.bottom() - 0.5,
            },
            color: BORDER,
        });
        for (index, (menu, rect)) in self.bar.menus.iter().zip(&self.titles).enumerate() {
            if self.open == Some(index) {
                scene.push(PaintOp::FillRect {
                    rect: *rect,
                    color: HIGHLIGHT,
                });
            }
            push_label(scene, &menu.title, *rect, TEXT, HorizontalAlign::Center);
        }
        let Some(open) = self.open else {
            return;
        };
        if self.items.is_empty() {
            return;
        }
        scene.push(PaintOp::FillRect {
            rect: self.dropdown,
            color: DROPDOWN_FILL,
        });
        scene.push(PaintOp::StrokeRect {
            rect: self.dropdown,
            color: BORDER,
        });
        for (index, (item, rect)) in self.bar.menus[open]
            .items
            .iter()
            .zip(&self.items)
            .enumerate()
        {
            match item {
                MenuItem::Separator => scene.push(PaintOp::Line {
                    from: Point {
                        x: rect.x + 8.0,
                        y: rect.y + rect.height / 2.0,
                    },
                    to: Point {
                        x: rect.right() - 8.0,
                        y: rect.y + rect.height / 2.0,
                    },
                    color: BORDER,
                }),
                MenuItem::Command {
                    title,
                    shortcut,
                    enabled,
                    ..
                } => {
                    if self.highlighted == Some(index) {
                        scene.push(PaintOp::FillRect {
                            rect: Rect {
                                x: rect.x + 4.0,
                                width: rect.width - 8.0,
                                ..*rect
                            },
                            color: HIGHLIGHT,
                        });
                    }
                    let color = if *enabled { TEXT } else { DIM_TEXT };
                    let inner = Rect {
                        x: rect.x + ITEM_PADDING,
                        width: rect.width - ITEM_PADDING * 2.0,
                        ..*rect
                    };
                    push_label(scene, title, inner, color, HorizontalAlign::Left);
                    if let Some(shortcut) = shortcut {
                        push_label(
                            scene,
                            &shortcut.label(),
                            inner,
                            DIM_TEXT,
                            HorizontalAlign::Right,
                        );
                    }
                }
            }
        }
    }
}

fn push_label(
    scene: &mut Vec<PaintOp>,
    text: &str,
    rect: Rect,
    color: Color,
    align: HorizontalAlign,
) {
    scene.push(PaintOp::Text {
        rect,
        clip_rect: Some(rect),
        text: text.to_string(),
        style: TextStyle {
            color,
            font_size: FONT_SIZE,
            horizontal_align: align,
            vertical_align: VerticalAlign::Middle,
            layout_mode: TextLayoutMode::SingleLine,
            overflow: TextOverflow::EllipsisEnd,
            ..TextStyle::default()
        },
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::PointerState;

    const OPEN: MenuCommand = MenuCommand(1);
    const SAVE: MenuCommand = MenuCommand(2);
    const SAVE_ALL: MenuCommand = MenuCommand(3);
    const UNDO: MenuCommand = MenuCommand(4);

    fn bar(save_enabled: bool) -> MenuBar {
        MenuBar {
            menus: vec![
                Menu::new(
                    "File",
                    vec![
                        MenuItem::command(OPEN, "Open Folder…")
                            .with_shortcut(Shortcut::primary('o')),
                        MenuItem::Separator,
                        MenuItem::command(SAVE, "Save")
                            .with_shortcut(Shortcut::primary('s'))
                            .enabled(save_enabled),
                        MenuItem::command(SAVE_ALL, "Save All")
                            .with_shortcut(Shortcut::primary_shift('s')),
                    ],
                ),
                Menu::new(
                    "Edit",
                    vec![MenuItem::command(UNDO, "Undo").with_shortcut(Shortcut::primary('z'))],
                ),
            ],
            quit: None,
        }
    }

    fn model(save_enabled: bool) -> MenuBarModel {
        let mut model = MenuBarModel::new(bar(save_enabled));
        model.relayout(
            Rect {
                x: 0.0,
                y: 0.0,
                width: 800.0,
                height: 600.0,
            },
            |text, size| text.chars().count() as f32 * f32::from(size) * 0.5,
        );
        model
    }

    fn ctrl(shift: bool) -> Modifiers {
        Modifiers {
            ctrl: true,
            shift,
            ..Modifiers::default()
        }
    }

    fn center(rect: Rect) -> Point {
        Point {
            x: rect.x + rect.width / 2.0,
            y: rect.y + rect.height / 2.0,
        }
    }

    fn press_release(model: &mut MenuBarModel, point: Point) -> MenuBarResponse {
        let state = PointerState::mouse(point, Modifiers::default());
        let pressed = model.handle_event(&UiEvent::PointerPressed {
            button: PointerButton::Primary,
            state,
        });
        model.relayout(
            Rect {
                x: 0.0,
                y: 0.0,
                width: 800.0,
                height: 600.0,
            },
            |text, size| text.chars().count() as f32 * f32::from(size) * 0.5,
        );
        let released = model.handle_event(&UiEvent::PointerReleased {
            button: PointerButton::Primary,
            state,
        });
        MenuBarResponse {
            consumed: pressed.consumed || released.consumed,
            command: released.command,
        }
    }

    #[test]
    fn shortcuts_need_the_primary_modifier_and_exact_shift() {
        let mut model = model(true);
        let key = |model: &mut MenuBarModel, modifiers| {
            model.handle_event(&UiEvent::KeyPressed {
                key: Key::Character('s'),
                modifiers,
            })
        };
        assert_eq!(key(&mut model, ctrl(false)).command, Some(SAVE));
        assert_eq!(key(&mut model, ctrl(true)).command, Some(SAVE_ALL));
        let plain = key(&mut model, Modifiers::default());
        assert_eq!(plain, MenuBarResponse::default(), "typing an s is text");
        let cmd = Modifiers {
            meta: true,
            ..Modifiers::default()
        };
        assert_eq!(key(&mut model, cmd).command, Some(SAVE));
    }

    #[test]
    fn a_disabled_item_neither_fires_nor_takes_its_shortcut() {
        let mut model = model(false);
        let response = model.handle_event(&UiEvent::KeyPressed {
            key: Key::Character('s'),
            modifiers: ctrl(false),
        });
        assert_eq!(response, MenuBarResponse::default());
        {
            let point = center(model.titles[0]);
            press_release(&mut model, point)
        };
        assert!(model.is_open());
        let save = model.items[2];
        assert_eq!(press_release(&mut model, center(save)).command, None);
    }

    #[test]
    fn click_a_title_then_an_item_to_choose_it() {
        let mut model = model(true);
        let opened = {
            let point = center(model.titles[0]);
            press_release(&mut model, point)
        };
        assert!(opened.consumed && model.is_open());
        let chosen = {
            let point = center(model.items[0]);
            press_release(&mut model, point)
        };
        assert_eq!(chosen.command, Some(OPEN));
        assert!(!model.is_open());
    }

    #[test]
    fn clicking_outside_an_open_menu_closes_it_without_passing_the_click_on() {
        let mut model = model(true);
        {
            let point = center(model.titles[1]);
            press_release(&mut model, point)
        };
        let outside = press_release(&mut model, Point { x: 600.0, y: 400.0 });
        assert!(outside.consumed && outside.command.is_none());
        assert!(!model.is_open());
    }

    #[test]
    fn keys_walk_an_open_menu_and_skip_separators() {
        let mut model = model(true);
        {
            let point = center(model.titles[0]);
            press_release(&mut model, point)
        };
        let key = |model: &mut MenuBarModel, key| {
            model.handle_event(&UiEvent::KeyPressed {
                key,
                modifiers: Modifiers::default(),
            })
        };
        key(&mut model, Key::Down);
        key(&mut model, Key::Down);
        assert_eq!(model.highlighted, Some(2), "past the separator");
        assert_eq!(key(&mut model, Key::Enter).command, Some(SAVE));
        {
            let point = center(model.titles[0]);
            press_release(&mut model, point)
        };
        key(&mut model, Key::Right);
        assert_eq!(model.open, Some(1));
        key(&mut model, Key::Escape);
        assert!(!model.is_open());
    }

    #[test]
    fn shortcut_labels_spell_out_the_modifiers() {
        assert_eq!(Shortcut::primary_shift('s').label(), "Ctrl+Shift+S");
        assert_eq!(Shortcut::primary('o').label(), "Ctrl+O");
    }
}
