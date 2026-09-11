//! Open and Save file dialogs, drawn and driven entirely by `loadngo`.
//!
//! A game or tool built on `loadngo` owns its whole window, so an OS-native
//! picker would be a second, differently-behaved UI toolkit bolted onto the
//! side (and a platform crate per OS). This is the `loadngo` one: a modal
//! [`FileDialogModel`] composed from the same primitives every other screen
//! uses -- [`ButtonModel`], [`TextFieldModel`], [`ScrollRegionModel`] -- so it
//! follows the same widget contract (events in, [`WidgetResponse`] out, paint
//! ops out) and the same desktop hover/focus/keyboard rules.
//!
//! The model never touches the filesystem directly. It asks a
//! [`DirectorySource`] for listings, which keeps every rule here (sorting,
//! filtering, overwrite confirmation, path resolution) unit-testable against
//! an in-memory tree; [`StdDirectorySource`] is the real `std::fs` one.
//!
//! Host loop, per frame, while the dialog is open:
//!
//! 1. `set_bounds` (if the window resized), then `relayout(measure)`.
//! 2. Feed every input event to `handle_event` and **nothing else** -- the
//!    dialog is modal, so app shortcuts must not see keys typed into its name
//!    field. Feed wheel input to `scroll_wheel`.
//! 3. `advance(delta_seconds)` -- drives scroll glide and double-click timing.
//! 4. `paint`. Stop showing the dialog once a response carries an `outcome`.

use std::cmp::Ordering;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use crate::{
    button::ButtonModel,
    geometry::{Color, Point, Rect},
    input::{Key, Modifiers, PointerButton, UiEvent},
    paint::{HorizontalAlign, PaintOp, TextOverflow, TextStyle, VerticalAlign},
    scroll::{ScrollRegionModel, ScrollThumbDragState},
    text::single_line_text_box_height,
    text_field::TextFieldModel,
    widget::{WidgetAction, WidgetId, WidgetResponse},
};

/// Whether the dialog picks an existing file or names a file to write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileDialogMode {
    Open,
    Save,
}

/// Restricts the listing to one kind of file, e.g. `WAV audio (*.wav)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileTypeFilter {
    pub label: String,
    /// Lowercase, without the leading dot. The first is the one a Save dialog
    /// appends when the typed name has none of them.
    pub extensions: Vec<String>,
}

impl FileTypeFilter {
    pub fn new(label: impl Into<String>, extensions: &[&str]) -> Self {
        Self {
            label: label.into(),
            extensions: extensions
                .iter()
                .map(|ext| ext.trim_start_matches('.').to_ascii_lowercase())
                .collect(),
        }
    }

    /// Case-insensitive extension match.
    pub fn matches(&self, file_name: &str) -> bool {
        let Some(ext) = Path::new(file_name)
            .extension()
            .and_then(|ext| ext.to_str())
        else {
            return false;
        };
        self.extensions
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(ext))
    }

    pub fn default_extension(&self) -> Option<&str> {
        self.extensions.first().map(String::as_str)
    }

    /// e.g. `WAV audio (*.wav)`.
    pub fn description(&self) -> String {
        let patterns: Vec<String> = self
            .extensions
            .iter()
            .map(|ext| format!("*.{ext}"))
            .collect();
        format!("{} ({})", self.label, patterns.join(", "))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathKind {
    File,
    Directory,
}

/// One row of a directory listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryEntry {
    pub name: String,
    pub is_dir: bool,
    /// `None` for directories, or when the size couldn't be read.
    pub size_bytes: Option<u64>,
}

/// Where the dialog gets its view of the filesystem.
pub trait DirectorySource {
    fn list(&self, directory: &Path) -> io::Result<Vec<DirectoryEntry>>;
    /// `None` if nothing exists at `path`.
    fn kind(&self, path: &Path) -> Option<PathKind>;
    /// Expansion target for a leading `~` in a typed path.
    fn home_dir(&self) -> Option<PathBuf>;
}

/// The real filesystem, through `std::fs`.
#[derive(Debug, Clone, Copy, Default)]
pub struct StdDirectorySource;

impl StdDirectorySource {
    /// Home plus the usual per-user folders that actually exist on this
    /// machine, for a dialog's places column.
    pub fn standard_places(&self) -> Vec<FileDialogPlace> {
        let Some(home) = self.home_dir() else {
            return Vec::new();
        };
        let mut places = vec![FileDialogPlace::new("Home", home.clone())];
        for folder in ["Desktop", "Documents", "Music", "Downloads"] {
            let path = home.join(folder);
            if path.is_dir() {
                places.push(FileDialogPlace::new(folder, path));
            }
        }
        places
    }
}

impl DirectorySource for StdDirectorySource {
    fn list(&self, directory: &Path) -> io::Result<Vec<DirectoryEntry>> {
        let mut entries = Vec::new();
        for entry in std::fs::read_dir(directory)? {
            let Ok(entry) = entry else { continue };
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                // Not valid UTF-8: can't be displayed or typed back faithfully.
                continue;
            };
            // `metadata` follows symlinks, so a link to a folder lists as one.
            let metadata = std::fs::metadata(entry.path()).ok();
            let is_dir = metadata.as_ref().is_some_and(std::fs::Metadata::is_dir);
            entries.push(DirectoryEntry {
                name,
                is_dir,
                size_bytes: metadata.filter(|_| !is_dir).map(|m| m.len()),
            });
        }
        Ok(entries)
    }

    fn kind(&self, path: &Path) -> Option<PathKind> {
        let metadata = std::fs::metadata(path).ok()?;
        Some(if metadata.is_dir() {
            PathKind::Directory
        } else {
            PathKind::File
        })
    }

    fn home_dir(&self) -> Option<PathBuf> {
        std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
    }
}

/// A shortcut in the dialog's left column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDialogPlace {
    pub label: String,
    pub path: PathBuf,
}

impl FileDialogPlace {
    pub fn new(label: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        Self {
            label: label.into(),
            path: path.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileDialogOutcome {
    /// Open: an existing file. Save: the path to write, already confirmed for
    /// overwrite if it exists.
    Confirmed(PathBuf),
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FileDialogResponse {
    pub widget: WidgetResponse,
    /// `Some` once the dialog is finished; the host should close it.
    pub outcome: Option<FileDialogOutcome>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Name,
    List,
    Up,
    Cancel,
    Confirm,
}

const FOCUS_ORDER: [Focus; 5] = [
    Focus::Name,
    Focus::List,
    Focus::Confirm,
    Focus::Cancel,
    Focus::Up,
];

const UP_ID: WidgetId = WidgetId(1);
const CANCEL_ID: WidgetId = WidgetId(2);
const CONFIRM_ID: WidgetId = WidgetId(3);
const NAME_ID: WidgetId = WidgetId(4);

const PADDING: f32 = 16.0;
const GAP: f32 = 10.0;
const TITLE_FONT: u16 = 18;
const BODY_FONT: u16 = 15;
const SMALL_FONT: u16 = 13;
const ROW_HEIGHT: f32 = 26.0;
const BUTTON_HEIGHT: f32 = 34.0;
const BUTTON_WIDTH: f32 = 112.0;
const PLACES_WIDTH: f32 = 150.0;
const SIZE_COLUMN_WIDTH: f32 = 96.0;
const NAME_LABEL_WIDTH: f32 = 56.0;
const WHEEL_ROWS_PER_NOTCH: f32 = 3.0;
const DOUBLE_CLICK_SECONDS: f64 = 0.45;

const PANEL_FILL: Color = Color::rgba(0x1a, 0x1f, 0x2a, 0xfa);
const PANEL_BORDER: Color = Color::rgba(0x5f, 0x6b, 0x80, 0xff);
const WELL_FILL: Color = Color::rgba(0x12, 0x16, 0x1e, 0xff);
const WELL_BORDER: Color = Color::rgba(0x36, 0x41, 0x53, 0xff);
const FOCUS_BORDER: Color = Color::rgba(0x5c, 0x8d, 0xe8, 0xff);
const HOVER_FILL: Color = Color::rgba(0x24, 0x2c, 0x3a, 0xff);
const SELECTED_FILL: Color = Color::rgba(0x36, 0x5c, 0x96, 0xff);
const SELECTED_UNFOCUSED_FILL: Color = Color::rgba(0x2c, 0x3a, 0x52, 0xff);
const TEXT: Color = Color::rgba(0xe8, 0xec, 0xf4, 0xff);
const DIM_TEXT: Color = Color::rgba(0x9a, 0xa4, 0xb6, 0xff);
const FOLDER_TEXT: Color = Color::rgba(0xa8, 0xd1, 0xff, 0xff);
const ERROR_TEXT: Color = Color::rgba(0xff, 0x8a, 0x7a, 0xff);
const SCROLL_THUMB: Color = Color::rgba(0x5c, 0x8d, 0xe8, 0xe6);
/// Dims whatever is behind a modal dialog; see [`FileDialogModel::paint_scrim`].
pub const FILE_DIALOG_SCRIM: Color = Color::rgba(0x00, 0x00, 0x00, 0x9c);

#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Layout {
    title: Rect,
    path: Rect,
    places: Option<Rect>,
    list: Rect,
    name_label: Rect,
    message: Rect,
}

pub struct FileDialogModel {
    bounds: Rect,
    title: String,
    mode: FileDialogMode,
    filter: Option<FileTypeFilter>,
    /// Show dot-files. Off by default, as every desktop picker does.
    pub show_hidden: bool,
    source: Box<dyn DirectorySource>,
    directory: PathBuf,
    entries: Vec<DirectoryEntry>,
    selected: Option<usize>,
    hover_entry: Option<usize>,
    places: Vec<FileDialogPlace>,
    hover_place: Option<usize>,
    list_scroll: ScrollRegionModel,
    thumb_drag: Option<ScrollThumbDragState>,
    name_field: TextFieldModel,
    up_button: ButtonModel,
    cancel_button: ButtonModel,
    confirm_button: ButtonModel,
    focus: Focus,
    overwrite_pending: Option<PathBuf>,
    message: Option<String>,
    clock_seconds: f64,
    last_entry_press: Option<(usize, f64)>,
    layout: Layout,
}

impl fmt::Debug for FileDialogModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileDialogModel")
            .field("mode", &self.mode)
            .field("title", &self.title)
            .field("directory", &self.directory)
            .field("entries", &self.entries.len())
            .field("selected", &self.selected)
            .field("focus", &self.focus)
            .field("overwrite_pending", &self.overwrite_pending)
            .field("message", &self.message)
            .finish_non_exhaustive()
    }
}

impl FileDialogModel {
    /// A dialog for choosing an existing file, starting in `directory`.
    pub fn open(
        title: impl Into<String>,
        directory: impl Into<PathBuf>,
        filter: Option<FileTypeFilter>,
        source: Box<dyn DirectorySource>,
    ) -> Self {
        let mut dialog = Self::new(FileDialogMode::Open, title.into(), filter, source);
        dialog.navigate_to(directory.into());
        dialog.set_focus(Focus::List);
        dialog
    }

    /// A dialog for naming a file to write, starting in `directory` with
    /// `suggested_name` filled in and its stem selected, so typing replaces
    /// the name but keeps the extension.
    pub fn save(
        title: impl Into<String>,
        directory: impl Into<PathBuf>,
        suggested_name: &str,
        filter: Option<FileTypeFilter>,
        source: Box<dyn DirectorySource>,
    ) -> Self {
        let mut dialog = Self::new(FileDialogMode::Save, title.into(), filter, source);
        dialog.navigate_to(directory.into());
        dialog.name_field.set_text(suggested_name);
        let stem_len = Path::new(suggested_name)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map_or(0, |stem| stem.chars().count());
        dialog.set_focus(Focus::Name);
        dialog.name_field.select_range(0, stem_len);
        dialog
    }

    fn new(
        mode: FileDialogMode,
        title: String,
        filter: Option<FileTypeFilter>,
        source: Box<dyn DirectorySource>,
    ) -> Self {
        let confirm_label = match mode {
            FileDialogMode::Open => "Open",
            FileDialogMode::Save => "Save",
        };
        let mut name_field = TextFieldModel::with_id(NAME_ID, "", Rect::default());
        name_field.area.style.font_size = BODY_FONT;
        let mut up_button = ButtonModel::with_id(UP_ID, "Up", Rect::default());
        let mut cancel_button = ButtonModel::with_id(CANCEL_ID, "Cancel", Rect::default());
        let mut confirm_button = ButtonModel::with_id(CONFIRM_ID, confirm_label, Rect::default());
        for button in [&mut up_button, &mut cancel_button, &mut confirm_button] {
            button.font_size = BODY_FONT;
        }
        Self {
            bounds: Rect::default(),
            title,
            mode,
            filter,
            show_hidden: false,
            source,
            directory: PathBuf::new(),
            entries: Vec::new(),
            selected: None,
            hover_entry: None,
            places: Vec::new(),
            hover_place: None,
            list_scroll: ScrollRegionModel::new(Rect::default(), 0.0),
            thumb_drag: None,
            name_field,
            up_button,
            cancel_button,
            confirm_button,
            focus: Focus::List,
            overwrite_pending: None,
            message: None,
            clock_seconds: 0.0,
            last_entry_press: None,
            layout: Layout::default(),
        }
    }

    /// Shortcuts shown in a column beside the listing.
    pub fn set_places(&mut self, places: Vec<FileDialogPlace>) {
        self.places = places;
        self.apply_layout();
    }

    pub fn mode(&self) -> FileDialogMode {
        self.mode
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// The listing as shown: filtered, folders first, then by name.
    pub fn entries(&self) -> &[DirectoryEntry] {
        &self.entries
    }

    pub fn selected_entry(&self) -> Option<&DirectoryEntry> {
        self.selected.and_then(|index| self.entries.get(index))
    }

    pub fn name_text(&self) -> String {
        self.name_field.text()
    }

    /// The last problem to report (bad name, missing folder, unreadable
    /// directory), or the overwrite question.
    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    pub fn is_confirming_overwrite(&self) -> bool {
        self.overwrite_pending.is_some()
    }

    pub fn bounds(&self) -> Rect {
        self.bounds
    }

    pub fn set_bounds(&mut self, bounds: Rect) {
        self.bounds = bounds;
        self.apply_layout();
    }

    /// Whether the text cursor should show at `point` (over the name field).
    pub fn prefers_text_cursor(&self, point: Point) -> bool {
        self.name_field.prefers_text_cursor(point)
    }

    /// Lays out children and measures the name field's text. Call every frame
    /// before painting.
    pub fn relayout<F>(&mut self, measure_width: F)
    where
        F: FnMut(&str, u16) -> f32,
    {
        self.apply_layout();
        self.name_field.relayout(measure_width);
    }

    /// Advances scroll glide and the double-click clock. Returns whether
    /// anything moved, so a host that only redraws on change keeps going.
    pub fn advance(&mut self, delta_seconds: f32) -> bool {
        self.clock_seconds += f64::from(delta_seconds.max(0.0));
        self.list_scroll.advance(delta_seconds)
    }

    /// Scrolls the listing if the pointer is over it. `precise` is
    /// `InputSnapshot::mouse_wheel_precise`: pixel deltas apply 1:1, notches
    /// glide by a few rows each.
    pub fn scroll_wheel(&mut self, pointer: Point, delta_y: f32, precise: bool) -> WidgetResponse {
        if delta_y == 0.0 || !self.layout.list.contains(pointer) {
            return WidgetResponse::default();
        }
        if precise {
            self.list_scroll.apply_scroll_delta(-delta_y);
        } else {
            self.list_scroll
                .glide_scroll_delta(-delta_y * ROW_HEIGHT * WHEEL_ROWS_PER_NOTCH);
        }
        WidgetResponse::redraw_consumed()
    }

    /// Paints a full-surface scrim behind the dialog so the app underneath
    /// reads as inactive. Call before [`paint`](Self::paint).
    pub fn paint_scrim(surface: Rect, scene: &mut Vec<PaintOp>) {
        scene.push(PaintOp::FillRect {
            rect: surface,
            color: FILE_DIALOG_SCRIM,
        });
    }

    pub fn handle_event(&mut self, event: UiEvent) -> FileDialogResponse {
        let mut response = FileDialogResponse {
            widget: WidgetResponse {
                // Modal: nothing behind the dialog may act on this input.
                input_consumed: true,
                ..WidgetResponse::default()
            },
            outcome: None,
        };
        match event {
            UiEvent::PointerMoved(state) => self.pointer_moved(state, &mut response),
            UiEvent::PointerLeft => {
                self.hover_entry = None;
                self.hover_place = None;
                for button in self.buttons_mut() {
                    button.handle_event(UiEvent::PointerLeft);
                }
                self.name_field.handle_event(UiEvent::PointerLeft);
                response.widget.request_redraw = true;
            }
            UiEvent::PointerPressed {
                button: PointerButton::Primary,
                state,
            } => self.pointer_pressed(state, &mut response),
            UiEvent::PointerReleased {
                button: PointerButton::Primary,
                state,
            } => self.pointer_released(state, &mut response),
            UiEvent::KeyPressed { key, modifiers } => {
                self.key_pressed(key, modifiers, &mut response)
            }
            UiEvent::TextInput { text } if self.overwrite_pending.is_none() => {
                if self.focus == Focus::List {
                    // Typing while browsing names a file, like every
                    // desktop picker: jump to the name field.
                    self.set_focus(Focus::Name);
                }
                if self.focus == Focus::Name {
                    let field = self.name_field.handle_event(UiEvent::TextInput { text });
                    response.widget.request_redraw |= field.request_redraw;
                    self.message = None;
                }
            }
            _ => {}
        }
        response
    }

    pub fn paint(&self, scene: &mut Vec<PaintOp>) {
        scene.push(PaintOp::FillRect {
            rect: self.bounds,
            color: PANEL_FILL,
        });
        scene.push(PaintOp::StrokeRect {
            rect: self.bounds,
            color: PANEL_BORDER,
        });

        push_text(
            scene,
            self.layout.title,
            &self.title,
            TITLE_FONT,
            TEXT,
            HorizontalAlign::Left,
        );
        self.up_button.paint(scene);
        let mut path_style = text_style(BODY_FONT, DIM_TEXT, HorizontalAlign::Left);
        path_style.overflow = TextOverflow::EllipsisMiddle;
        scene.push(PaintOp::Text {
            rect: self.layout.path,
            clip_rect: Some(self.layout.path),
            text: self.directory.display().to_string(),
            style: path_style,
        });

        if let Some(places_rect) = self.layout.places {
            self.paint_places(scene, places_rect);
        }
        self.paint_entries(scene);

        push_text(
            scene,
            self.layout.name_label,
            "Name",
            BODY_FONT,
            DIM_TEXT,
            HorizontalAlign::Left,
        );
        self.name_field.paint(scene);

        let (message, color) = match (&self.message, &self.filter) {
            (Some(message), _) => (message.clone(), ERROR_TEXT),
            (None, Some(filter)) => (format!("Showing {}", filter.description()), DIM_TEXT),
            (None, None) => (String::new(), DIM_TEXT),
        };
        if !message.is_empty() {
            push_text(
                scene,
                self.layout.message,
                &message,
                SMALL_FONT,
                color,
                HorizontalAlign::Left,
            );
        }

        self.cancel_button.paint(scene);
        self.confirm_button.paint(scene);
    }

    // --- navigation and confirmation -------------------------------------

    fn navigate_to(&mut self, directory: PathBuf) {
        match self.source.list(&directory) {
            Ok(entries) => {
                let mut entries: Vec<DirectoryEntry> = entries
                    .into_iter()
                    .filter(|entry| self.show_hidden || !entry.name.starts_with('.'))
                    .filter(|entry| {
                        entry.is_dir
                            || self
                                .filter
                                .as_ref()
                                .is_none_or(|filter| filter.matches(&entry.name))
                    })
                    .collect();
                entries.sort_by(compare_entries);
                self.entries = entries;
                self.directory = directory;
                self.selected = None;
                self.hover_entry = None;
                self.last_entry_press = None;
                self.list_scroll
                    .apply_scroll_delta(-self.list_scroll.offset);
                self.message = None;
                self.apply_layout();
            }
            Err(error) => {
                self.message = Some(format!("Can't open {}: {error}", directory.display()));
            }
        }
    }

    fn go_up(&mut self) {
        if let Some(parent) = self.directory.parent().map(Path::to_path_buf) {
            let previous = self
                .directory
                .file_name()
                .map(|name| name.to_string_lossy().into_owned());
            self.navigate_to(parent);
            // Land on the folder we came out of, so Up then Enter is a no-op.
            if let Some(previous) = previous {
                if let Some(index) = self.entries.iter().position(|entry| entry.name == previous) {
                    self.select_entry(index);
                }
            }
        }
    }

    fn resolve_typed(&self, typed: &str) -> PathBuf {
        if typed == "~" {
            if let Some(home) = self.source.home_dir() {
                return home;
            }
        }
        if let Some(rest) = typed.strip_prefix("~/") {
            if let Some(home) = self.source.home_dir() {
                return home.join(rest);
            }
        }
        let path = Path::new(typed);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.directory.join(path)
        }
    }

    /// Confirm pressed (button, Enter in the name field).
    fn confirm(&mut self) -> Option<FileDialogOutcome> {
        if let Some(path) = self.overwrite_pending.take() {
            return Some(FileDialogOutcome::Confirmed(path));
        }
        let typed = self.name_field.text().trim().to_string();
        if typed.is_empty() {
            return match self.selected_entry().cloned() {
                Some(entry) => self.activate_entry(&entry),
                None => {
                    self.message = Some(match self.mode {
                        FileDialogMode::Open => "Choose a file to open".to_string(),
                        FileDialogMode::Save => "Type a name for the file".to_string(),
                    });
                    None
                }
            };
        }

        let path = self.resolve_typed(&typed);
        if self.source.kind(&path) == Some(PathKind::Directory) {
            self.navigate_to(path);
            self.name_field.set_text("");
            return None;
        }
        match self.mode {
            FileDialogMode::Open => self.confirm_open(path, &typed),
            FileDialogMode::Save => self.confirm_save(path),
        }
    }

    fn confirm_open(&mut self, path: PathBuf, typed: &str) -> Option<FileDialogOutcome> {
        if self.source.kind(&path) != Some(PathKind::File) {
            self.message = Some(format!("No such file: {typed}"));
            return None;
        }
        if let Some(filter) = &self.filter {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            if !filter.matches(name) {
                self.message = Some(format!("Not a {} file", filter.label));
                return None;
            }
        }
        Some(FileDialogOutcome::Confirmed(path))
    }

    fn confirm_save(&mut self, mut path: PathBuf) -> Option<FileDialogOutcome> {
        let Some(name) = path
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_owned)
        else {
            self.message = Some("Type a name for the file".to_string());
            return None;
        };
        if let Some(filter) = &self.filter {
            if !filter.matches(&name) {
                if let Some(ext) = filter.default_extension() {
                    path.set_file_name(format!("{name}.{ext}"));
                }
            }
        }
        let parent_ok = path
            .parent()
            .is_some_and(|parent| self.source.kind(parent) == Some(PathKind::Directory));
        if !parent_ok {
            self.message = Some("That folder doesn't exist".to_string());
            return None;
        }
        match self.source.kind(&path) {
            Some(PathKind::File) => {
                let shown = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned());
                self.message = Some(format!(
                    "\"{}\" already exists. Replace it?",
                    shown.unwrap_or_default()
                ));
                self.confirm_button.text = "Replace".to_string();
                self.overwrite_pending = Some(path);
                self.set_focus(Focus::Confirm);
                None
            }
            Some(PathKind::Directory) => {
                self.message = Some("A folder with that name already exists".to_string());
                None
            }
            None => Some(FileDialogOutcome::Confirmed(path)),
        }
    }

    fn cancel_overwrite(&mut self) {
        self.overwrite_pending = None;
        self.message = None;
        self.confirm_button.text = "Save".to_string();
        self.set_focus(Focus::Name);
    }

    /// Enter / double-click on a row.
    fn activate_entry(&mut self, entry: &DirectoryEntry) -> Option<FileDialogOutcome> {
        if entry.is_dir {
            self.navigate_to(self.directory.join(&entry.name));
            return None;
        }
        match self.mode {
            FileDialogMode::Open => Some(FileDialogOutcome::Confirmed(
                self.directory.join(&entry.name),
            )),
            FileDialogMode::Save => {
                self.name_field.set_text(&entry.name);
                self.confirm_save(self.directory.join(&entry.name))
            }
        }
    }

    fn select_entry(&mut self, index: usize) {
        let Some(entry) = self.entries.get(index) else {
            return;
        };
        self.selected = Some(index);
        if !entry.is_dir {
            let name = entry.name.clone();
            self.name_field.set_text(&name);
        }
        self.message = None;
        self.scroll_entry_into_view(index);
    }

    fn scroll_entry_into_view(&mut self, index: usize) {
        let top = index as f32 * ROW_HEIGHT;
        let bottom = top + ROW_HEIGHT;
        let view = self.list_scroll.viewport.height;
        let offset = self.list_scroll.offset;
        if top < offset {
            self.list_scroll.apply_scroll_delta(top - offset);
        } else if bottom > offset + view {
            self.list_scroll
                .apply_scroll_delta(bottom - (offset + view));
        }
    }

    // --- input --------------------------------------------------------------

    fn buttons_mut(&mut self) -> [&mut ButtonModel; 3] {
        [
            &mut self.up_button,
            &mut self.cancel_button,
            &mut self.confirm_button,
        ]
    }

    fn set_focus(&mut self, focus: Focus) {
        self.focus = focus;
        self.name_field
            .handle_event(UiEvent::FocusChanged(focus == Focus::Name));
        self.up_button
            .handle_event(UiEvent::FocusChanged(focus == Focus::Up));
        self.cancel_button
            .handle_event(UiEvent::FocusChanged(focus == Focus::Cancel));
        self.confirm_button
            .handle_event(UiEvent::FocusChanged(focus == Focus::Confirm));
    }

    fn pointer_moved(
        &mut self,
        state: crate::input::PointerState,
        response: &mut FileDialogResponse,
    ) {
        if let Some(drag) = self.thumb_drag {
            self.list_scroll.drag_indicator_to(state.position.y, drag);
            response.widget.request_redraw = true;
            return;
        }
        let mut redraw = false;
        for button in self.buttons_mut() {
            redraw |= button
                .handle_event(UiEvent::PointerMoved(state))
                .request_redraw;
        }
        redraw |= self
            .name_field
            .handle_event(UiEvent::PointerMoved(state))
            .request_redraw;
        let hover_entry = self.entry_at(state.position);
        let hover_place = self.place_at(state.position);
        redraw |= hover_entry != self.hover_entry || hover_place != self.hover_place;
        self.hover_entry = hover_entry;
        self.hover_place = hover_place;
        response.widget.request_redraw = redraw;
    }

    fn pointer_pressed(
        &mut self,
        state: crate::input::PointerState,
        response: &mut FileDialogResponse,
    ) {
        response.widget.request_redraw = true;
        let point = state.position;
        let press = UiEvent::PointerPressed {
            button: PointerButton::Primary,
            state,
        };

        if self.overwrite_pending.is_some() {
            // Only the two answers to "Replace it?" are live.
            self.cancel_button.handle_event(press.clone());
            self.confirm_button.handle_event(press);
            return;
        }

        for (focus, hit) in [
            (Focus::Up, self.up_button.bounds.contains(point)),
            (Focus::Cancel, self.cancel_button.bounds.contains(point)),
            (Focus::Confirm, self.confirm_button.bounds.contains(point)),
        ] {
            if hit {
                self.set_focus(focus);
            }
        }
        for button in self.buttons_mut() {
            button.handle_event(press.clone());
        }

        if self.name_field.bounds().contains(point) {
            self.set_focus(Focus::Name);
            self.name_field.handle_event(press);
            return;
        }

        if let Some(drag) = self.list_scroll.begin_indicator_drag(point.y).filter(|_| {
            self.list_scroll
                .indicator_track_rect()
                .is_some_and(|track| expand(track, 6.0).contains(point))
        }) {
            self.thumb_drag = Some(drag);
            return;
        }
        if let Some(track) = self.list_scroll.indicator_track_rect() {
            if expand(track, 6.0).contains(point) {
                self.list_scroll.scroll_to_indicator_position(point.y);
                return;
            }
        }

        if let Some(index) = self.entry_at(point) {
            self.set_focus(Focus::List);
            let double = self.last_entry_press.is_some_and(|(last, at)| {
                last == index && self.clock_seconds - at <= DOUBLE_CLICK_SECONDS
            });
            self.select_entry(index);
            if double {
                self.last_entry_press = None;
                if let Some(entry) = self.entries.get(index).cloned() {
                    response.outcome = self.activate_entry(&entry);
                }
            } else {
                self.last_entry_press = Some((index, self.clock_seconds));
            }
            return;
        }
        if self.layout.list.contains(point) {
            self.set_focus(Focus::List);
            self.selected = None;
            return;
        }
        if let Some(index) = self.place_at(point) {
            if let Some(place) = self.places.get(index).cloned() {
                self.navigate_to(place.path);
            }
        }
    }

    fn pointer_released(
        &mut self,
        state: crate::input::PointerState,
        response: &mut FileDialogResponse,
    ) {
        response.widget.request_redraw = true;
        if self.thumb_drag.take().is_some() {
            return;
        }
        let release = UiEvent::PointerReleased {
            button: PointerButton::Primary,
            state,
        };
        self.name_field.handle_event(release.clone());
        let mut activated = Vec::new();
        for button in self.buttons_mut() {
            if let Some(WidgetAction::Activate(id)) = button.handle_event(release.clone()).action {
                activated.push(id);
            }
        }
        for id in activated {
            if let Some(outcome) = self.activate_button(id) {
                response.outcome = Some(outcome);
                return;
            }
        }
    }

    fn activate_button(&mut self, id: WidgetId) -> Option<FileDialogOutcome> {
        match id {
            CONFIRM_ID => self.confirm(),
            CANCEL_ID if self.overwrite_pending.is_some() => {
                self.cancel_overwrite();
                None
            }
            CANCEL_ID => Some(FileDialogOutcome::Cancelled),
            UP_ID if self.overwrite_pending.is_none() => {
                self.go_up();
                None
            }
            _ => None,
        }
    }

    fn key_pressed(&mut self, key: Key, modifiers: Modifiers, response: &mut FileDialogResponse) {
        response.widget.request_redraw = true;
        if key == Key::Escape {
            if self.overwrite_pending.is_some() {
                self.cancel_overwrite();
            } else {
                response.outcome = Some(FileDialogOutcome::Cancelled);
            }
            return;
        }
        if key == Key::Tab {
            let current = FOCUS_ORDER
                .iter()
                .position(|f| *f == self.focus)
                .unwrap_or(0);
            let len = FOCUS_ORDER.len();
            let next = if modifiers.shift {
                (current + len - 1) % len
            } else {
                (current + 1) % len
            };
            self.set_focus(FOCUS_ORDER[next]);
            return;
        }
        if self.overwrite_pending.is_some() {
            if matches!(key, Key::Enter | Key::Space) {
                match self.focus {
                    Focus::Cancel => self.cancel_overwrite(),
                    _ => response.outcome = self.confirm(),
                }
            }
            return;
        }

        match self.focus {
            Focus::Name => {
                if key == Key::Down {
                    self.set_focus(Focus::List);
                    self.move_selection(1);
                    return;
                }
                let field = self
                    .name_field
                    .handle_event(UiEvent::KeyPressed { key, modifiers });
                if field.action.is_some() {
                    response.outcome = self.confirm();
                } else if field.request_redraw {
                    self.message = None;
                }
            }
            Focus::List => match key {
                Key::Up => self.move_selection(-1),
                Key::Down => self.move_selection(1),
                Key::Home if !self.entries.is_empty() => self.select_entry(0),
                Key::End if !self.entries.is_empty() => self.select_entry(self.entries.len() - 1),
                Key::Backspace | Key::Left => self.go_up(),
                Key::Right => {
                    if let Some(entry) = self.selected_entry().filter(|entry| entry.is_dir).cloned()
                    {
                        self.activate_entry(&entry);
                    }
                }
                Key::Enter => {
                    if let Some(entry) = self.selected_entry().cloned() {
                        response.outcome = self.activate_entry(&entry);
                    } else {
                        response.outcome = self.confirm();
                    }
                }
                _ => {}
            },
            Focus::Up | Focus::Cancel | Focus::Confirm => {
                if matches!(key, Key::Enter | Key::Space) {
                    let id = match self.focus {
                        Focus::Up => UP_ID,
                        Focus::Cancel => CANCEL_ID,
                        _ => CONFIRM_ID,
                    };
                    response.outcome = self.activate_button(id);
                }
            }
        }
    }

    fn move_selection(&mut self, step: isize) {
        if self.entries.is_empty() {
            return;
        }
        let last = self.entries.len() - 1;
        let next = match self.selected {
            None => 0,
            Some(current) => current.saturating_add_signed(step).min(last),
        };
        self.select_entry(next);
    }

    // --- layout and hit testing ----------------------------------------------

    fn apply_layout(&mut self) {
        let inner = Rect {
            x: self.bounds.x + PADDING,
            y: self.bounds.y + PADDING,
            width: (self.bounds.width - PADDING * 2.0).max(0.0),
            height: (self.bounds.height - PADDING * 2.0).max(0.0),
        };
        let title_height = single_line_text_box_height(TITLE_FONT) + 4.0;
        let title = Rect {
            height: title_height,
            ..inner
        };
        let path_row_y = title.bottom() + 4.0;
        self.up_button.set_bounds(Rect {
            x: inner.x,
            y: path_row_y,
            width: 64.0,
            height: 30.0,
        });
        let path = Rect {
            x: inner.x + 64.0 + GAP,
            y: path_row_y,
            width: (inner.width - 64.0 - GAP).max(0.0),
            height: 30.0,
        };

        let buttons_y = inner.bottom() - BUTTON_HEIGHT;
        self.confirm_button.set_bounds(Rect {
            x: inner.right() - BUTTON_WIDTH,
            y: buttons_y,
            width: BUTTON_WIDTH,
            height: BUTTON_HEIGHT,
        });
        self.cancel_button.set_bounds(Rect {
            x: inner.right() - BUTTON_WIDTH * 2.0 - GAP,
            y: buttons_y,
            width: BUTTON_WIDTH,
            height: BUTTON_HEIGHT,
        });
        let message = Rect {
            x: inner.x,
            y: buttons_y,
            width: (inner.width - BUTTON_WIDTH * 2.0 - GAP * 2.0).max(0.0),
            height: BUTTON_HEIGHT,
        };
        let name_y = buttons_y - GAP - 32.0;
        let name_label = Rect {
            x: inner.x,
            y: name_y,
            width: NAME_LABEL_WIDTH,
            height: 32.0,
        };
        self.name_field.set_bounds(Rect {
            x: inner.x + NAME_LABEL_WIDTH,
            y: name_y,
            width: (inner.width - NAME_LABEL_WIDTH).max(0.0),
            height: 32.0,
        });

        let body_y = path.bottom() + GAP;
        let body_height = (name_y - GAP - body_y).max(0.0);
        let (places, list_x) = if self.places.is_empty() {
            (None, inner.x)
        } else {
            (
                Some(Rect {
                    x: inner.x,
                    y: body_y,
                    width: PLACES_WIDTH,
                    height: body_height,
                }),
                inner.x + PLACES_WIDTH + GAP,
            )
        };
        let list = Rect {
            x: list_x,
            y: body_y,
            width: (inner.right() - list_x).max(0.0),
            height: body_height,
        };
        self.list_scroll.set_viewport(list);
        self.list_scroll
            .set_content_height(self.entries.len() as f32 * ROW_HEIGHT);

        self.layout = Layout {
            title,
            path,
            places,
            list,
            name_label,
            message,
        };
    }

    fn entry_rect(&self, index: usize) -> Rect {
        let list = self.layout.list;
        Rect {
            x: list.x,
            y: list.y + index as f32 * ROW_HEIGHT - self.list_scroll.offset,
            width: list.width,
            height: ROW_HEIGHT,
        }
    }

    fn entry_at(&self, point: Point) -> Option<usize> {
        let list = self.layout.list;
        if !list.contains(point) {
            return None;
        }
        if self
            .list_scroll
            .indicator_track_rect()
            .is_some_and(|track| expand(track, 6.0).contains(point))
        {
            return None;
        }
        let index = ((point.y - list.y + self.list_scroll.offset) / ROW_HEIGHT).floor();
        (index >= 0.0 && (index as usize) < self.entries.len()).then_some(index as usize)
    }

    fn place_rect(&self, index: usize) -> Option<Rect> {
        let places = self.layout.places?;
        Some(Rect {
            x: places.x,
            y: places.y + 4.0 + index as f32 * ROW_HEIGHT,
            width: places.width,
            height: ROW_HEIGHT,
        })
    }

    fn place_at(&self, point: Point) -> Option<usize> {
        (0..self.places.len()).find(|index| {
            self.place_rect(*index)
                .is_some_and(|rect| rect.contains(point))
        })
    }

    // --- painting ------------------------------------------------------------

    fn paint_places(&self, scene: &mut Vec<PaintOp>, rect: Rect) {
        scene.push(PaintOp::FillRect {
            rect,
            color: WELL_FILL,
        });
        scene.push(PaintOp::StrokeRect {
            rect,
            color: WELL_BORDER,
        });
        for (index, place) in self.places.iter().enumerate() {
            let Some(row) = self.place_rect(index) else {
                continue;
            };
            let Some(visible) = intersect(row, rect) else {
                continue;
            };
            let current = place.path == self.directory;
            if current || self.hover_place == Some(index) {
                scene.push(PaintOp::FillRect {
                    rect: visible,
                    color: if current {
                        SELECTED_UNFOCUSED_FILL
                    } else {
                        HOVER_FILL
                    },
                });
            }
            let text_rect = Rect {
                x: row.x + 10.0,
                width: (row.width - 14.0).max(0.0),
                ..row
            };
            let mut style = text_style(BODY_FONT, TEXT, HorizontalAlign::Left);
            style.overflow = TextOverflow::EllipsisEnd;
            scene.push(PaintOp::Text {
                rect: text_rect,
                clip_rect: Some(rect),
                text: place.label.clone(),
                style,
            });
        }
    }

    fn paint_entries(&self, scene: &mut Vec<PaintOp>) {
        let list = self.layout.list;
        scene.push(PaintOp::FillRect {
            rect: list,
            color: WELL_FILL,
        });
        scene.push(PaintOp::StrokeRect {
            rect: list,
            color: if self.focus == Focus::List {
                FOCUS_BORDER
            } else {
                WELL_BORDER
            },
        });

        if self.entries.is_empty() {
            let text = match &self.filter {
                Some(filter) => format!("No folders or {} files here", filter.label),
                None => "This folder is empty".to_string(),
            };
            push_text(
                scene,
                Rect {
                    x: list.x + 12.0,
                    y: list.y + 8.0,
                    width: (list.width - 24.0).max(0.0),
                    height: ROW_HEIGHT,
                },
                &text,
                BODY_FONT,
                DIM_TEXT,
                HorizontalAlign::Left,
            );
        }

        let first = (self.list_scroll.offset / ROW_HEIGHT).floor().max(0.0) as usize;
        let visible_rows = (list.height / ROW_HEIGHT).ceil() as usize + 1;
        let scrollbar_room = if self.list_scroll.indicator_track_rect().is_some() {
            14.0
        } else {
            0.0
        };
        for index in first..(first + visible_rows).min(self.entries.len()) {
            let entry = &self.entries[index];
            let row = self.entry_rect(index);
            let Some(visible) = intersect(row, list) else {
                continue;
            };
            let fill = if self.selected == Some(index) {
                Some(if self.focus == Focus::List {
                    SELECTED_FILL
                } else {
                    SELECTED_UNFOCUSED_FILL
                })
            } else if self.hover_entry == Some(index) {
                Some(HOVER_FILL)
            } else {
                None
            };
            if let Some(color) = fill {
                scene.push(PaintOp::FillRect {
                    rect: visible,
                    color,
                });
            }

            let name_rect = Rect {
                x: row.x + 12.0,
                width: (row.width - 24.0 - SIZE_COLUMN_WIDTH - scrollbar_room).max(0.0),
                ..row
            };
            let (label, color) = if entry.is_dir {
                (format!("{}/", entry.name), FOLDER_TEXT)
            } else {
                (entry.name.clone(), TEXT)
            };
            let mut style = text_style(BODY_FONT, color, HorizontalAlign::Left);
            style.overflow = TextOverflow::EllipsisMiddle;
            scene.push(PaintOp::Text {
                rect: name_rect,
                clip_rect: Some(list),
                text: label,
                style,
            });
            if let Some(size) = entry.size_bytes {
                scene.push(PaintOp::Text {
                    rect: Rect {
                        x: row.right() - 12.0 - SIZE_COLUMN_WIDTH - scrollbar_room,
                        width: SIZE_COLUMN_WIDTH,
                        ..row
                    },
                    clip_rect: Some(list),
                    text: format_size(size),
                    style: text_style(SMALL_FONT, DIM_TEXT, HorizontalAlign::Right),
                });
            }
        }
        self.list_scroll.paint_indicator(scene, SCROLL_THUMB);
    }
}

fn compare_entries(a: &DirectoryEntry, b: &DirectoryEntry) -> Ordering {
    b.is_dir
        .cmp(&a.is_dir)
        .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        .then_with(|| a.name.cmp(&b.name))
}

/// `532 B`, `48.2 KB`, `12.7 MB`, `1.4 GB`.
pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

fn text_style(font_size: u16, color: Color, horizontal_align: HorizontalAlign) -> TextStyle {
    TextStyle {
        color,
        font_size,
        horizontal_align,
        vertical_align: VerticalAlign::Middle,
        ..TextStyle::default()
    }
}

fn push_text(
    scene: &mut Vec<PaintOp>,
    rect: Rect,
    text: &str,
    font_size: u16,
    color: Color,
    horizontal_align: HorizontalAlign,
) {
    scene.push(PaintOp::Text {
        rect,
        clip_rect: Some(rect),
        text: text.to_string(),
        style: text_style(font_size, color, horizontal_align),
    });
}

fn intersect(a: Rect, b: Rect) -> Option<Rect> {
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    let right = a.right().min(b.right());
    let bottom = a.bottom().min(b.bottom());
    (right > x && bottom > y).then_some(Rect {
        x,
        y,
        width: right - x,
        height: bottom - y,
    })
}

fn expand(rect: Rect, by: f32) -> Rect {
    Rect {
        x: rect.x - by,
        y: rect.y - by,
        width: rect.width + by * 2.0,
        height: rect.height + by * 2.0,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::rc::Rc;

    use super::*;
    use crate::input::PointerState;

    /// An in-memory tree: path -> Some(size) for files, None for folders.
    #[derive(Clone, Default)]
    struct FakeFs {
        nodes: Rc<BTreeMap<PathBuf, Option<u64>>>,
    }

    impl FakeFs {
        fn new(paths: &[(&str, Option<u64>)]) -> Self {
            let mut nodes = BTreeMap::new();
            for (path, size) in paths {
                let path = PathBuf::from(path);
                // Every ancestor is a folder.
                for ancestor in path.ancestors().skip(1) {
                    nodes.entry(ancestor.to_path_buf()).or_insert(None);
                }
                nodes.insert(path, *size);
            }
            Self {
                nodes: Rc::new(nodes),
            }
        }
    }

    impl DirectorySource for FakeFs {
        fn list(&self, directory: &Path) -> io::Result<Vec<DirectoryEntry>> {
            if self.kind(directory) != Some(PathKind::Directory) {
                return Err(io::Error::new(io::ErrorKind::NotFound, "no such folder"));
            }
            Ok(self
                .nodes
                .iter()
                .filter(|(path, _)| path.parent() == Some(directory))
                .map(|(path, size)| DirectoryEntry {
                    name: path.file_name().unwrap().to_string_lossy().into_owned(),
                    is_dir: size.is_none(),
                    size_bytes: *size,
                })
                .collect())
        }

        fn kind(&self, path: &Path) -> Option<PathKind> {
            self.nodes.get(path).map(|size| match size {
                Some(_) => PathKind::File,
                None => PathKind::Directory,
            })
        }

        fn home_dir(&self) -> Option<PathBuf> {
            Some(PathBuf::from("/home/jay"))
        }
    }

    fn tree() -> FakeFs {
        FakeFs::new(&[
            ("/home/jay/Music/Takes/take-01.wav", Some(4_800_044)),
            ("/home/jay/Music/Takes/take-02.WAV", Some(1_024)),
            ("/home/jay/Music/Takes/notes.txt", Some(12)),
            ("/home/jay/Music/Takes/.hidden.wav", Some(44)),
            ("/home/jay/Music/Takes/Archive/old.wav", Some(44)),
            ("/home/jay/Music/Takes/bass/solo.wav", Some(44)),
        ])
    }

    fn wav() -> Option<FileTypeFilter> {
        Some(FileTypeFilter::new("WAV audio", &[".wav"]))
    }

    fn laid_out(mut dialog: FileDialogModel) -> FileDialogModel {
        dialog.set_bounds(Rect {
            x: 0.0,
            y: 0.0,
            width: 720.0,
            height: 480.0,
        });
        dialog.relayout(|text, size| text.chars().count() as f32 * f32::from(size) * 0.5);
        dialog
    }

    fn open_dialog() -> FileDialogModel {
        laid_out(FileDialogModel::open(
            "Open take",
            "/home/jay/Music/Takes",
            wav(),
            Box::new(tree()),
        ))
    }

    fn save_dialog(name: &str) -> FileDialogModel {
        laid_out(FileDialogModel::save(
            "Save take",
            "/home/jay/Music/Takes",
            name,
            wav(),
            Box::new(tree()),
        ))
    }

    fn key(dialog: &mut FileDialogModel, key: Key) -> FileDialogResponse {
        dialog.handle_event(UiEvent::KeyPressed {
            key,
            modifiers: Modifiers::default(),
        })
    }

    fn type_text(dialog: &mut FileDialogModel, text: &str) {
        dialog.handle_event(UiEvent::TextInput {
            text: text.to_string(),
        });
    }

    fn click(dialog: &mut FileDialogModel, point: Point) -> FileDialogResponse {
        let state = PointerState::mouse(point, Modifiers::default());
        dialog.handle_event(UiEvent::PointerMoved(state));
        let pressed = dialog.handle_event(UiEvent::PointerPressed {
            button: PointerButton::Primary,
            state,
        });
        if pressed.outcome.is_some() {
            return pressed;
        }
        dialog.handle_event(UiEvent::PointerReleased {
            button: PointerButton::Primary,
            state,
        })
    }

    fn center(rect: Rect) -> Point {
        Point {
            x: rect.x + rect.width / 2.0,
            y: rect.y + rect.height / 2.0,
        }
    }

    fn names(dialog: &FileDialogModel) -> Vec<&str> {
        dialog
            .entries()
            .iter()
            .map(|entry| entry.name.as_str())
            .collect()
    }

    #[test]
    fn listing_puts_folders_first_filters_by_type_and_hides_dotfiles() {
        let dialog = open_dialog();
        assert_eq!(
            names(&dialog),
            vec!["Archive", "bass", "take-01.wav", "take-02.WAV"]
        );
    }

    #[test]
    fn show_hidden_lists_dotfiles_after_a_refresh() {
        let mut dialog = open_dialog();
        dialog.show_hidden = true;
        dialog.navigate_to(PathBuf::from("/home/jay/Music/Takes"));
        assert!(names(&dialog).contains(&".hidden.wav"));
    }

    #[test]
    fn arrow_keys_select_and_enter_opens_a_file() {
        let mut dialog = open_dialog();
        for _ in 0..3 {
            key(&mut dialog, Key::Down);
        }
        assert_eq!(dialog.selected_entry().unwrap().name, "take-01.wav");
        assert_eq!(dialog.name_text(), "take-01.wav");
        let response = key(&mut dialog, Key::Enter);
        assert_eq!(
            response.outcome,
            Some(FileDialogOutcome::Confirmed(PathBuf::from(
                "/home/jay/Music/Takes/take-01.wav"
            )))
        );
    }

    #[test]
    fn enter_on_a_folder_navigates_and_backspace_comes_back_to_it() {
        let mut dialog = open_dialog();
        key(&mut dialog, Key::Down);
        assert!(key(&mut dialog, Key::Enter).outcome.is_none());
        assert_eq!(
            dialog.directory(),
            Path::new("/home/jay/Music/Takes/Archive")
        );
        assert_eq!(names(&dialog), vec!["old.wav"]);

        key(&mut dialog, Key::Backspace);
        assert_eq!(dialog.directory(), Path::new("/home/jay/Music/Takes"));
        assert_eq!(dialog.selected_entry().unwrap().name, "Archive");
    }

    #[test]
    fn double_clicking_a_row_opens_it_but_slow_clicks_only_select() {
        let mut dialog = open_dialog();
        let row = center(dialog.entry_rect(2));

        click(&mut dialog, row);
        dialog.advance(1.0);
        assert!(
            click(&mut dialog, row).outcome.is_none(),
            "slow second click opened"
        );

        dialog.advance(0.1);
        let response = click(&mut dialog, row);
        assert_eq!(
            response.outcome,
            Some(FileDialogOutcome::Confirmed(PathBuf::from(
                "/home/jay/Music/Takes/take-01.wav"
            )))
        );
    }

    #[test]
    fn typing_a_folder_path_navigates_instead_of_confirming() {
        let mut dialog = open_dialog();
        type_text(&mut dialog, "~/Music");
        assert!(key(&mut dialog, Key::Enter).outcome.is_none());
        assert_eq!(dialog.directory(), Path::new("/home/jay/Music"));
        assert_eq!(dialog.name_text(), "");
    }

    #[test]
    fn open_refuses_missing_and_wrong_type_files() {
        let mut dialog = open_dialog();
        type_text(&mut dialog, "nope.wav");
        assert!(key(&mut dialog, Key::Enter).outcome.is_none());
        assert_eq!(dialog.message(), Some("No such file: nope.wav"));

        dialog.name_field.set_text("notes.txt");
        assert!(key(&mut dialog, Key::Enter).outcome.is_none());
        assert_eq!(dialog.message(), Some("Not a WAV audio file"));
    }

    #[test]
    fn save_appends_the_filter_extension_to_a_bare_name() {
        let mut dialog = save_dialog("take-03.wav");
        dialog.name_field.set_text("groove");
        let response = key(&mut dialog, Key::Enter);
        assert_eq!(
            response.outcome,
            Some(FileDialogOutcome::Confirmed(PathBuf::from(
                "/home/jay/Music/Takes/groove.wav"
            )))
        );
    }

    #[test]
    fn save_starts_with_the_stem_selected_so_typing_keeps_the_extension() {
        let mut dialog = save_dialog("take-03.wav");
        type_text(&mut dialog, "groove");
        dialog.relayout(|text, size| text.chars().count() as f32 * f32::from(size) * 0.5);
        assert_eq!(dialog.name_text(), "groove.wav");
    }

    #[test]
    fn saving_over_an_existing_file_asks_first() {
        let mut dialog = save_dialog("take-01.wav");
        assert!(key(&mut dialog, Key::Enter).outcome.is_none());
        assert!(dialog.is_confirming_overwrite());
        assert_eq!(
            dialog.message(),
            Some("\"take-01.wav\" already exists. Replace it?")
        );

        // Escape backs out to the name field rather than closing the dialog.
        assert!(key(&mut dialog, Key::Escape).outcome.is_none());
        assert!(!dialog.is_confirming_overwrite());

        key(&mut dialog, Key::Enter);
        let response = key(&mut dialog, Key::Enter);
        assert_eq!(
            response.outcome,
            Some(FileDialogOutcome::Confirmed(PathBuf::from(
                "/home/jay/Music/Takes/take-01.wav"
            )))
        );
    }

    #[test]
    fn replace_is_also_reachable_by_clicking() {
        let mut dialog = save_dialog("take-01.wav");
        key(&mut dialog, Key::Enter);
        let confirm = center(dialog.confirm_button.bounds);
        assert_eq!(dialog.confirm_button.text, "Replace");
        let response = click(&mut dialog, confirm);
        assert!(matches!(
            response.outcome,
            Some(FileDialogOutcome::Confirmed(_))
        ));
    }

    #[test]
    fn save_refuses_a_missing_folder() {
        let mut dialog = save_dialog("x.wav");
        dialog.name_field.set_text("/nowhere/x.wav");
        assert!(key(&mut dialog, Key::Enter).outcome.is_none());
        assert_eq!(dialog.message(), Some("That folder doesn't exist"));
    }

    #[test]
    fn escape_and_the_cancel_button_both_cancel() {
        let mut dialog = open_dialog();
        assert_eq!(
            key(&mut dialog, Key::Escape).outcome,
            Some(FileDialogOutcome::Cancelled)
        );
        let mut dialog = open_dialog();
        let cancel = center(dialog.cancel_button.bounds);
        assert_eq!(
            click(&mut dialog, cancel).outcome,
            Some(FileDialogOutcome::Cancelled)
        );
    }

    #[test]
    fn enter_only_activates_the_focused_button() {
        // ButtonModel activates on Enter regardless of its own focus, so the
        // dialog must route keys to the focused control only.
        let mut dialog = open_dialog();
        key(&mut dialog, Key::Down);
        key(&mut dialog, Key::Down);
        let response = key(&mut dialog, Key::Enter);
        assert!(response.outcome.is_none());
        assert_eq!(dialog.directory(), Path::new("/home/jay/Music/Takes/bass"));
    }

    #[test]
    fn typing_while_browsing_moves_into_the_name_field() {
        let mut dialog = open_dialog();
        type_text(&mut dialog, "take-02.WAV");
        dialog.relayout(|text, size| text.chars().count() as f32 * f32::from(size) * 0.5);
        assert_eq!(dialog.name_text(), "take-02.WAV");
        assert!(matches!(
            key(&mut dialog, Key::Enter).outcome,
            Some(FileDialogOutcome::Confirmed(_))
        ));
    }

    #[test]
    fn an_unreadable_folder_reports_and_keeps_the_current_listing() {
        let mut dialog = open_dialog();
        dialog.navigate_to(PathBuf::from("/does/not/exist"));
        assert_eq!(dialog.directory(), Path::new("/home/jay/Music/Takes"));
        assert!(dialog
            .message()
            .unwrap()
            .starts_with("Can't open /does/not/exist"));
    }

    #[test]
    fn clicking_a_place_navigates_there() {
        let mut dialog = open_dialog();
        dialog.set_places(vec![FileDialogPlace::new("Music", "/home/jay/Music")]);
        let place = center(dialog.place_rect(0).unwrap());
        click(&mut dialog, place);
        assert_eq!(dialog.directory(), Path::new("/home/jay/Music"));
    }

    #[test]
    fn wheel_scrolling_only_applies_over_the_listing() {
        let many: Vec<(String, Option<u64>)> = (0..60)
            .map(|i| (format!("/d/take-{i:02}.wav"), Some(44)))
            .collect();
        let refs: Vec<(&str, Option<u64>)> = many
            .iter()
            .map(|(path, size)| (path.as_str(), *size))
            .collect();
        let mut dialog = laid_out(FileDialogModel::open(
            "Open",
            "/d",
            wav(),
            Box::new(FakeFs::new(&refs)),
        ));
        let outside = Point { x: 1.0, y: 1.0 };
        assert!(!dialog.scroll_wheel(outside, -3.0, true).input_consumed);
        let inside = center(dialog.layout.list);
        dialog.scroll_wheel(inside, -40.0, true);
        assert_eq!(dialog.list_scroll.offset, 40.0);
    }

    #[test]
    fn keyboard_selection_scrolls_the_row_into_view() {
        let many: Vec<(String, Option<u64>)> = (0..60)
            .map(|i| (format!("/d/take-{i:02}.wav"), Some(44)))
            .collect();
        let refs: Vec<(&str, Option<u64>)> = many
            .iter()
            .map(|(path, size)| (path.as_str(), *size))
            .collect();
        let mut dialog = laid_out(FileDialogModel::open(
            "Open",
            "/d",
            wav(),
            Box::new(FakeFs::new(&refs)),
        ));
        key(&mut dialog, Key::End);
        let row = dialog.entry_rect(59);
        assert!(row.bottom() <= dialog.layout.list.bottom() + 0.01);
        assert!(row.y >= dialog.layout.list.y);
    }

    #[test]
    fn sizes_format_in_binary_units() {
        assert_eq!(format_size(532), "532 B");
        assert_eq!(format_size(4_800_044), "4.6 MB");
        assert_eq!(format_size(1_024), "1.0 KB");
    }

    #[test]
    fn filter_matching_ignores_case_and_leading_dots() {
        let filter = FileTypeFilter::new("WAV audio", &[".WAV", "wave"]);
        assert!(filter.matches("a.wav"));
        assert!(filter.matches("a.Wave"));
        assert!(!filter.matches("wav"));
        assert_eq!(filter.description(), "WAV audio (*.wav, *.wave)");
    }
}
