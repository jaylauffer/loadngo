//! The editor window as a model: input in, paint ops out, and file work out
//! as [`Job`]s the shell runs on the host's offload workers. Nothing here
//! touches a window, so tests and `--render` draw it with the software
//! rasterizer (`render.rs`).
//!
//! The C++ BitmapEditor's window had three parts, kept here: the picture at
//! its own scale (`BitmapView`), the picture as a grid of 16x12 cells, one per
//! pixel, each blended by its alpha over a dark backdrop (`PixelView`), and a
//! readout of the pixel under the mouse with colour swatches. A left click on
//! a cell set that pixel's alpha from an edit box, here a slider. Its grid
//! drew every pixel, which only works for icons; here the grid shows what fits
//! and pans (wheel, or press and drag on the picture), and the picture panel
//! outlines the part the grid shows.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use loadngo_host_core::{DecodedImage, InputSnapshot};
use ui_core::{
    ButtonModel, Color, FileDialogModel, FileDialogOutcome, FileDialogPlace, FileTypeFilter,
    HorizontalAlign, Key, Menu, MenuBar, MenuBarModel, MenuCommand, MenuItem, PaintOp, Point,
    PointerButton, Rect, Shortcut, SliderModel, StdDirectorySource, TextLayoutMode, TextOverflow,
    TextStyle, TextVerticalMetricMode, UiEvent, VerticalAlign, WidgetAction, WidgetId,
};

use crate::picture::{self, Format, Opened, SaveRequest, Saved, Source};

/// The key the frame being edited is uploaded under.
pub const IMAGE_KEY: &str = "bitmap-editor-frame";
/// One pixel's cell in the grid, as the C++ `PixelView`'s zoom (16x12).
pub const CELL_WIDTH: u32 = 16;
pub const CELL_HEIGHT: u32 = 12;

/// The grid's backdrop: the C++ brush `0x000f0503`, a COLORREF, so blue 0x0f.
const BACKDROP: [u8; 3] = [0x03, 0x05, 0x0f];
/// Behind the picture at its own scale: the C++ `BitmapView` brush `0xe9e9e9`.
const PICTURE_FILL: Color = Color::rgba(0xe9, 0xe9, 0xe9, 0xff);
/// The outline of the part the grid shows: the C++ pen `0x00ff3385`.
const REGION: Color = Color::rgba(0x85, 0x33, 0xff, 0xff);
const WINDOW: Color = Color::rgba(0x1b, 0x1e, 0x25, 0xff);
const PANEL: Color = Color::rgba(0x24, 0x28, 0x31, 0xff);
const PANEL_EDGE: Color = Color::rgba(0x3a, 0x41, 0x4f, 0xff);
const TEXT: Color = Color::rgba(0xe3, 0xe7, 0xee, 0xff);
const DIM_TEXT: Color = Color::rgba(0x9a, 0xa3, 0xb3, 0xff);
const ERROR_TEXT: Color = Color::rgba(0xff, 0x8a, 0x7a, 0xff);

const TOOLBAR_HEIGHT: f32 = 52.0;
const STATUS_HEIGHT: f32 = 26.0;
const READOUT_HEIGHT: f32 = 236.0;
const GAP: f32 = 8.0;
const INSET: f32 = 8.0;
const FONT: u16 = 14;
/// Undo keeps whole-frame copies (pre-multiply, make opaque) up to this many
/// bytes, dropping the oldest steps beyond it.
const UNDO_BUDGET: usize = 256 << 20;

/// Width of `text` at a font size, from the host's fonts.
pub type Measure<'a> = &'a dyn Fn(&str, u16) -> f32;

/// What the menus and their shortcuts do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Open,
    Save,
    SaveAs,
    Close,
    Quit,
    Undo,
    Redo,
    Premultiply,
    MakeOpaque,
    PreviousFrame,
    NextFrame,
}

impl Command {
    const ALL: [Self; 11] = [
        Self::Open,
        Self::Save,
        Self::SaveAs,
        Self::Close,
        Self::Quit,
        Self::Undo,
        Self::Redo,
        Self::Premultiply,
        Self::MakeOpaque,
        Self::PreviousFrame,
        Self::NextFrame,
    ];

    pub fn menu_command(self) -> MenuCommand {
        MenuCommand(Self::ALL.iter().position(|c| *c == self).unwrap_or(0) as u32 + 1)
    }

    pub fn from_menu(command: MenuCommand) -> Option<Self> {
        Self::ALL.get((command.0 as usize).checked_sub(1)?).copied()
    }
}

/// File work for the offload workers. [`Job::perform`] blocks (reads,
/// decodes, runs ffmpeg, writes), so it never runs on the frame thread.
#[derive(Debug)]
pub enum Job {
    Open {
        generation: u64,
        path: PathBuf,
    },
    Frame {
        generation: u64,
        source: Arc<Source>,
        frame: usize,
    },
    Save {
        generation: u64,
        state: u64,
        request: SaveRequest,
    },
}

/// A finished [`Job`], for [`Editor::apply`].
#[derive(Debug)]
pub enum JobResult {
    Opened {
        generation: u64,
        result: Result<Opened, String>,
    },
    Frame {
        generation: u64,
        source: Arc<Source>,
        frame: usize,
        result: Result<DecodedImage, String>,
    },
    Saved {
        generation: u64,
        state: u64,
        result: Result<Saved, String>,
    },
}

impl Job {
    /// Runs the job. A decoder that panics on a malformed file becomes an
    /// error result, so the editor always hears back.
    pub fn perform(self) -> JobResult {
        match self {
            Self::Open { generation, path } => JobResult::Opened {
                generation,
                result: contained(|| picture::open(&path, 0)),
            },
            Self::Frame {
                generation,
                source,
                frame,
            } => {
                let result = contained(|| picture::load_frame(&source, frame));
                JobResult::Frame {
                    generation,
                    source,
                    frame,
                    result,
                }
            }
            Self::Save {
                generation,
                state,
                request,
            } => JobResult::Saved {
                generation,
                state,
                result: contained(|| picture::save(&request)),
            },
        }
    }
}

fn contained<T>(work: impl FnOnce() -> anyhow::Result<T>) -> Result<T, String> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)) {
        Ok(result) => result.map_err(|error| format!("{error:#}")),
        Err(payload) => Err(payload
            .downcast_ref::<&str>()
            .map(|text| (*text).to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .map_or_else(
                || "the decoder failed".to_string(),
                |text| format!("the decoder failed: {text}"),
            )),
    }
}

/// What a frame asks of the shell.
#[derive(Debug, Default)]
pub struct FrameOutcome {
    pub quit: bool,
    /// The menus changed; hand them to the host again.
    pub menu_bar: Option<MenuBar>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Layout {
    surface: Rect,
    toolbar: Rect,
    status: Rect,
    picture: Rect,
    picture_image: Rect,
    readout: Rect,
    grid_panel: Rect,
    grid: Rect,
    columns: u32,
    rows: u32,
    frame_label: Rect,
}

/// One undoable edit. Undo and redo swap `change` with what the frame holds,
/// so each step keeps one copy.
#[derive(Debug)]
struct Step {
    frame: usize,
    change: Change,
    before: u64,
    after: u64,
}

#[derive(Debug)]
enum Change {
    Pixel { x: u32, y: u32, other: [u8; 4] },
    Whole(DecodedImage),
}

impl Change {
    fn bytes(&self) -> usize {
        match self {
            Self::Pixel { .. } => 16,
            Self::Whole(image) => image.rgba8.len(),
        }
    }
}

#[derive(Debug)]
struct Document {
    source: Arc<Source>,
    frame: usize,
    image: DecodedImage,
    /// A frame being decoded to replace the one shown.
    loading: Option<usize>,
    /// Edited frames not shown now; they are kept until the file is closed.
    stash: BTreeMap<usize, DecodedImage>,
    edited: BTreeSet<usize>,
    undo: VecDeque<Step>,
    redo: Vec<Step>,
    undo_bytes: usize,
    /// Each edit makes a new state; the document is unsaved while its state
    /// is not the one last saved.
    state: u64,
    saved_state: u64,
    next_state: u64,
}

impl Document {
    fn new(opened: Opened) -> Self {
        Self {
            source: opened.source,
            frame: opened.frame,
            image: opened.image,
            loading: None,
            stash: BTreeMap::new(),
            edited: BTreeSet::new(),
            undo: VecDeque::new(),
            redo: Vec::new(),
            undo_bytes: 0,
            state: 0,
            saved_state: 0,
            next_state: 1,
        }
    }

    fn unsaved(&self) -> bool {
        self.state != self.saved_state
    }

    fn name(&self) -> String {
        file_name(&self.source.path)
    }

    /// Whether Save can write the file it came from: a still, or an animated
    /// GIF (saved with every frame).
    fn saves_in_place(&self) -> bool {
        self.source.format.writable()
            && (self.source.frames == 1 || self.source.format == Format::Gif)
    }

    fn record(&mut self, change: Change) {
        let before = self.state;
        self.state = self.next_state;
        self.next_state += 1;
        self.undo_bytes += change.bytes();
        self.undo.push_back(Step {
            frame: self.frame,
            change,
            before,
            after: self.state,
        });
        self.redo.clear();
        self.edited.insert(self.frame);
        while self.undo_bytes > UNDO_BUDGET {
            let Some(oldest) = self.undo.pop_front() else {
                break;
            };
            self.undo_bytes -= oldest.change.bytes();
        }
    }

    /// Shows `frame` if it is stashed (it was edited), keeping the current
    /// frame in the stash if it was edited too.
    fn show_stashed(&mut self, frame: usize) -> bool {
        if frame == self.frame {
            return true;
        }
        let Some(image) = self.stash.remove(&frame) else {
            return false;
        };
        let shown = std::mem::replace(&mut self.image, image);
        if self.edited.contains(&self.frame) {
            self.stash.insert(self.frame, shown);
        }
        self.frame = frame;
        self.loading = None;
        true
    }

    /// Undo or redo `step` on its frame, which becomes the one shown.
    fn swap(&mut self, step: &mut Step) -> bool {
        if !self.show_stashed(step.frame) {
            return false;
        }
        match &mut step.change {
            Change::Pixel { x, y, other } => {
                if let Some(now) = picture::set_pixel(&mut self.image, *x, *y, *other) {
                    *other = now;
                }
            }
            Change::Whole(other) => std::mem::swap(&mut self.image, other),
        }
        true
    }

    fn edited_frames(&self) -> Vec<(usize, DecodedImage)> {
        self.edited
            .iter()
            .filter_map(|&frame| {
                let image = if frame == self.frame {
                    &self.image
                } else {
                    self.stash.get(&frame)?
                };
                Some((frame, image.clone()))
            })
            .collect()
    }
}

/// What to do once unsaved changes are saved or discarded.
#[derive(Debug, Clone, PartialEq)]
enum Pending {
    OpenDialog,
    Open(PathBuf),
    Close,
    Quit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DialogFor {
    Open,
    SaveAs,
}

struct Confirm {
    pending: Pending,
    bounds: Rect,
    message: String,
    save: Option<ButtonModel>,
    discard: ButtonModel,
    cancel: ButtonModel,
}

const PREVIOUS_ID: WidgetId = WidgetId(1);
const NEXT_ID: WidgetId = WidgetId(2);
const ALPHA_ID: WidgetId = WidgetId(3);
const SAVE_ID: WidgetId = WidgetId(4);
const DISCARD_ID: WidgetId = WidgetId(5);
const CANCEL_ID: WidgetId = WidgetId(6);

pub struct Editor {
    doc: Option<Document>,
    hover: Option<(u32, u32)>,
    origin: (u32, u32),
    layout: Layout,
    drawn_menu: Option<MenuBarModel>,
    shown_menu: Option<MenuBar>,
    previous: ButtonModel,
    next: ButtonModel,
    alpha: SliderModel,
    dialog: Option<(FileDialogModel, DialogFor)>,
    dialog_gliding: bool,
    confirm: Option<Confirm>,
    status: String,
    status_is_error: bool,
    pointer: Point,
    dragging_picture: bool,
    grid_pressed: bool,
    pan_remainder: (f32, f32),
    /// Bumped whenever the pixels shown change, so the shell re-uploads.
    revision: u64,
    next_generation: u64,
    decode_latest: u64,
    decode_running: bool,
    decode_queued: Option<Job>,
    save_running: bool,
    outbox: Vec<Job>,
    after_save: Option<Pending>,
    quitting: bool,
}

impl Editor {
    /// `native_menu`: the host shows the menus itself (macOS); otherwise the
    /// window draws a menu bar.
    pub fn new(native_menu: bool) -> Self {
        let mut alpha = SliderModel::with_id(ALPHA_ID, "", Rect::default(), 0.0, 255.0, 255.0);
        alpha.text = alpha_label(255);
        let mut editor = Self {
            doc: None,
            hover: None,
            origin: (0, 0),
            layout: Layout::default(),
            drawn_menu: None,
            shown_menu: None,
            previous: ButtonModel::with_id(PREVIOUS_ID, "Previous", Rect::default()),
            next: ButtonModel::with_id(NEXT_ID, "Next", Rect::default()),
            alpha,
            dialog: None,
            dialog_gliding: false,
            confirm: None,
            status: "Open a picture or a video: File > Open, or drop one on the window."
                .to_string(),
            status_is_error: false,
            pointer: Point::default(),
            dragging_picture: false,
            grid_pressed: false,
            pan_remainder: (0.0, 0.0),
            revision: 0,
            next_generation: 1,
            decode_latest: 0,
            decode_running: false,
            decode_queued: None,
            save_running: false,
            outbox: Vec::new(),
            after_save: None,
            quitting: false,
        };
        for button in [&mut editor.previous, &mut editor.next] {
            button.font_size = FONT;
        }
        if !native_menu {
            editor.drawn_menu = Some(MenuBarModel::new(editor.menu_bar()));
        }
        editor
    }

    /// The menus, for the host's menu bar.
    pub fn menu_bar(&self) -> MenuBar {
        let item = |command: Command, title: &str| MenuItem::command(command.menu_command(), title);
        let open = self.doc.is_some();
        let (frame, frames) = self.doc.as_ref().map_or((0, 0), |doc| {
            (doc.loading.unwrap_or(doc.frame), doc.source.frames)
        });
        let mut file = vec![
            item(Command::Open, "Open...").with_shortcut(Shortcut::primary('o')),
            item(Command::Save, "Save")
                .with_shortcut(Shortcut::primary('s'))
                .enabled(open),
            item(Command::SaveAs, "Save As...")
                .with_shortcut(Shortcut::primary_shift('s'))
                .enabled(open),
            item(Command::Close, "Close").enabled(open),
        ];
        if self.drawn_menu.is_some() {
            file.push(MenuItem::Separator);
            file.push(item(Command::Quit, "Quit").with_shortcut(Shortcut::primary('q')));
        }
        let can_undo = self.doc.as_ref().is_some_and(|doc| !doc.undo.is_empty());
        let can_redo = self.doc.as_ref().is_some_and(|doc| !doc.redo.is_empty());
        let edit = vec![
            item(Command::Undo, "Undo")
                .with_shortcut(Shortcut::primary('z'))
                .enabled(can_undo),
            item(Command::Redo, "Redo")
                .with_shortcut(Shortcut::primary_shift('z'))
                .enabled(can_redo),
        ];
        let image = vec![
            item(Command::Premultiply, "Pre-Multiply Alpha").enabled(open),
            item(Command::MakeOpaque, "Make Opaque").enabled(open),
            MenuItem::Separator,
            // Page Up and Page Down (`frame_keys`): a menu shortcut takes a
            // letter, and no letter means "frame".
            item(Command::PreviousFrame, "Previous Frame").enabled(frame > 0),
            item(Command::NextFrame, "Next Frame").enabled(frame + 1 < frames),
        ];
        MenuBar {
            menus: vec![
                Menu::new("File", file),
                Menu::new("Edit", edit),
                Menu::new("Image", image),
            ],
            quit: Some(Command::Quit.menu_command()),
        }
    }

    // ----- what the shell reads ------------------------------------------

    /// The frame shown and a number that changes whenever its pixels do.
    pub fn image(&self) -> Option<(&DecodedImage, u64)> {
        self.doc.as_ref().map(|doc| (&doc.image, self.revision))
    }

    /// The open file and the frame shown.
    pub fn shown(&self) -> Option<(&Source, usize)> {
        self.doc.as_ref().map(|doc| (&*doc.source, doc.frame))
    }

    pub fn hovered(&self) -> Option<(u32, u32)> {
        self.hover
    }

    /// Where the hovered pixel's cell is drawn, if the grid shows it.
    pub fn hover_cell(&self) -> Option<Rect> {
        let (x, y) = self.hover?;
        self.cell_rect(x, y)
    }

    /// File work to start now. At most one decode and one save run at once;
    /// a newer decode request replaces a waiting one.
    pub fn take_jobs(&mut self) -> Vec<Job> {
        std::mem::take(&mut self.outbox)
    }

    /// Whether something is moving that needs frames without input (the
    /// file dialog's scroll glide).
    pub fn animating(&self) -> bool {
        self.dialog_gliding
    }

    pub fn status(&self) -> &str {
        &self.status
    }

    // ----- driving it ------------------------------------------------------

    /// Asks to open `path` (asking first about unsaved changes).
    pub fn request_open(&mut self, path: PathBuf) {
        if self.doc.as_ref().is_some_and(Document::unsaved) {
            self.ask(Pending::Open(path));
        } else {
            self.start_open(path);
        }
    }

    /// Shows an already opened file (for `--render` and tests).
    pub fn show(&mut self, opened: Opened) {
        self.doc = Some(Document::new(opened));
        self.hover = None;
        self.origin = (0, 0);
        self.revision += 1;
        self.set_status(String::new(), false);
    }

    /// Hovers pixel (`x`, `y`) and pans the grid to show it in the middle.
    pub fn hover_pixel(&mut self, x: u32, y: u32) {
        self.hover = Some((x, y));
        self.centre_on(x, y);
    }

    /// A finished job's result.
    pub fn apply(&mut self, result: JobResult) {
        match result {
            JobResult::Opened { generation, result } => {
                if !self.decode_finished(generation) {
                    return;
                }
                match result {
                    Ok(opened) => {
                        let summary = describe(&opened.source);
                        self.show(opened);
                        self.set_status(summary, false);
                    }
                    Err(error) => self.set_status(format!("Could not open: {error}"), true),
                }
            }
            JobResult::Frame {
                generation,
                source,
                frame,
                result,
            } => {
                if !self.decode_finished(generation) {
                    return;
                }
                let Some(doc) = &mut self.doc else {
                    return;
                };
                if !Arc::ptr_eq(&doc.source, &source) || doc.loading != Some(frame) {
                    return;
                }
                doc.loading = None;
                match result {
                    Ok(image) => {
                        let shown = std::mem::replace(&mut doc.image, image);
                        if doc.edited.contains(&doc.frame) {
                            doc.stash.insert(doc.frame, shown);
                        }
                        doc.frame = frame;
                        self.revision += 1;
                        let summary = describe(&doc.source);
                        self.set_status(summary, false);
                    }
                    Err(error) => self
                        .set_status(format!("Could not read frame {}: {error}", frame + 1), true),
                }
            }
            JobResult::Saved {
                generation: _,
                state,
                result,
            } => {
                self.save_running = false;
                match result {
                    Ok(saved) => self.saved(saved, state),
                    Err(error) => {
                        self.after_save = None;
                        self.quitting = false;
                        self.set_status(format!("Could not save: {error}"), true);
                    }
                }
            }
        }
    }

    /// One frame: input, then layout. `delta_seconds` drives the file
    /// dialog's double-click timing and scroll glide.
    pub fn frame(
        &mut self,
        input: &InputSnapshot,
        surface: (f32, f32),
        delta_seconds: f32,
        measure: Measure<'_>,
    ) -> FrameOutcome {
        self.relayout(surface, measure);
        if let Some(path) = input.dropped_files.first() {
            if self.dialog.is_none() && self.confirm.is_none() {
                self.request_open(path.clone());
            }
        }
        for command in &input.menu_commands {
            if let Some(command) = Command::from_menu(*command) {
                self.run(command);
            }
        }
        self.route_input(input, delta_seconds);
        self.relayout(surface, measure);

        let mut outcome = FrameOutcome::default();
        let menu = self.menu_bar();
        if self.shown_menu.as_ref() != Some(&menu) {
            if let Some(drawn) = &mut self.drawn_menu {
                drawn.set_menu_bar(menu.clone());
            }
            self.shown_menu = Some(menu.clone());
            outcome.menu_bar = Some(menu);
        }
        outcome.quit = self.quitting && !self.save_running && self.confirm.is_none();
        outcome
    }

    pub fn run(&mut self, command: Command) {
        if self.dialog.is_some() || self.confirm.is_some() {
            return;
        }
        match command {
            Command::Open => {
                if self.doc.as_ref().is_some_and(Document::unsaved) {
                    self.ask(Pending::OpenDialog);
                } else {
                    self.show_open_dialog();
                }
            }
            Command::Save => self.save(),
            Command::SaveAs => self.show_save_dialog(),
            Command::Close => {
                if self.doc.as_ref().is_some_and(Document::unsaved) {
                    self.ask(Pending::Close);
                } else {
                    self.close();
                }
            }
            Command::Quit => {
                if self.doc.as_ref().is_some_and(Document::unsaved) {
                    self.ask(Pending::Quit);
                } else {
                    self.quitting = true;
                }
            }
            Command::Undo => self.undo(),
            Command::Redo => self.redo(),
            Command::Premultiply => self.edit_whole(picture::premultiply, "Pre-multiplied alpha"),
            Command::MakeOpaque => self.edit_whole(picture::make_opaque, "Made opaque"),
            Command::PreviousFrame => self.step_frame(-1),
            Command::NextFrame => self.step_frame(1),
        }
    }

    // ----- input -------------------------------------------------------------

    fn route_input(&mut self, input: &InputSnapshot, delta_seconds: f32) {
        let point = Point {
            x: input.mouse_x,
            y: input.mouse_y,
        };
        self.pointer = point;
        let events = input.ui_events();

        if let Some((dialog, purpose)) = &mut self.dialog {
            let purpose = *purpose;
            if input.mouse_wheel_y != 0.0 {
                dialog.scroll_wheel(point, input.mouse_wheel_y, input.mouse_wheel_precise);
            }
            self.dialog_gliding = dialog.advance(delta_seconds);
            let mut outcome = None;
            for event in events {
                if let Some(done) = dialog.handle_event(event).outcome {
                    outcome = Some(done);
                    break;
                }
            }
            if let Some(outcome) = outcome {
                self.dialog = None;
                self.dialog_gliding = false;
                self.dialog_finished(purpose, outcome);
            }
            return;
        }

        if let Some(confirm) = &mut self.confirm {
            let mut chosen = None;
            for event in &events {
                if let UiEvent::KeyPressed {
                    key: Key::Escape, ..
                } = event
                {
                    chosen = Some(CANCEL_ID);
                }
                let buttons = confirm
                    .save
                    .iter_mut()
                    .chain([&mut confirm.discard, &mut confirm.cancel]);
                for button in buttons {
                    if let Some(WidgetAction::Activate(id)) =
                        button.handle_event(event.clone()).action
                    {
                        chosen = Some(id);
                    }
                }
            }
            if let Some(id) = chosen {
                self.confirmed(id);
            }
            return;
        }

        if let Some(menu) = &mut self.drawn_menu {
            let mut consumed = false;
            let mut commands = Vec::new();
            for event in &events {
                let response = menu.handle_event(event);
                consumed |= response.consumed && !matches!(event, UiEvent::PointerMoved(_));
                commands.extend(response.command.and_then(Command::from_menu));
            }
            let open = menu.is_open();
            for command in commands {
                self.run(command);
            }
            if consumed || open {
                return;
            }
        }

        // The toolbar. Keys go to the picture, not the slider, so the arrow
        // keys move the hovered pixel.
        let mut step = 0;
        for event in &events {
            if matches!(
                event,
                UiEvent::KeyPressed { .. } | UiEvent::TextInput { .. }
            ) {
                continue;
            }
            for button in [&mut self.previous, &mut self.next] {
                if let Some(WidgetAction::Activate(id)) = button.handle_event(event.clone()).action
                {
                    step = if id == PREVIOUS_ID { -1 } else { 1 };
                }
            }
            self.alpha.handle_event(event.clone());
        }
        self.alpha.text = alpha_label(self.alpha_value());
        if step != 0 {
            self.step_frame(step);
        }
        if self.alpha.dragging {
            return;
        }

        self.picture_input(input, point);
        self.frame_keys(input);
        for event in &events {
            if let UiEvent::KeyPressed { key, modifiers } = event {
                if !(modifiers.ctrl || modifiers.meta) {
                    self.key(*key);
                }
            }
        }
    }

    fn picture_input(&mut self, input: &InputSnapshot, point: Point) {
        if self.doc.is_none() {
            return;
        }
        let primary = input.mouse_button == PointerButton::Primary;
        if let Some(pixel) = self.grid_pixel(point).or_else(|| self.picture_pixel(point)) {
            self.hover = Some(pixel);
        }
        if input.mouse_pressed && primary {
            if self.layout.picture_image.contains(point) {
                self.dragging_picture = true;
            }
            self.grid_pressed = self.grid_pixel(point).is_some();
        }
        if self.dragging_picture {
            if let Some((x, y)) = self.picture_pixel(point) {
                self.centre_on(x, y);
            }
        }
        if input.mouse_released && primary {
            if self.grid_pressed {
                if let Some((x, y)) = self.grid_pixel(point) {
                    self.set_alpha_at(x, y);
                }
            }
            self.dragging_picture = false;
            self.grid_pressed = false;
        }
        if !input.mouse_down && !input.mouse_released {
            self.dragging_picture = false;
            self.grid_pressed = false;
        }
        let wheel = (input.mouse_wheel_x, input.mouse_wheel_y);
        if wheel != (0.0, 0.0) && self.layout.grid_panel.contains(point) {
            let (mut dx, mut dy) = (-wheel.0, -wheel.1);
            if input.modifiers.shift && dx == 0.0 {
                (dx, dy) = (dy, 0.0);
            }
            let (columns, rows) = if input.mouse_wheel_precise {
                (dx / CELL_WIDTH as f32, dy / CELL_HEIGHT as f32)
            } else {
                (dx * 3.0, dy * 3.0)
            };
            self.pan_remainder.0 += columns;
            self.pan_remainder.1 += rows;
            let whole = (self.pan_remainder.0.trunc(), self.pan_remainder.1.trunc());
            self.pan_remainder.0 -= whole.0;
            self.pan_remainder.1 -= whole.1;
            self.pan(whole.0 as i64, whole.1 as i64);
        }
    }

    /// Page Up and Page Down step through frames; Home and End go to the
    /// first and last.
    fn frame_keys(&mut self, input: &InputSnapshot) {
        use loadngo_host_core::HostKey;
        for event in &input.key_events {
            if event.modifiers.ctrl || event.modifiers.meta {
                continue;
            }
            let Some(doc) = &self.doc else {
                return;
            };
            let at = doc.loading.unwrap_or(doc.frame) as i64;
            let last = doc.source.frames as i64 - 1;
            let step = match event.key {
                HostKey::PageUp => -1,
                HostKey::PageDown => 1,
                HostKey::Home => -at,
                HostKey::End => last - at,
                _ => 0,
            };
            if step != 0 {
                self.step_frame(step);
            }
        }
    }

    /// Arrow keys move the hovered pixel, keeping it in the grid; Enter or
    /// Space sets its alpha, as a click on its cell does.
    fn key(&mut self, key: Key) {
        let Some(doc) = &self.doc else {
            return;
        };
        let (width, height) = (doc.image.width, doc.image.height);
        let (x, y) = self.hover.unwrap_or(self.origin);
        let moved = match key {
            Key::Left => (x.saturating_sub(1), y),
            Key::Right => ((x + 1).min(width - 1), y),
            Key::Up => (x, y.saturating_sub(1)),
            Key::Down => (x, (y + 1).min(height - 1)),
            Key::Enter | Key::Space => {
                self.set_alpha_at(x, y);
                return;
            }
            _ => return,
        };
        self.hover = Some(moved);
        self.keep_visible(moved.0, moved.1);
    }

    // ----- editing -------------------------------------------------------------

    fn alpha_value(&self) -> u8 {
        self.alpha.value.round().clamp(0.0, 255.0) as u8
    }

    fn set_alpha_at(&mut self, x: u32, y: u32) {
        let alpha = self.alpha_value();
        let Some(doc) = &mut self.doc else {
            return;
        };
        let Some(before) = picture::set_alpha(&mut doc.image, x, y, alpha) else {
            return;
        };
        if before[3] == alpha {
            return;
        }
        doc.record(Change::Pixel {
            x,
            y,
            other: before,
        });
        self.revision += 1;
        self.set_status(format!("Pixel {x}, {y}: alpha {alpha}"), false);
    }

    fn edit_whole(&mut self, edit: fn(&mut DecodedImage), done: &str) {
        let Some(doc) = &mut self.doc else {
            return;
        };
        let before = doc.image.clone();
        edit(&mut doc.image);
        if doc.image.rgba8 == before.rgba8 {
            self.set_status(format!("{done}: nothing changed"), false);
            return;
        }
        doc.record(Change::Whole(before));
        self.revision += 1;
        self.set_status(done.to_string(), false);
    }

    fn undo(&mut self) {
        let Some(doc) = &mut self.doc else {
            return;
        };
        let Some(mut step) = doc.undo.pop_back() else {
            return;
        };
        doc.undo_bytes -= step.change.bytes();
        if doc.swap(&mut step) {
            doc.state = step.before;
            doc.redo.push(step);
            self.revision += 1;
            self.set_status("Undone".to_string(), false);
        }
    }

    fn redo(&mut self) {
        let Some(doc) = &mut self.doc else {
            return;
        };
        let Some(mut step) = doc.redo.pop() else {
            return;
        };
        if doc.swap(&mut step) {
            doc.state = step.after;
            doc.undo_bytes += step.change.bytes();
            doc.undo.push_back(step);
            self.revision += 1;
            self.set_status("Redone".to_string(), false);
        }
    }

    fn step_frame(&mut self, step: i64) {
        let Some(doc) = &mut self.doc else {
            return;
        };
        let from = doc.loading.unwrap_or(doc.frame) as i64;
        let to = from + step;
        if to < 0 || to >= doc.source.frames as i64 {
            return;
        }
        let to = to as usize;
        if doc.show_stashed(to) {
            self.revision += 1;
            let summary = describe(&doc.source);
            self.set_status(summary, false);
            return;
        }
        doc.loading = Some(to);
        let source = doc.source.clone();
        let generation = self.generation();
        self.set_status(format!("Reading frame {}...", to + 1), false);
        self.request_decode(Job::Frame {
            generation,
            source,
            frame: to,
        });
    }

    // ----- files ---------------------------------------------------------------

    fn generation(&mut self) -> u64 {
        let generation = self.next_generation;
        self.next_generation += 1;
        generation
    }

    fn request_decode(&mut self, job: Job) {
        self.decode_latest = match &job {
            Job::Open { generation, .. } | Job::Frame { generation, .. } => *generation,
            Job::Save { .. } => return,
        };
        if self.decode_running {
            self.decode_queued = Some(job);
        } else {
            self.decode_running = true;
            self.outbox.push(job);
        }
    }

    /// Starts the waiting decode, if any; whether `generation`'s result is
    /// still wanted.
    fn decode_finished(&mut self, generation: u64) -> bool {
        self.decode_running = false;
        if let Some(job) = self.decode_queued.take() {
            self.decode_running = true;
            self.outbox.push(job);
        }
        generation == self.decode_latest
    }

    fn start_open(&mut self, path: PathBuf) {
        let generation = self.generation();
        self.set_status(format!("Opening {}...", file_name(&path)), false);
        self.request_decode(Job::Open { generation, path });
    }

    fn close(&mut self) {
        self.doc = None;
        self.hover = None;
        self.revision += 1;
        self.set_status(String::new(), false);
    }

    fn save(&mut self) {
        let Some(doc) = &self.doc else {
            return;
        };
        if doc.saves_in_place() {
            let path = doc.source.path.clone();
            self.start_save(path);
        } else {
            self.show_save_dialog();
        }
    }

    fn start_save(&mut self, out: PathBuf) {
        if self.save_running {
            self.set_status("A save is still running".to_string(), true);
            return;
        }
        let Some(doc) = &self.doc else {
            return;
        };
        let request = SaveRequest {
            out: out.clone(),
            source: doc.source.clone(),
            frame: doc.frame,
            edited: doc.edited_frames(),
        };
        let state = doc.state;
        let generation = self.generation();
        self.save_running = true;
        self.set_status(format!("Saving {}...", file_name(&out)), false);
        self.outbox.push(Job::Save {
            generation,
            state,
            request,
        });
    }

    fn saved(&mut self, saved: Saved, state: u64) {
        let Some(doc) = &mut self.doc else {
            return;
        };
        let message = if saved.whole {
            doc.source = Arc::new(doc.source.saved_as(&saved));
            doc.saved_state = state;
            format!(
                "Saved {} ({} {})",
                saved.path.display(),
                saved.frames,
                if saved.frames == 1 { "frame" } else { "frames" }
            )
        } else {
            format!("Saved frame {} as {}", doc.frame + 1, saved.path.display())
        };
        let clean = !doc.unsaved();
        self.set_status(message, false);
        if let Some(pending) = self.after_save.take() {
            if clean {
                self.proceed(pending);
            }
        }
    }

    fn show_open_dialog(&mut self) {
        let start = self.start_folder();
        let filter = FileTypeFilter::new(
            "Pictures and video",
            &[
                "png", "jpg", "jpeg", "bmp", "dib", "tif", "tiff", "gif", "webp", "mp4", "m4v",
                "mov", "webm", "mkv", "avi",
            ],
        );
        let mut dialog =
            FileDialogModel::open("Open", start, Some(filter), Box::new(StdDirectorySource));
        dialog.set_places(StdDirectorySource.standard_places());
        self.dialog = Some((dialog, DialogFor::Open));
    }

    fn show_save_dialog(&mut self) {
        let Some(doc) = &self.doc else {
            return;
        };
        let stem = doc
            .source
            .path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("picture")
            .to_string();
        let suggested = if doc.saves_in_place() {
            file_name(&doc.source.path)
        } else {
            format!("{stem}-frame{}.png", doc.frame + 1)
        };
        let start = self.start_folder();
        let filter = FileTypeFilter::new(
            "Pictures",
            &["png", "jpg", "jpeg", "bmp", "tif", "tiff", "gif"],
        );
        let mut dialog = FileDialogModel::save(
            "Save As",
            start,
            &suggested,
            Some(filter),
            Box::new(StdDirectorySource),
        );
        let mut places = StdDirectorySource.standard_places();
        if let Some(folder) = self.doc.as_ref().and_then(|doc| doc.source.path.parent()) {
            places.insert(0, FileDialogPlace::new("This folder", folder));
        }
        dialog.set_places(places);
        self.dialog = Some((dialog, DialogFor::SaveAs));
    }

    fn start_folder(&self) -> PathBuf {
        self.doc
            .as_ref()
            .and_then(|doc| doc.source.path.parent().map(Path::to_path_buf))
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("/"))
    }

    fn dialog_finished(&mut self, purpose: DialogFor, outcome: FileDialogOutcome) {
        match (purpose, outcome) {
            (DialogFor::Open, FileDialogOutcome::Confirmed(path)) => self.start_open(path),
            (DialogFor::SaveAs, FileDialogOutcome::Confirmed(path)) => {
                match Format::for_saving(&path) {
                    Ok(_) => self.start_save(path),
                    Err(error) => {
                        self.after_save = None;
                        self.set_status(format!("{error:#}"), true);
                    }
                }
            }
            (_, FileDialogOutcome::Cancelled) => {
                self.after_save = None;
            }
        }
    }

    /// Asks what to do with unsaved changes before `pending`.
    fn ask(&mut self, pending: Pending) {
        let Some(doc) = &self.doc else {
            self.proceed(pending);
            return;
        };
        let message = if doc.saves_in_place() {
            format!("{} has unsaved changes.", doc.name())
        } else {
            format!(
                "{} has unsaved changes. A {} cannot be saved in place; File > Save As writes the frame as a picture.",
                doc.name(),
                doc.source.format.name().to_ascii_uppercase()
            )
        };
        let save = doc
            .saves_in_place()
            .then(|| ButtonModel::with_id(SAVE_ID, "Save", Rect::default()));
        let mut confirm = Confirm {
            pending,
            bounds: Rect::default(),
            message,
            save,
            discard: ButtonModel::with_id(DISCARD_ID, "Discard", Rect::default()),
            cancel: ButtonModel::with_id(CANCEL_ID, "Cancel", Rect::default()),
        };
        for button in confirm
            .save
            .iter_mut()
            .chain([&mut confirm.discard, &mut confirm.cancel])
        {
            button.font_size = FONT;
        }
        self.confirm = Some(confirm);
    }

    fn confirmed(&mut self, id: WidgetId) {
        let Some(confirm) = self.confirm.take() else {
            return;
        };
        match id {
            SAVE_ID => {
                self.after_save = Some(confirm.pending);
                self.save();
            }
            DISCARD_ID => {
                if let Some(doc) = &mut self.doc {
                    doc.saved_state = doc.state;
                }
                self.proceed(confirm.pending);
            }
            _ => {}
        }
    }

    fn proceed(&mut self, pending: Pending) {
        match pending {
            Pending::OpenDialog => self.show_open_dialog(),
            Pending::Open(path) => self.start_open(path),
            Pending::Close => self.close(),
            Pending::Quit => self.quitting = true,
        }
    }

    fn set_status(&mut self, text: String, error: bool) {
        self.status = text;
        self.status_is_error = error;
    }

    // ----- the grid ------------------------------------------------------------

    fn image_size(&self) -> Option<(u32, u32)> {
        self.doc
            .as_ref()
            .map(|doc| (doc.image.width, doc.image.height))
    }

    /// The pixel whose cell is under `point`.
    fn grid_pixel(&self, point: Point) -> Option<(u32, u32)> {
        let (width, height) = self.image_size()?;
        let grid = self.layout.grid;
        if !grid.contains(point) {
            return None;
        }
        let column = ((point.x - grid.x) / CELL_WIDTH as f32).floor() as u32;
        let row = ((point.y - grid.y) / CELL_HEIGHT as f32).floor() as u32;
        if column >= self.layout.columns || row >= self.layout.rows {
            return None;
        }
        let (x, y) = (self.origin.0 + column, self.origin.1 + row);
        (x < width && y < height).then_some((x, y))
    }

    /// The pixel under `point` on the picture at its own scale.
    fn picture_pixel(&self, point: Point) -> Option<(u32, u32)> {
        let (width, height) = self.image_size()?;
        let rect = self.layout.picture_image;
        if !rect.contains(point) || rect.width <= 0.0 || rect.height <= 0.0 {
            return None;
        }
        let x = ((point.x - rect.x) / rect.width * width as f32).floor() as u32;
        let y = ((point.y - rect.y) / rect.height * height as f32).floor() as u32;
        Some((x.min(width - 1), y.min(height - 1)))
    }

    fn cell_rect(&self, x: u32, y: u32) -> Option<Rect> {
        let (column, row) = (x.checked_sub(self.origin.0)?, y.checked_sub(self.origin.1)?);
        if column >= self.layout.columns || row >= self.layout.rows {
            return None;
        }
        let (width, height) = self.image_size()?;
        if x >= width || y >= height {
            return None;
        }
        Some(Rect {
            x: self.layout.grid.x + (column * CELL_WIDTH) as f32,
            y: self.layout.grid.y + (row * CELL_HEIGHT) as f32,
            width: CELL_WIDTH as f32,
            height: CELL_HEIGHT as f32,
        })
    }

    fn max_origin(&self) -> (u32, u32) {
        let (width, height) = self.image_size().unwrap_or((0, 0));
        (
            width.saturating_sub(self.layout.columns),
            height.saturating_sub(self.layout.rows),
        )
    }

    fn centre_on(&mut self, x: u32, y: u32) {
        let (max_x, max_y) = self.max_origin();
        self.origin = (
            x.saturating_sub(self.layout.columns / 2).min(max_x),
            y.saturating_sub(self.layout.rows / 2).min(max_y),
        );
    }

    fn keep_visible(&mut self, x: u32, y: u32) {
        let (columns, rows) = (self.layout.columns.max(1), self.layout.rows.max(1));
        let (mut ox, mut oy) = self.origin;
        if x < ox {
            ox = x;
        } else if x >= ox + columns {
            ox = x + 1 - columns;
        }
        if y < oy {
            oy = y;
        } else if y >= oy + rows {
            oy = y + 1 - rows;
        }
        let (max_x, max_y) = self.max_origin();
        self.origin = (ox.min(max_x), oy.min(max_y));
    }

    fn pan(&mut self, columns: i64, rows: i64) {
        let (max_x, max_y) = self.max_origin();
        let shift = |at: u32, by: i64, max: u32| (at as i64 + by).clamp(0, max as i64) as u32;
        self.origin = (
            shift(self.origin.0, columns, max_x),
            shift(self.origin.1, rows, max_y),
        );
    }

    // ----- layout and paint ------------------------------------------------------

    fn relayout(&mut self, (width, height): (f32, f32), measure: Measure<'_>) {
        let surface = Rect {
            x: 0.0,
            y: 0.0,
            width: width.max(1.0),
            height: height.max(1.0),
        };
        let mut top = 0.0;
        if let Some(menu) = &mut self.drawn_menu {
            menu.relayout(surface, |text, size| measure(text, size));
            top = MenuBarModel::height();
        }
        let toolbar = Rect {
            x: 0.0,
            y: top,
            width: surface.width,
            height: TOOLBAR_HEIGHT,
        };
        let status = Rect {
            x: 0.0,
            y: (surface.height - STATUS_HEIGHT).floor(),
            width: surface.width,
            height: STATUS_HEIGHT,
        };
        let body_top = top + TOOLBAR_HEIGHT;
        let body = Rect {
            x: GAP,
            y: body_top,
            width: (surface.width - 2.0 * GAP).max(1.0),
            height: (status.y - body_top - GAP).max(1.0),
        };
        let left = (body.width * 0.3).clamp(180.0, 360.0).floor();
        let picture = Rect {
            x: body.x,
            y: body.y,
            width: left,
            height: (body.height - READOUT_HEIGHT - GAP).max(40.0),
        };
        let readout = Rect {
            x: body.x,
            y: picture.y + picture.height + GAP,
            width: left,
            height: READOUT_HEIGHT,
        };
        let grid_panel = Rect {
            x: body.x + left + GAP,
            y: body.y,
            width: (body.width - left - GAP).max(1.0),
            height: body.height,
        };
        let inner = inset(grid_panel, INSET);
        let columns = ((inner.width / CELL_WIDTH as f32).floor() as u32).max(1);
        let rows = ((inner.height / CELL_HEIGHT as f32).floor() as u32).max(1);
        let grid = Rect {
            x: inner.x.floor(),
            y: inner.y.floor(),
            width: (columns * CELL_WIDTH) as f32,
            height: (rows * CELL_HEIGHT) as f32,
        };
        let picture_image = match self.image_size() {
            Some((w, h)) => fit(inset(picture, INSET), w, h),
            None => Rect::default(),
        };

        let button_y = (toolbar.y + (TOOLBAR_HEIGHT - 30.0) / 2.0).floor();
        self.previous.set_bounds(Rect {
            x: GAP,
            y: button_y,
            width: 84.0,
            height: 30.0,
        });
        self.next.set_bounds(Rect {
            x: GAP + 92.0,
            y: button_y,
            width: 84.0,
            height: 30.0,
        });
        let slider_width = 260.0f32.min((surface.width - 400.0).max(120.0));
        self.alpha.set_bounds(Rect {
            x: (surface.width - slider_width - 2.0 * GAP).floor(),
            y: toolbar.y + 6.0,
            width: slider_width,
            height: 40.0,
        });
        // After the frame buttons, which only show for several frames.
        let several = self.doc.as_ref().is_some_and(|doc| doc.source.frames > 1);
        let label_x = if several { GAP + 186.0 } else { 2.0 * GAP };
        let frame_label = Rect {
            x: label_x,
            y: toolbar.y,
            width: (self.alpha.bounds.x - label_x - GAP).max(0.0),
            height: TOOLBAR_HEIGHT,
        };

        self.layout = Layout {
            surface,
            toolbar,
            status,
            picture,
            picture_image,
            readout,
            grid_panel,
            grid,
            columns,
            rows,
            frame_label,
        };
        let (max_x, max_y) = self.max_origin();
        self.origin = (self.origin.0.min(max_x), self.origin.1.min(max_y));

        if let Some((dialog, _)) = &mut self.dialog {
            dialog.set_bounds(centred(surface, 760.0, 520.0));
            dialog.relayout(|text, size| measure(text, size));
        }
        if let Some(confirm) = &mut self.confirm {
            confirm.bounds = centred(surface, 480.0, 180.0);
            let b = confirm.bounds;
            let y = b.y + b.height - 30.0 - 16.0;
            let mut x = b.x + b.width - 16.0;
            for button in [
                Some(&mut confirm.cancel),
                Some(&mut confirm.discard),
                confirm.save.as_mut(),
            ]
            .into_iter()
            .flatten()
            {
                x -= 100.0;
                button.set_bounds(Rect {
                    x,
                    y,
                    width: 92.0,
                    height: 30.0,
                });
                x -= 8.0;
            }
        }
    }

    pub fn paint(&self, scene: &mut Vec<PaintOp>) {
        let layout = &self.layout;
        scene.push(PaintOp::FillRect {
            rect: layout.surface,
            color: WINDOW,
        });
        self.paint_toolbar(scene);
        self.paint_picture(scene);
        self.paint_grid(scene);
        self.paint_readout(scene);

        scene.push(PaintOp::FillRect {
            rect: layout.status,
            color: PANEL,
        });
        let status = match &self.doc {
            Some(doc) if self.status.is_empty() => describe(&doc.source),
            _ => self.status.clone(),
        };
        let colour = if self.status_is_error {
            ERROR_TEXT
        } else {
            DIM_TEXT
        };
        let line = Rect {
            x: layout.status.x + 2.0 * GAP,
            width: (layout.status.width - 4.0 * GAP).max(0.0),
            ..layout.status
        };
        text(scene, line, &status, 13, colour, HorizontalAlign::Left);

        if let Some(menu) = &self.drawn_menu {
            menu.paint(scene);
        }
        if let Some((dialog, _)) = &self.dialog {
            FileDialogModel::paint_scrim(layout.surface, scene);
            dialog.paint(scene);
        }
        if let Some(confirm) = &self.confirm {
            FileDialogModel::paint_scrim(layout.surface, scene);
            scene.push(PaintOp::FillRect {
                rect: confirm.bounds,
                color: PANEL,
            });
            scene.push(PaintOp::StrokeRect {
                rect: confirm.bounds,
                color: PANEL_EDGE,
            });
            let message = Rect {
                height: confirm.bounds.height - 70.0,
                ..inset(confirm.bounds, 16.0)
            };
            scene.push(PaintOp::Text {
                rect: message,
                clip_rect: Some(message),
                text: confirm.message.clone(),
                style: TextStyle {
                    layout_mode: TextLayoutMode::MultiLine,
                    ..style(FONT, TEXT, HorizontalAlign::Left)
                },
            });
            for button in confirm
                .save
                .iter()
                .chain([&confirm.discard, &confirm.cancel])
            {
                button.paint(scene);
            }
        }
    }

    fn paint_toolbar(&self, scene: &mut Vec<PaintOp>) {
        let layout = &self.layout;
        scene.push(PaintOp::FillRect {
            rect: layout.toolbar,
            color: PANEL,
        });
        let frames = self.doc.as_ref().map_or(0, |doc| doc.source.frames);
        if frames > 1 {
            self.previous.paint(scene);
            self.next.paint(scene);
        }
        let label = match &self.doc {
            Some(doc) if frames > 1 => {
                let shown = doc.loading.unwrap_or(doc.frame);
                let loading = if doc.loading.is_some() {
                    " (reading)"
                } else {
                    ""
                };
                format!("Frame {} of {frames}{loading}", shown + 1)
            }
            Some(doc) => format!(
                "{}  {}",
                doc.name(),
                if doc.unsaved() { "(edited)" } else { "" }
            ),
            None => String::new(),
        };
        text(
            scene,
            layout.frame_label,
            &label,
            FONT,
            TEXT,
            HorizontalAlign::Left,
        );
        self.alpha.paint(scene);
    }

    fn paint_picture(&self, scene: &mut Vec<PaintOp>) {
        let layout = &self.layout;
        panel(scene, layout.picture);
        let Some(doc) = &self.doc else {
            return;
        };
        scene.push(PaintOp::FillRect {
            rect: inset(layout.picture, INSET - 2.0),
            color: PICTURE_FILL,
        });
        scene.push(PaintOp::BlitImage {
            rect: layout.picture_image,
            image_key: IMAGE_KEY.to_string(),
        });
        // The part of the picture the grid shows.
        let image = layout.picture_image;
        let (sx, sy) = (
            image.width / doc.image.width as f32,
            image.height / doc.image.height as f32,
        );
        let shown_columns = layout
            .columns
            .min(doc.image.width - self.origin.0.min(doc.image.width));
        let shown_rows = layout
            .rows
            .min(doc.image.height - self.origin.1.min(doc.image.height));
        if shown_columns < doc.image.width || shown_rows < doc.image.height {
            scene.push(PaintOp::StrokeRect {
                rect: Rect {
                    x: image.x + self.origin.0 as f32 * sx - 1.0,
                    y: image.y + self.origin.1 as f32 * sy - 1.0,
                    width: shown_columns as f32 * sx + 2.0,
                    height: shown_rows as f32 * sy + 2.0,
                },
                color: REGION,
            });
        }
    }

    fn paint_grid(&self, scene: &mut Vec<PaintOp>) {
        let layout = &self.layout;
        panel(scene, layout.grid_panel);
        let Some(doc) = &self.doc else {
            let hint = "Open a picture or a video: File > Open, or drop one on the window.";
            text(
                scene,
                inset(layout.grid_panel, 24.0),
                hint,
                16,
                DIM_TEXT,
                HorizontalAlign::Center,
            );
            return;
        };
        let image = &doc.image;
        let columns = layout
            .columns
            .min(image.width.saturating_sub(self.origin.0));
        let rows = layout.rows.min(image.height.saturating_sub(self.origin.1));
        scene.push(PaintOp::FillRect {
            rect: Rect {
                width: (columns * CELL_WIDTH) as f32,
                height: (rows * CELL_HEIGHT) as f32,
                ..layout.grid
            },
            color: Color::rgba(BACKDROP[0], BACKDROP[1], BACKDROP[2], 0xff),
        });
        for row in 0..rows {
            for column in 0..columns {
                let (x, y) = (self.origin.0 + column, self.origin.1 + row);
                let Some(rgba) = picture::pixel(image, x, y) else {
                    continue;
                };
                scene.push(PaintOp::FillRect {
                    rect: Rect {
                        x: layout.grid.x + (column * CELL_WIDTH) as f32,
                        y: layout.grid.y + (row * CELL_HEIGHT) as f32,
                        width: CELL_WIDTH as f32,
                        height: CELL_HEIGHT as f32,
                    },
                    color: over_backdrop(rgba),
                });
            }
        }
        // The hovered cell gets a one-pixel frame on its edges in a colour
        // that stands out from it, so its middle still shows the pixel. (The
        // C++ framed it in the pixel's own colour, which vanishes on an
        // opaque pixel.)
        if let (Some(cell), Some((x, y))) = (self.hover_cell(), self.hover) {
            let shown =
                picture::pixel(image, x, y).map_or(Color::rgba(0, 0, 0, 255), over_backdrop);
            let edge = contrast(shown);
            for side in edges(cell) {
                scene.push(PaintOp::FillRect {
                    rect: side,
                    color: edge,
                });
            }
        }
    }

    fn paint_readout(&self, scene: &mut Vec<PaintOp>) {
        let layout = &self.layout;
        panel(scene, layout.readout);
        let Some(doc) = &self.doc else {
            return;
        };
        let area = inset(layout.readout, 12.0);
        let line = |index: f32| Rect {
            x: area.x,
            y: area.y + index * 20.0,
            width: area.width,
            height: 18.0,
        };
        let info = format!(
            "{} x {} {}",
            doc.image.width,
            doc.image.height,
            doc.source.format.name().to_ascii_uppercase()
        );
        text(
            scene,
            line(0.0),
            &info,
            FONT,
            DIM_TEXT,
            HorizontalAlign::Left,
        );
        let Some((x, y)) = self.hover else {
            text(
                scene,
                line(1.0),
                "Point at a pixel",
                FONT,
                DIM_TEXT,
                HorizontalAlign::Left,
            );
            return;
        };
        let Some([r, g, b, a]) = picture::pixel(&doc.image, x, y) else {
            return;
        };
        // The C++ readout: alpha, colour and the raw 32-bit value, in hex.
        let lines = [
            format!("Pixel {x}, {y}"),
            format!("Alpha 0x{a:02x} ({a})"),
            format!("Colour #{r:02x}{g:02x}{b:02x}"),
            format!("Value 0x{a:02x}{r:02x}{g:02x}{b:02x}"),
        ];
        for (index, content) in lines.iter().enumerate() {
            text(
                scene,
                line(index as f32 + 1.0),
                content,
                FONT,
                TEXT,
                HorizontalAlign::Left,
            );
        }
        // Swatches: the pixel as the grid blends it, then each channel alone.
        let top = area.y + 5.0 * 20.0 + 8.0;
        let blended = Rect {
            x: area.x,
            y: top,
            width: 48.0,
            height: 48.0,
        };
        scene.push(PaintOp::FillRect {
            rect: blended,
            color: over_backdrop([r, g, b, a]),
        });
        scene.push(PaintOp::StrokeRect {
            rect: blended,
            color: PANEL_EDGE,
        });
        let channels = [
            ("R", Color::rgba(r, 0, 0, 255)),
            ("G", Color::rgba(0, g, 0, 255)),
            ("B", Color::rgba(0, 0, b, 255)),
        ];
        for (index, (name, colour)) in channels.into_iter().enumerate() {
            let x = area.x + 60.0 + index as f32 * 40.0;
            let swatch = Rect {
                x,
                y: top,
                width: 28.0,
                height: 28.0,
            };
            scene.push(PaintOp::FillRect {
                rect: swatch,
                color: colour,
            });
            scene.push(PaintOp::StrokeRect {
                rect: swatch,
                color: PANEL_EDGE,
            });
            let label = Rect {
                x,
                y: top + 30.0,
                width: 28.0,
                height: 18.0,
            };
            text(scene, label, name, 12, DIM_TEXT, HorizontalAlign::Center);
        }
    }
}

/// The pixel as the grid shows it: blended by its alpha over the backdrop.
fn over_backdrop([r, g, b, a]: [u8; 4]) -> Color {
    let a = u32::from(a);
    let mix =
        |c: u8, under: u8| ((u32::from(c) * a + u32::from(under) * (255 - a) + 127) / 255) as u8;
    Color::rgba(
        mix(r, BACKDROP[0]),
        mix(g, BACKDROP[1]),
        mix(b, BACKDROP[2]),
        255,
    )
}

/// Black on a light colour, white on a dark one.
fn contrast(colour: Color) -> Color {
    let luma = 299 * u32::from(colour.r) + 587 * u32::from(colour.g) + 114 * u32::from(colour.b);
    if luma > 128_000 {
        Color::rgba(0, 0, 0, 255)
    } else {
        Color::rgba(255, 255, 255, 255)
    }
}

/// The four one-pixel edges inside `rect`.
fn edges(rect: Rect) -> [Rect; 4] {
    [
        Rect {
            height: 1.0,
            ..rect
        },
        Rect {
            y: rect.y + rect.height - 1.0,
            height: 1.0,
            ..rect
        },
        Rect { width: 1.0, ..rect },
        Rect {
            x: rect.x + rect.width - 1.0,
            width: 1.0,
            ..rect
        },
    ]
}

/// `width` x `height` fitted inside `area`, centred, on whole pixels: shrunk
/// to fit, or enlarged by a whole factor when small.
fn fit(area: Rect, width: u32, height: u32) -> Rect {
    if width == 0 || height == 0 || area.width <= 0.0 || area.height <= 0.0 {
        return Rect::default();
    }
    let scale = (area.width / width as f32).min(area.height / height as f32);
    let scale = if scale >= 1.0 { scale.floor() } else { scale };
    let (w, h) = (
        (width as f32 * scale).floor().max(1.0),
        (height as f32 * scale).floor().max(1.0),
    );
    Rect {
        x: (area.x + (area.width - w) / 2.0).floor(),
        y: (area.y + (area.height - h) / 2.0).floor(),
        width: w,
        height: h,
    }
}

fn inset(rect: Rect, by: f32) -> Rect {
    Rect {
        x: rect.x + by,
        y: rect.y + by,
        width: (rect.width - 2.0 * by).max(0.0),
        height: (rect.height - 2.0 * by).max(0.0),
    }
}

fn centred(surface: Rect, width: f32, height: f32) -> Rect {
    let (w, h) = (
        width.min(surface.width - 32.0).max(1.0),
        height.min(surface.height - 32.0).max(1.0),
    );
    Rect {
        x: ((surface.width - w) / 2.0).floor(),
        y: ((surface.height - h) / 2.0).floor(),
        width: w,
        height: h,
    }
}

fn panel(scene: &mut Vec<PaintOp>, rect: Rect) {
    scene.push(PaintOp::FillRect { rect, color: PANEL });
    scene.push(PaintOp::StrokeRect {
        rect,
        color: PANEL_EDGE,
    });
}

fn style(size: u16, color: Color, align: HorizontalAlign) -> TextStyle {
    TextStyle {
        color,
        font_size: size,
        horizontal_align: align,
        vertical_align: VerticalAlign::Middle,
        vertical_metric_mode: TextVerticalMetricMode::LogicalLineBox,
        layout_mode: TextLayoutMode::SingleLine,
        overflow: TextOverflow::EllipsisEnd,
    }
}

fn text(
    scene: &mut Vec<PaintOp>,
    rect: Rect,
    content: &str,
    size: u16,
    color: Color,
    align: HorizontalAlign,
) {
    if content.is_empty() || rect.width <= 0.0 {
        return;
    }
    scene.push(PaintOp::Text {
        rect,
        clip_rect: Some(rect),
        text: content.to_string(),
        style: style(size, color, align),
    });
}

fn alpha_label(alpha: u8) -> String {
    format!("Alpha for clicks: {alpha} (0x{alpha:02x})")
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// `640 x 480 PNG, photo.png` and, for several frames, how many.
fn describe(source: &Source) -> String {
    let frames = if source.frames > 1 {
        format!(", {} frames", source.frames)
    } else {
        String::new()
    };
    format!(
        "{} x {} {}{frames}: {}",
        source.width,
        source.height,
        source.format.name().to_ascii_uppercase(),
        source.path.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::approximate_width;
    use loadngo_host_core::InputSnapshot;

    const SURFACE: (f32, f32) = (1000.0, 700.0);

    /// A `width` x `height` PNG in `dir` whose pixel (x, y) is
    /// (x, y, 100, 255), opened.
    fn png(dir: &Path, width: u32, height: u32) -> Opened {
        let mut rgba = Vec::new();
        for y in 0..height {
            for x in 0..width {
                rgba.extend_from_slice(&[x as u8, y as u8, 100, 255]);
            }
        }
        let path = dir.join("pattern.png");
        let image = DecodedImage::new(width, height, rgba);
        std::fs::write(&path, picture::encode(&image, &Format::Png).unwrap()).unwrap();
        picture::open(&path, 0).unwrap()
    }

    /// A three-frame GIF in `dir`: red, green, blue, opened at frame 0.
    fn gif(dir: &Path) -> Opened {
        let frames: Vec<image::Frame> = [[255u8, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255]]
            .iter()
            .map(|colour| {
                image::Frame::new(image::RgbaImage::from_raw(4, 3, colour.repeat(12)).unwrap())
            })
            .collect();
        let mut bytes = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
            encoder.encode_frames(frames).unwrap();
        }
        let path = dir.join("anim.gif");
        std::fs::write(&path, bytes).unwrap();
        picture::open(&path, 0).unwrap()
    }

    fn editor_with(opened: Opened) -> Editor {
        let mut editor = Editor::new(true);
        editor.show(opened);
        idle(&mut editor);
        editor
    }

    fn idle(editor: &mut Editor) -> FrameOutcome {
        editor.frame(
            &InputSnapshot::default(),
            SURFACE,
            0.016,
            &approximate_width,
        )
    }

    fn input_at(point: Point) -> InputSnapshot {
        InputSnapshot {
            mouse_x: point.x,
            mouse_y: point.y,
            ..InputSnapshot::default()
        }
    }

    fn click(editor: &mut Editor, point: Point) {
        let mut press = input_at(point);
        press.mouse_pressed = true;
        press.mouse_down = true;
        editor.frame(&press, SURFACE, 0.016, &approximate_width);
        let mut release = input_at(point);
        release.mouse_released = true;
        editor.frame(&release, SURFACE, 0.016, &approximate_width);
    }

    fn centre(rect: Rect) -> Point {
        Point {
            x: rect.x + rect.width / 2.0,
            y: rect.y + rect.height / 2.0,
        }
    }

    /// Runs every waiting job, as the offload workers would, and applies the
    /// results in order.
    fn run_jobs(editor: &mut Editor) -> usize {
        let mut ran = 0;
        loop {
            let jobs = editor.take_jobs();
            if jobs.is_empty() {
                return ran;
            }
            for job in jobs {
                ran += 1;
                editor.apply(job.perform());
            }
        }
    }

    fn pixel_at(editor: &Editor, x: u32, y: u32) -> [u8; 4] {
        picture::pixel(editor.image().unwrap().0, x, y).unwrap()
    }

    #[test]
    fn a_click_on_a_cell_sets_that_pixels_alpha_and_undo_brings_it_back() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor_with(png(dir.path(), 20, 10));
        editor.alpha.set_value(128.0);
        let cell = editor.cell_rect(3, 4).unwrap();
        let revision = editor.image().unwrap().1;
        click(&mut editor, centre(cell));
        assert_eq!(pixel_at(&editor, 3, 4), [3, 4, 100, 128]);
        assert_eq!(pixel_at(&editor, 4, 4), [4, 4, 100, 255]);
        assert_eq!(editor.hovered(), Some((3, 4)));
        assert!(editor.image().unwrap().1 > revision, "the shell re-uploads");
        assert!(editor.doc.as_ref().unwrap().unsaved());

        editor.run(Command::Undo);
        assert_eq!(pixel_at(&editor, 3, 4), [3, 4, 100, 255]);
        assert!(!editor.doc.as_ref().unwrap().unsaved(), "back to the file");
        editor.run(Command::Redo);
        assert_eq!(pixel_at(&editor, 3, 4), [3, 4, 100, 128]);

        editor.run(Command::Premultiply);
        assert_eq!(pixel_at(&editor, 3, 4), [1, 2, 50, 128]);
        editor.run(Command::MakeOpaque);
        assert_eq!(pixel_at(&editor, 3, 4), [1, 2, 50, 255]);
        editor.run(Command::Undo);
        editor.run(Command::Undo);
        assert_eq!(pixel_at(&editor, 3, 4), [3, 4, 100, 128]);
    }

    #[test]
    fn frames_are_read_through_jobs_and_edits_survive_leaving_a_frame() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor_with(gif(dir.path()));
        assert!(editor.take_jobs().is_empty());
        editor.run(Command::NextFrame);
        assert_eq!(editor.shown().unwrap().1, 0, "frame 1 is still being read");
        assert_eq!(run_jobs(&mut editor), 1);
        assert_eq!(editor.shown().unwrap().1, 1);
        assert_eq!(pixel_at(&editor, 0, 0), [0, 255, 0, 255]);

        editor.alpha.set_value(10.0);
        editor.set_alpha_at(2, 1);
        editor.run(Command::NextFrame);
        assert_eq!(run_jobs(&mut editor), 1);
        assert_eq!(pixel_at(&editor, 2, 1), [0, 0, 255, 255]);
        // Back to the edited frame: no read, the edit is still there.
        editor.run(Command::PreviousFrame);
        assert_eq!(run_jobs(&mut editor), 0);
        assert_eq!(editor.shown().unwrap().1, 1);
        assert_eq!(pixel_at(&editor, 2, 1), [0, 255, 0, 10]);
        // Undo from another frame goes back to the frame it changed.
        editor.run(Command::NextFrame);
        run_jobs(&mut editor);
        editor.run(Command::Undo);
        assert_eq!(editor.shown().unwrap().1, 1);
        assert_eq!(pixel_at(&editor, 2, 1), [0, 255, 0, 255]);
    }

    #[test]
    fn one_read_runs_at_a_time_and_a_newer_request_replaces_a_waiting_one() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor_with(gif(dir.path()));
        editor.run(Command::NextFrame);
        let first = editor.take_jobs();
        assert_eq!(first.len(), 1);
        editor.run(Command::NextFrame);
        assert!(
            editor.take_jobs().is_empty(),
            "the second waits for the first"
        );
        editor.run(Command::PreviousFrame);
        assert!(editor.take_jobs().is_empty());
        for job in first {
            editor.apply(job.perform());
        }
        // The first read (frame 1) was overtaken; the waiting one (frame 1
        // again, after next then previous) starts.
        assert_eq!(editor.shown().unwrap().1, 0);
        assert_eq!(run_jobs(&mut editor), 1);
        assert_eq!(editor.shown().unwrap().1, 1);
    }

    #[test]
    fn save_writes_the_edit_and_quitting_with_unsaved_changes_asks_first() {
        let dir = tempfile::tempdir().unwrap();
        let opened = png(dir.path(), 8, 8);
        let path = opened.source.path.clone();
        let mut editor = editor_with(opened);
        editor.alpha.set_value(7.0);
        editor.set_alpha_at(1, 1);
        editor.run(Command::Save);
        assert_eq!(run_jobs(&mut editor), 1);
        assert!(!editor.doc.as_ref().unwrap().unsaved());
        let reopened = picture::open(&path, 0).unwrap();
        assert_eq!(picture::pixel(&reopened.image, 1, 1), Some([1, 1, 100, 7]));

        editor.set_alpha_at(2, 2);
        editor.run(Command::Quit);
        assert!(!idle(&mut editor).quit, "it asks first");
        assert!(editor.confirm.is_some());
        let cancel = editor.confirm.as_ref().unwrap().cancel.bounds;
        click(&mut editor, centre(cancel));
        assert!(editor.confirm.is_none() && !idle(&mut editor).quit);

        editor.run(Command::Quit);
        idle(&mut editor);
        let discard = editor.confirm.as_ref().unwrap().discard.bounds;
        click(&mut editor, centre(discard));
        assert!(idle(&mut editor).quit);
    }

    #[test]
    fn the_wheel_pans_the_grid_and_arrow_keys_move_the_hovered_pixel() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor_with(png(dir.path(), 200, 150));
        assert_eq!(editor.origin, (0, 0));
        let (columns, rows) = (editor.layout.columns, editor.layout.rows);
        assert!(columns < 200 && rows < 150, "{columns}x{rows}");
        let mut wheel = input_at(centre(editor.layout.grid));
        wheel.mouse_wheel_y = -2.0;
        editor.frame(&wheel, SURFACE, 0.016, &approximate_width);
        assert_eq!(editor.origin, (0, 6), "two notches, three rows each");
        wheel.mouse_wheel_y = -1000.0;
        editor.frame(&wheel, SURFACE, 0.016, &approximate_width);
        assert_eq!(editor.origin.1, 150 - rows, "stops at the bottom");

        editor.hover_pixel(0, 0);
        idle(&mut editor);
        assert_eq!(editor.origin, (0, 0));
        let mut key = key_input(loadngo_host_core::HostKey::Left, false);
        editor.frame(&key, SURFACE, 0.016, &approximate_width);
        assert_eq!(editor.hovered(), Some((0, 0)), "stops at the edge");
        for _ in 0..columns {
            key.key_events[0].key = loadngo_host_core::HostKey::Right;
            editor.frame(&key, SURFACE, 0.016, &approximate_width);
        }
        assert_eq!(editor.hovered(), Some((columns, 0)));
        assert_eq!(editor.origin.0, 1, "the grid follows the hovered pixel");
    }

    #[test]
    fn pressing_and_dragging_on_the_picture_moves_the_part_the_grid_shows() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor_with(png(dir.path(), 300, 200));
        let picture = editor.layout.picture_image;
        let point = Point {
            x: picture.x + picture.width * 0.75,
            y: picture.y + picture.height * 0.75,
        };
        let mut press = input_at(point);
        press.mouse_pressed = true;
        press.mouse_down = true;
        editor.frame(&press, SURFACE, 0.016, &approximate_width);
        let (x, y) = editor.picture_pixel(point).unwrap();
        assert!(
            (224..226).contains(&x) && (149..151).contains(&y),
            "{x},{y}"
        );
        // The grid is centred on that pixel.
        let cell = editor.cell_rect(x, y).unwrap();
        let grid = editor.layout.grid;
        assert!((cell.x - (grid.x + grid.width / 2.0)).abs() <= CELL_WIDTH as f32);
        assert!((cell.y - (grid.y + grid.height / 2.0)).abs() <= CELL_HEIGHT as f32);
        assert_eq!(editor.hovered(), Some((x, y)));
    }

    fn key_input(key: loadngo_host_core::HostKey, ctrl: bool) -> InputSnapshot {
        let mut input = InputSnapshot::default();
        input.key_events.push(loadngo_host_core::HostKeyEvent {
            key,
            modifiers: ui_core::Modifiers {
                ctrl,
                ..Default::default()
            },
        });
        input
    }

    #[test]
    fn a_window_without_a_system_menu_bar_draws_one_and_its_shortcuts_work() {
        use loadngo_host_core::HostKey;
        let dir = tempfile::tempdir().unwrap();
        let mut editor = Editor::new(false);
        editor.show(gif(dir.path()));
        idle(&mut editor);
        assert_eq!(editor.layout.toolbar.y, MenuBarModel::height());
        editor.set_alpha_at(0, 0);
        editor.alpha.set_value(0.0);
        editor.set_alpha_at(1, 1);
        assert_eq!(pixel_at(&editor, 1, 1), [255, 0, 0, 0]);
        // The menus catch up at the end of the frame that made the edit.
        idle(&mut editor);
        editor.frame(
            &key_input(HostKey::Z, true),
            SURFACE,
            0.016,
            &approximate_width,
        );
        assert_eq!(pixel_at(&editor, 1, 1), [255, 0, 0, 255], "Ctrl-Z undid it");

        // Page Down, End and Home step through the frames.
        editor.frame(
            &key_input(HostKey::PageDown, false),
            SURFACE,
            0.016,
            &approximate_width,
        );
        assert_eq!(run_jobs(&mut editor), 1);
        assert_eq!(editor.shown().unwrap().1, 1);
        editor.frame(
            &key_input(HostKey::End, false),
            SURFACE,
            0.016,
            &approximate_width,
        );
        run_jobs(&mut editor);
        assert_eq!(editor.shown().unwrap().1, 2);
        editor.frame(
            &key_input(HostKey::Home, false),
            SURFACE,
            0.016,
            &approximate_width,
        );
        run_jobs(&mut editor);
        assert_eq!(editor.shown().unwrap().1, 0);
    }
}
