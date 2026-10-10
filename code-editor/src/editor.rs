//! The editor: an open folder, one tab per open file, and the bars and
//! buttons around them. It never touches the filesystem or the window:
//! input comes in through [`Editor::frame`], file work goes out as
//! [`IoRequest`]s and comes back through [`Editor::apply`], and the scene
//! goes out through [`Editor::paint`]. The app runs the requests on the
//! host's offload workers.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use loadngo_host_core::InputSnapshot;
use ui_core::{
    FileDialogModel, FileDialogOutcome, FileDialogPlace, HorizontalAlign, Key, Menu, MenuBar,
    MenuBarModel, MenuCommand, MenuItem, Modifiers, PaintOp, Point, PointerButton, PointerState,
    Rect, Shortcut, StdDirectorySource, TextAreaModel, TextFieldModel, UiEvent,
};

use crate::draw;
use crate::file_tree::{FileTree, TreeAction};
use crate::fs_ops::{self, DiskStamp, IoRequest, IoResponse, ReadError, WriteError};
use crate::session::{self, Backup, Session, SessionTab};
use crate::text_file::LineEnding;
use crate::theme;

/// What the editor needs from the window it runs in.
pub trait EditorHost {
    /// Width in pixels of `text` drawn at `font_size`.
    fn measure(&self, text: &str, font_size: u16) -> f32;
    fn read_clipboard(&self) -> Option<String>;
    fn write_clipboard(&self, text: &str);
}

/// How long the caret blinks after the last input before it holds still,
/// so an untouched editor needs no frames at all.
const BLINK_FOR: Duration = Duration::from_secs(10);
const BLINK_HALF_PERIOD_MS: u128 = 530;
/// Unsaved text is backed up this long after typing stops.
const BACKUP_DELAY: Duration = Duration::from_secs(2);
/// The session is written this long after the last change to it.
const SESSION_DELAY: Duration = Duration::from_secs(2);
/// Indentation the Tab key inserts.
const INDENT: &str = "    ";

struct Buffer {
    id: u64,
    path: PathBuf,
    title: String,
    area: TextAreaModel,
    /// False until the file's contents arrive.
    loaded: bool,
    saved_revision: u64,
    line_ending: LineEnding,
    /// The file on disk as last read or written; `None` once a conflict was
    /// resolved in favor of this buffer, so the next save writes regardless.
    disk: Option<DiskStamp>,
    /// The file changed on disk while this buffer had unsaved edits.
    conflict: bool,
    /// The revision being written, while a save is in flight.
    saving: Option<u64>,
    /// Close this tab once the save in flight succeeds.
    close_after_save: bool,
    seen_revision: u64,
    backup_revision: Option<u64>,
    has_backup_file: bool,
    /// A backup that arrived before the file did.
    pending_backup: Option<String>,
    restore_caret: Option<usize>,
}

impl Buffer {
    fn new(id: u64, path: PathBuf) -> Self {
        let title = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        Self {
            id,
            path,
            title,
            area: code_area(""),
            loaded: false,
            saved_revision: 0,
            line_ending: LineEnding::Lf,
            disk: None,
            conflict: false,
            saving: None,
            close_after_save: false,
            seen_revision: 0,
            backup_revision: None,
            has_backup_file: false,
            pending_backup: None,
            restore_caret: None,
        }
    }

    fn dirty(&self) -> bool {
        self.loaded && self.area.revision() != self.saved_revision
    }

    /// Replaces the whole text as one undoable edit, keeping the caret near
    /// where it was.
    fn replace_text(&mut self, text: &str) {
        if self.area.text() == text {
            return;
        }
        let caret = self.area.caret();
        let len = self.area.document.len_chars();
        self.area.select_range(0, len);
        if text.is_empty() {
            let _ = self.area.handle_event(UiEvent::KeyPressed {
                key: Key::Backspace,
                modifiers: Modifiers::default(),
            });
        } else {
            self.area.insert_text(text);
        }
        self.area.set_caret(caret);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Editor,
    Find,
    Goto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    Save,
    SaveAll,
    Find,
    GoToLine,
    Refresh,
    OpenFolder,
    CloseTab,
    Quit,
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    SelectAll,
    QuitSaveAll,
    QuitDiscard,
    QuitCancel,
    PromptSave,
    PromptDiscard,
    PromptCancel,
    ConflictKeepMine,
    ConflictReload,
    FindPrevious,
    FindNext,
    CloseBar,
    GotoGo,
}

impl Command {
    /// The commands a menu item can send.
    const IN_MENUS: [Command; 14] = [
        Command::OpenFolder,
        Command::Refresh,
        Command::Save,
        Command::SaveAll,
        Command::CloseTab,
        Command::Quit,
        Command::Undo,
        Command::Redo,
        Command::Cut,
        Command::Copy,
        Command::Paste,
        Command::SelectAll,
        Command::Find,
        Command::GoToLine,
    ];

    fn menu_command(self) -> MenuCommand {
        MenuCommand(self as u32)
    }

    fn from_menu(command: MenuCommand) -> Option<Self> {
        Self::IN_MENUS
            .into_iter()
            .find(|candidate| candidate.menu_command() == command)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatusKind {
    Info,
    Warning,
    Error,
}

struct FindBar {
    field: TextFieldModel,
    /// Matches as character ranges, for `matches_for`.
    matches: Vec<(usize, usize)>,
    matches_for: Option<(u64, u64, String)>,
}

struct TabSlot {
    rect: Rect,
    close: Rect,
}

#[derive(Default)]
struct Layout {
    toolbar: Rect,
    buttons: Vec<(Command, Rect)>,
    tree: Rect,
    tabs: Rect,
    tab_slots: Vec<TabSlot>,
    bar: Option<Rect>,
    bar_buttons: Vec<(Command, Rect)>,
    bar_field: Option<Rect>,
    editor: Rect,
    status: Rect,
}

/// What a frame asks of the window.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FrameOutcome {
    /// Show the text (I-beam) pointer.
    pub text_cursor: bool,
    /// The menus changed (titles or enabled states): give them to the
    /// host's system menu bar. Only after [`Editor::set_native_menu`].
    pub menu_bar: Option<MenuBar>,
    /// The editor has finished: every save asked for is done and the
    /// session is written once the pending file work completes.
    pub quit: bool,
}

pub struct Editor {
    state_dir: Option<PathBuf>,
    tree: Option<FileTree>,
    /// Waiting for the saved session before choosing a folder.
    awaiting_session: bool,
    fallback_root: PathBuf,
    restore_active: Option<PathBuf>,
    buffers: Vec<Buffer>,
    active: Option<usize>,
    next_id: u64,
    outbox: Vec<IoRequest>,
    focus: Focus,
    find: Option<FindBar>,
    goto: Option<TextFieldModel>,
    close_prompt: Option<u64>,
    status: Option<(String, StatusKind)>,
    layout: Layout,
    pointer: Point,
    window_focused: bool,
    last_input: Instant,
    blink_origin: Instant,
    backup_due: Option<Instant>,
    session_due: Option<Instant>,
    written_session: Option<Vec<u8>>,
    /// The menu bar drawn in the window, when the host has no system one.
    drawn_menu: Option<MenuBarModel>,
    /// The menus as last given to the system menu bar.
    published_menu: Option<MenuBar>,
    native_menu: bool,
    folder_dialog: Option<FileDialogModel>,
    dialog_clock: Instant,
    dialog_animating: bool,
    quit_prompt: bool,
    quit_after_save: bool,
    quitting: bool,
}

impl Editor {
    /// An editor showing `root`, or else the folder of the saved session in
    /// `state_dir`, or else `fallback_root`. With no `state_dir` nothing is
    /// remembered.
    pub fn new(
        state_dir: Option<PathBuf>,
        root: Option<PathBuf>,
        fallback_root: PathBuf,
        now: Instant,
    ) -> Self {
        let mut editor = Self {
            state_dir,
            tree: None,
            awaiting_session: false,
            fallback_root,
            restore_active: None,
            buffers: Vec::new(),
            active: None,
            next_id: 1,
            outbox: Vec::new(),
            focus: Focus::Editor,
            find: None,
            goto: None,
            close_prompt: None,
            status: None,
            layout: Layout::default(),
            pointer: Point { x: -1.0, y: -1.0 },
            window_focused: true,
            last_input: now,
            blink_origin: now,
            backup_due: None,
            session_due: None,
            written_session: None,
            drawn_menu: Some(MenuBarModel::default()),
            published_menu: None,
            native_menu: false,
            folder_dialog: None,
            dialog_clock: now,
            dialog_animating: false,
            quit_prompt: false,
            quit_after_save: false,
            quitting: false,
        };
        if let Some(root) = &root {
            editor.open_folder(root.clone());
        }
        match editor.state_dir.clone() {
            Some(state_dir) => {
                editor.awaiting_session = true;
                editor.outbox.push(IoRequest::ReadState {
                    path: session::session_path(&state_dir),
                });
            }
            None if root.is_none() => {
                let fallback = editor.fallback_root.clone();
                editor.open_folder(fallback);
            }
            None => {}
        }
        editor
    }

    /// File work to run, oldest first.
    pub fn take_requests(&mut self) -> Vec<IoRequest> {
        if let Some(tree) = &mut self.tree {
            for dir in tree.take_listing_requests() {
                self.outbox.push(IoRequest::ListDir { dir });
            }
        }
        std::mem::take(&mut self.outbox)
    }

    /// How long until the editor next needs a frame with no input: the
    /// caret's blink or a pending backup or session write. `None` when
    /// nothing is due.
    pub fn next_wake(&self, now: Instant) -> Option<Duration> {
        let mut wake: Option<Instant> = None;
        let mut consider = |at: Instant| {
            wake = Some(wake.map_or(at, |current| current.min(at)));
        };
        if self.caret_blinking(now) {
            let elapsed = now.duration_since(self.blink_origin).as_millis();
            let next = (elapsed / BLINK_HALF_PERIOD_MS + 1) * BLINK_HALF_PERIOD_MS;
            consider(self.blink_origin + Duration::from_millis(next as u64));
        }
        if let Some(at) = self.backup_due {
            consider(at);
        }
        if let Some(at) = self.session_due {
            consider(at);
        }
        if self.dialog_animating {
            consider(now + Duration::from_millis(16));
        }
        wake.map(|at| at.saturating_duration_since(now))
    }

    fn caret_blinking(&self, now: Instant) -> bool {
        self.window_focused
            && now.duration_since(self.last_input) < BLINK_FOR
            && (self.focus != Focus::Editor || self.active_buffer().is_some_and(|b| b.loaded))
    }

    fn active_buffer(&self) -> Option<&Buffer> {
        self.active.and_then(|index| self.buffers.get(index))
    }

    fn active_buffer_mut(&mut self) -> Option<&mut Buffer> {
        self.active.and_then(|index| self.buffers.get_mut(index))
    }

    fn buffer_index(&self, id: u64) -> Option<usize> {
        self.buffers.iter().position(|buffer| buffer.id == id)
    }

    fn set_status(&mut self, text: impl Into<String>, kind: StatusKind) {
        self.status = Some((text.into(), kind));
    }

    // ----- folders, files and tabs -----

    fn open_folder(&mut self, root: PathBuf) {
        self.tree = Some(FileTree::new(root));
        self.touch_session();
    }

    /// Opens `path` in a tab (or switches to its tab).
    pub fn open_file(&mut self, path: PathBuf) {
        if let Some(index) = self.buffers.iter().position(|buffer| buffer.path == path) {
            self.activate(index);
            return;
        }
        let id = self.next_id;
        self.next_id += 1;
        self.buffers.push(Buffer::new(id, path.clone()));
        self.outbox.push(IoRequest::ReadFile { path });
        self.activate(self.buffers.len() - 1);
    }

    fn activate(&mut self, index: usize) {
        if let Some(previous) = self.active_buffer_mut() {
            let _ = previous.area.handle_event(UiEvent::FocusChanged(false));
        }
        self.active = Some(index);
        self.focus = Focus::Editor;
        let path = self.buffers[index].path.clone();
        let area = &mut self.buffers[index].area;
        let _ = area.handle_event(UiEvent::FocusChanged(true));
        if let Some(tree) = &mut self.tree {
            tree.selected = Some(path);
        }
        self.touch_session();
    }

    fn request_close(&mut self, index: usize) {
        if self.buffers[index].dirty() {
            self.activate(index);
            self.close_prompt = Some(self.buffers[index].id);
        } else {
            self.close(index);
        }
    }

    fn close(&mut self, index: usize) {
        let buffer = self.buffers.remove(index);
        if buffer.has_backup_file {
            if let Some(state_dir) = &self.state_dir {
                self.outbox.push(IoRequest::RemoveState {
                    path: session::backup_path(state_dir, &buffer.path),
                });
            }
        }
        if self.close_prompt == Some(buffer.id) {
            self.close_prompt = None;
        }
        self.active = match self.active {
            _ if self.buffers.is_empty() => None,
            Some(active) if active > index => Some(active - 1),
            Some(active) if active == index => Some(index.min(self.buffers.len() - 1)),
            other => other,
        };
        if let Some(active) = self.active {
            self.activate(active);
        }
        self.touch_session();
    }

    fn save(&mut self, index: usize) {
        let buffer = &mut self.buffers[index];
        if !buffer.loaded || buffer.saving.is_some() {
            return;
        }
        let revision = buffer.area.revision();
        buffer.saving = Some(revision);
        self.outbox.push(IoRequest::WriteFile {
            path: buffer.path.clone(),
            bytes: fs_ops::file_bytes(&buffer.area.text(), buffer.line_ending),
            expected: buffer.disk,
            revision,
        });
    }

    fn save_all(&mut self) {
        let dirty: Vec<usize> = (0..self.buffers.len())
            .filter(|&index| self.buffers[index].dirty())
            .collect();
        if dirty.is_empty() {
            self.set_status("Nothing to save.", StatusKind::Info);
        }
        for index in dirty {
            self.save(index);
        }
    }

    // ----- results of file work -----

    /// Takes in the result of a request from [`Editor::take_requests`].
    pub fn apply(&mut self, response: IoResponse, now: Instant) {
        match response {
            IoResponse::Listed { dir, entries } => {
                if let Some(tree) = &mut self.tree {
                    tree.apply_listing(&dir, entries);
                }
            }
            IoResponse::Read { path, result } => self.apply_read(path, result),
            IoResponse::Written {
                path,
                revision,
                result,
            } => self.apply_written(&path, revision, result),
            IoResponse::StateRead { path, result } => self.apply_state_read(&path, result),
            IoResponse::StateWritten { path, result } => {
                if let Err(error) = result {
                    self.set_status(
                        format!("Could not write {}: {error}", path.display()),
                        StatusKind::Error,
                    );
                }
            }
            IoResponse::Stats { stamps } => self.apply_stats(stamps),
        }
        self.after_change(now);
    }

    fn apply_read(
        &mut self,
        path: PathBuf,
        result: Result<(crate::text_file::DecodedText, DiskStamp), ReadError>,
    ) {
        let Some(index) = self.buffers.iter().position(|buffer| buffer.path == path) else {
            return;
        };
        match result {
            Ok((decoded, stamp)) => {
                let buffer = &mut self.buffers[index];
                if buffer.loaded {
                    // A reload after the file changed on disk.
                    buffer.replace_text(&decoded.text);
                    buffer.conflict = false;
                    self.status = Some((format!("Reloaded {}.", buffer.title), StatusKind::Info));
                } else {
                    let focused = buffer.area.focused;
                    buffer.area = code_area(&decoded.text);
                    let _ = buffer.area.handle_event(UiEvent::FocusChanged(focused));
                    buffer.loaded = true;
                    if let Some(caret) = buffer.restore_caret.take() {
                        buffer.area.set_caret(caret);
                    }
                }
                buffer.line_ending = decoded.line_ending;
                buffer.disk = Some(stamp);
                buffer.saved_revision = buffer.area.revision();
                buffer.seen_revision = buffer.saved_revision;
                if let Some(text) = buffer.pending_backup.take() {
                    Self::restore_backup(buffer, &text);
                }
                if self.restore_active.as_ref() == Some(&path) {
                    self.restore_active = None;
                    self.activate(index);
                }
            }
            Err(error) => {
                let buffer = &self.buffers[index];
                if buffer.loaded {
                    self.set_status(
                        format!("Could not reload {}: {error}", buffer.title),
                        StatusKind::Error,
                    );
                    return;
                }
                let title = buffer.title.clone();
                self.close(index);
                let message = match error {
                    ReadError::NotText(reason) => format!("{title} was not opened: {reason}."),
                    ReadError::Io(message) => format!("Could not open {title}: {message}"),
                };
                self.set_status(message, StatusKind::Warning);
            }
        }
    }

    fn restore_backup(buffer: &mut Buffer, text: &str) {
        let caret = buffer.area.caret();
        buffer.replace_text(text);
        buffer.area.set_caret(caret);
        buffer.has_backup_file = true;
        buffer.backup_revision = Some(buffer.area.revision());
        buffer.seen_revision = buffer.area.revision();
    }

    fn apply_written(&mut self, path: &Path, revision: u64, result: Result<DiskStamp, WriteError>) {
        let Some(index) = self.buffers.iter().position(|buffer| buffer.path == path) else {
            return;
        };
        let buffer = &mut self.buffers[index];
        buffer.saving = None;
        match result {
            Ok(stamp) => {
                buffer.saved_revision = revision;
                buffer.disk = Some(stamp);
                buffer.conflict = false;
                let title = buffer.title.clone();
                let close = buffer.close_after_save && !buffer.dirty();
                if buffer.has_backup_file && !buffer.dirty() {
                    buffer.has_backup_file = false;
                    buffer.backup_revision = None;
                    if let Some(state_dir) = &self.state_dir {
                        self.outbox.push(IoRequest::RemoveState {
                            path: session::backup_path(state_dir, path),
                        });
                    }
                }
                if close {
                    self.close(index);
                }
                self.set_status(format!("Saved {title}."), StatusKind::Info);
                if self.quit_after_save
                    && self
                        .buffers
                        .iter()
                        .all(|buffer| !buffer.dirty() && buffer.saving.is_none())
                {
                    self.quit_now();
                }
            }
            Err(WriteError::ChangedOnDisk) => {
                self.quit_after_save = false;
                let buffer = &mut self.buffers[index];
                buffer.conflict = true;
                buffer.close_after_save = false;
                let title = buffer.title.clone();
                self.activate(index);
                self.set_status(
                    format!("{title} changed on disk since it was opened; not saved."),
                    StatusKind::Warning,
                );
            }
            Err(WriteError::Io(error)) => {
                self.quit_after_save = false;
                let buffer = &mut self.buffers[index];
                buffer.close_after_save = false;
                let title = buffer.title.clone();
                self.set_status(
                    format!("Could not save {title}: {error}"),
                    StatusKind::Error,
                );
            }
        }
    }

    fn apply_state_read(&mut self, path: &Path, result: Result<Option<Vec<u8>>, String>) {
        let Some(state_dir) = self.state_dir.clone() else {
            return;
        };
        let bytes = match result {
            Ok(bytes) => bytes,
            Err(error) => {
                self.set_status(
                    format!("Could not read {}: {error}", path.display()),
                    StatusKind::Warning,
                );
                None
            }
        };
        if path == session::session_path(&state_dir) {
            self.restore_session(bytes.and_then(|bytes| serde_json::from_slice(&bytes).ok()));
            return;
        }
        let Some(backup) = bytes.and_then(|bytes| serde_json::from_slice::<Backup>(&bytes).ok())
        else {
            return;
        };
        let Some(buffer) = self
            .buffers
            .iter_mut()
            .find(|buffer| buffer.path == backup.path)
        else {
            return;
        };
        if buffer.loaded {
            Self::restore_backup(buffer, &backup.text);
        } else {
            buffer.pending_backup = Some(backup.text);
        }
        let title = buffer.title.clone();
        self.set_status(
            format!("Restored unsaved changes to {title}."),
            StatusKind::Info,
        );
    }

    fn restore_session(&mut self, session: Option<Session>) {
        if !self.awaiting_session {
            return;
        }
        self.awaiting_session = false;
        let Some(session) = session else {
            if self.tree.is_none() {
                let fallback = self.fallback_root.clone();
                self.open_folder(fallback);
            }
            return;
        };
        match &self.tree {
            Some(tree) if tree.root() != session.root => return,
            Some(_) => {}
            None => self.open_folder(session.root.clone()),
        }
        if let Some(tree) = &mut self.tree {
            for dir in &session.expanded {
                tree.set_expanded(dir, true);
            }
        }
        let state_dir = self.state_dir.clone();
        for tab in &session.tabs {
            self.open_file(tab.path.clone());
            if let Some(buffer) = self.buffers.last_mut() {
                buffer.restore_caret = Some(tab.caret);
            }
            if let Some(state_dir) = &state_dir {
                self.outbox.push(IoRequest::ReadState {
                    path: session::backup_path(state_dir, &tab.path),
                });
            }
        }
        self.restore_active = session
            .active
            .and_then(|index| session.tabs.get(index))
            .map(|tab| tab.path.clone());
        if let Some(index) = session.active.filter(|&index| index < self.buffers.len()) {
            self.activate(index);
        }
    }

    fn apply_stats(&mut self, stamps: Vec<(PathBuf, Option<DiskStamp>)>) {
        for (path, stamp) in stamps {
            let Some(index) = self.buffers.iter().position(|buffer| buffer.path == path) else {
                continue;
            };
            let buffer = &mut self.buffers[index];
            if !buffer.loaded || buffer.saving.is_some() || buffer.disk.is_none() {
                continue;
            }
            match stamp {
                None => {
                    let title = buffer.title.clone();
                    self.set_status(
                        format!("{title} no longer exists on disk; saving recreates it."),
                        StatusKind::Warning,
                    );
                    self.buffers[index].disk = None;
                }
                Some(stamp) if Some(stamp) != buffer.disk => {
                    if buffer.dirty() {
                        buffer.conflict = true;
                    } else {
                        self.outbox.push(IoRequest::ReadFile { path });
                    }
                }
                Some(_) => {}
            }
        }
    }

    // ----- frames -----

    /// Handles one frame of input and lays out for painting.
    pub fn frame(
        &mut self,
        input: &InputSnapshot,
        window_focused: bool,
        surface: (f32, f32),
        now: Instant,
        host: &dyn EditorHost,
    ) -> FrameOutcome {
        if window_focused && !self.window_focused {
            self.check_disk();
        }
        self.window_focused = window_focused;
        self.relayout(surface, host);

        let had_input = input.mouse_pressed
            || input.mouse_released
            || !input.key_events.is_empty()
            || !input.typed_text.is_empty()
            || input.mouse_wheel_y != 0.0
            || input.mouse_wheel_x != 0.0;
        if had_input {
            self.last_input = now;
            self.blink_origin = now;
        }

        for &command in &input.menu_commands {
            if let Some(command) = Command::from_menu(command) {
                self.run(command, host);
            }
        }
        if self.folder_dialog.is_some() {
            self.route_dialog(input, now);
        } else {
            self.route_pointer(input, host);
            for event in &input.key_events {
                self.route_key(event.key, event.modifiers, host);
            }
            if !input.typed_text.is_empty() {
                self.route_text(&input.typed_text);
            }
        }

        self.relayout(surface, host);
        self.update_find_matches();
        let blink_on = !self.caret_blinking(now)
            || (now.duration_since(self.blink_origin).as_millis() / BLINK_HALF_PERIOD_MS)
                .is_multiple_of(2);
        let focus = self.focus;
        if let Some(buffer) = self.active_buffer_mut() {
            buffer.area.show_caret = buffer.area.focused && focus == Focus::Editor && blink_on;
        }
        if let Some(find) = &mut self.find {
            find.field.area.show_caret = focus == Focus::Find && blink_on;
        }
        if let Some(goto) = &mut self.goto {
            goto.area.show_caret = focus == Focus::Goto && blink_on;
        }
        if had_input {
            self.touch_session();
        }
        self.after_change(now);
        self.run_due(now);
        FrameOutcome {
            text_cursor: self.wants_text_cursor(),
            menu_bar: self.publish_menu(),
            quit: self.quitting,
        }
    }

    /// Tells the editor the host shows its menus in the system menu bar, so
    /// it draws none and takes chosen items from the frame's input.
    pub fn set_native_menu(&mut self, native: bool) {
        self.native_menu = native;
        self.drawn_menu = if native {
            None
        } else {
            Some(MenuBarModel::new(self.menu_bar()))
        };
        // The host shows whatever was set before this call; publish again
        // so the menus match the kind of menu bar (Quit moves).
        self.published_menu = None;
    }

    /// The menus as they stand: File and Edit, with each item enabled only
    /// when it can act.
    pub fn menu_bar(&self) -> MenuBar {
        let any_dirty = self.buffers.iter().any(Buffer::dirty);
        let active_dirty = self.active_buffer().is_some_and(Buffer::dirty);
        let has_file = self.active_buffer().is_some_and(|buffer| buffer.loaded);
        let editing = self.focused_area_ref().is_some() && self.folder_dialog.is_none();
        let item = |command: Command, title: &str| MenuItem::command(command.menu_command(), title);
        let mut file = vec![
            item(Command::OpenFolder, "Open Folder…").with_shortcut(Shortcut::primary('o')),
            item(Command::Refresh, "Refresh Folder").enabled(self.tree.is_some()),
            MenuItem::Separator,
            item(Command::Save, "Save")
                .with_shortcut(Shortcut::primary('s'))
                .enabled(active_dirty),
            item(Command::SaveAll, "Save All")
                .with_shortcut(Shortcut::primary_shift('s'))
                .enabled(any_dirty),
            MenuItem::Separator,
            item(Command::CloseTab, "Close Tab")
                .with_shortcut(Shortcut::primary('w'))
                .enabled(self.active.is_some()),
        ];
        if !self.native_menu {
            // The system's application menu holds Quit where there is one.
            file.push(MenuItem::Separator);
            file.push(item(Command::Quit, "Quit").with_shortcut(Shortcut::primary('q')));
        }
        let edit = vec![
            item(Command::Undo, "Undo")
                .with_shortcut(Shortcut::primary('z'))
                .enabled(editing),
            item(Command::Redo, "Redo")
                .with_shortcut(Shortcut::primary_shift('z'))
                .enabled(editing),
            MenuItem::Separator,
            item(Command::Cut, "Cut")
                .with_shortcut(Shortcut::primary('x'))
                .enabled(editing),
            item(Command::Copy, "Copy")
                .with_shortcut(Shortcut::primary('c'))
                .enabled(editing),
            item(Command::Paste, "Paste")
                .with_shortcut(Shortcut::primary('v'))
                .enabled(editing),
            item(Command::SelectAll, "Select All")
                .with_shortcut(Shortcut::primary('a'))
                .enabled(editing),
            MenuItem::Separator,
            item(Command::Find, "Find…")
                .with_shortcut(Shortcut::primary('f'))
                .enabled(has_file),
            item(Command::GoToLine, "Go to Line…").enabled(has_file),
        ];
        MenuBar {
            menus: vec![Menu::new("File", file), Menu::new("Edit", edit)],
            quit: Some(Command::Quit.menu_command()),
        }
    }

    /// Brings the menus up to date; returns them when the system menu bar
    /// needs them.
    fn publish_menu(&mut self) -> Option<MenuBar> {
        let menu = self.menu_bar();
        if self.published_menu.as_ref() == Some(&menu) {
            return None;
        }
        self.published_menu = Some(menu.clone());
        match &mut self.drawn_menu {
            Some(drawn) => {
                drawn.set_menu_bar(menu);
                None
            }
            None => Some(menu),
        }
    }

    /// Input while the Open Folder dialog is up: all of it goes to the
    /// dialog.
    fn route_dialog(&mut self, input: &InputSnapshot, now: Instant) {
        let Some(dialog) = &mut self.folder_dialog else {
            return;
        };
        let delta = now.duration_since(self.dialog_clock).as_secs_f32();
        self.dialog_clock = now;
        self.dialog_animating = dialog.advance(delta);
        let point = Point {
            x: input.mouse_x,
            y: input.mouse_y,
        };
        self.pointer = point;
        let state = PointerState::mouse(point, input.modifiers);
        let mut events = vec![UiEvent::PointerMoved(state)];
        if input.mouse_pressed {
            events.push(UiEvent::PointerPressed {
                button: PointerButton::Primary,
                state,
            });
        }
        if input.mouse_released {
            events.push(UiEvent::PointerReleased {
                button: PointerButton::Primary,
                state,
            });
        }
        for event in &input.key_events {
            if let Some(key) = event.key.ui_key() {
                events.push(UiEvent::KeyPressed {
                    key,
                    modifiers: event.modifiers,
                });
            }
        }
        if !input.typed_text.is_empty() {
            events.push(UiEvent::TextInput {
                text: input.typed_text.clone(),
            });
        }
        if input.mouse_wheel_y != 0.0 {
            dialog.scroll_wheel(point, input.mouse_wheel_y, input.mouse_wheel_precise);
            self.dialog_animating = true;
        }
        let mut outcome = None;
        for event in events {
            if let Some(result) = dialog.handle_event(event).outcome {
                outcome = Some(result);
                break;
            }
        }
        match outcome {
            Some(FileDialogOutcome::Confirmed(folder)) => {
                self.folder_dialog = None;
                self.dialog_animating = false;
                self.set_status(format!("Opened {}.", folder.display()), StatusKind::Info);
                self.open_folder(folder);
            }
            Some(FileDialogOutcome::Cancelled) => {
                self.folder_dialog = None;
                self.dialog_animating = false;
            }
            None => {}
        }
    }

    fn show_folder_dialog(&mut self) {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let start = self
            .tree
            .as_ref()
            .and_then(|tree| tree.root().parent().map(Path::to_path_buf))
            .or_else(|| home.clone())
            .unwrap_or_else(|| PathBuf::from("/"));
        let mut dialog =
            FileDialogModel::choose_folder("Open Folder", start, Box::new(StdDirectorySource));
        let mut places = Vec::new();
        if let Some(home) = home {
            places.push(FileDialogPlace::new("Home", home));
        }
        if let Some(tree) = &self.tree {
            places.push(FileDialogPlace::new("This folder", tree.root()));
        }
        dialog.set_places(places);
        self.folder_dialog = Some(dialog);
        self.dialog_animating = false;
    }

    /// Quits now: the session is written at once, and the app stops once
    /// the file work in flight completes.
    fn quit_now(&mut self) {
        self.quit_prompt = false;
        self.quit_after_save = false;
        self.quitting = true;
        if self.state_dir.is_some() {
            self.session_due = Some(Instant::now());
            self.run_due(Instant::now());
        }
    }

    fn wants_text_cursor(&self) -> bool {
        let pointer = self.pointer;
        if let Some(buffer) = self.active_buffer() {
            if buffer.area.drag_selecting || buffer.area.prefers_text_cursor(pointer) {
                return true;
            }
        }
        self.find
            .as_ref()
            .is_some_and(|find| find.field.prefers_text_cursor(pointer))
            || self
                .goto
                .as_ref()
                .is_some_and(|goto| goto.prefers_text_cursor(pointer))
    }

    /// Asks for the disk state of every open file (on regaining focus).
    fn check_disk(&mut self) {
        let paths: Vec<PathBuf> = self
            .buffers
            .iter()
            .filter(|buffer| buffer.loaded)
            .map(|buffer| buffer.path.clone())
            .collect();
        if !paths.is_empty() {
            self.outbox.push(IoRequest::StatFiles { paths });
        }
    }

    fn touch_session(&mut self) {
        if self.state_dir.is_some() && !self.awaiting_session {
            self.session_due
                .get_or_insert(Instant::now() + SESSION_DELAY);
        }
    }

    /// Notices edits since the last frame and schedules their backup.
    fn after_change(&mut self, now: Instant) {
        let mut edited = false;
        for buffer in &mut self.buffers {
            let revision = buffer.area.revision();
            if buffer.loaded && revision != buffer.seen_revision {
                buffer.seen_revision = revision;
                edited = true;
            }
        }
        if edited && self.state_dir.is_some() {
            self.backup_due = Some(now + BACKUP_DELAY);
        }
    }

    fn run_due(&mut self, now: Instant) {
        let Some(state_dir) = self.state_dir.clone() else {
            return;
        };
        if self.backup_due.is_some_and(|due| now >= due) {
            self.backup_due = None;
            for buffer in &mut self.buffers {
                if !buffer.loaded {
                    continue;
                }
                let path = session::backup_path(&state_dir, &buffer.path);
                if buffer.dirty() {
                    let revision = buffer.area.revision();
                    if buffer.backup_revision != Some(revision) {
                        let backup = Backup {
                            path: buffer.path.clone(),
                            text: buffer.area.text(),
                        };
                        if let Ok(bytes) = serde_json::to_vec(&backup) {
                            self.outbox.push(IoRequest::WriteState { path, bytes });
                            buffer.backup_revision = Some(revision);
                            buffer.has_backup_file = true;
                        }
                    }
                } else if buffer.has_backup_file {
                    self.outbox.push(IoRequest::RemoveState { path });
                    buffer.has_backup_file = false;
                    buffer.backup_revision = None;
                }
            }
        }
        if self.session_due.is_some_and(|due| now >= due) {
            self.session_due = None;
            if let Some(session) = self.session() {
                if let Ok(bytes) = serde_json::to_vec_pretty(&session) {
                    if self.written_session.as_ref() != Some(&bytes) {
                        self.written_session = Some(bytes.clone());
                        self.outbox.push(IoRequest::WriteState {
                            path: session::session_path(&state_dir),
                            bytes,
                        });
                    }
                }
            }
        }
    }

    fn session(&self) -> Option<Session> {
        let tree = self.tree.as_ref()?;
        Some(Session {
            root: tree.root().to_path_buf(),
            tabs: self
                .buffers
                .iter()
                .map(|buffer| SessionTab {
                    path: buffer.path.clone(),
                    caret: if buffer.loaded {
                        buffer.area.caret()
                    } else {
                        buffer.restore_caret.unwrap_or(0)
                    },
                })
                .collect(),
            active: self.active,
            expanded: tree.expanded_dirs(),
        })
    }

    // ----- layout -----

    fn relayout(&mut self, (width, height): (f32, f32), host: &dyn EditorHost) {
        let surface = Rect {
            x: 0.0,
            y: 0.0,
            width,
            height,
        };
        let top = if let Some(menu) = &mut self.drawn_menu {
            menu.relayout(surface, |text, size| host.measure(text, size));
            MenuBarModel::height()
        } else {
            0.0
        };
        if let Some(dialog) = &mut self.folder_dialog {
            let dialog_width = (width - 80.0).clamp(320.0, 760.0);
            let dialog_height = (height - 120.0).clamp(240.0, 520.0);
            dialog.set_bounds(Rect {
                x: (width - dialog_width) / 2.0,
                y: (height - dialog_height) / 2.0,
                width: dialog_width,
                height: dialog_height,
            });
            dialog.relayout(|text, size| host.measure(text, size));
        }
        let mut layout = Layout {
            toolbar: Rect {
                x: 0.0,
                y: top,
                width,
                height: theme::TOOLBAR_HEIGHT,
            },
            status: Rect {
                x: 0.0,
                y: height - theme::STATUS_HEIGHT,
                width,
                height: theme::STATUS_HEIGHT,
            },
            ..Layout::default()
        };
        let mut x = 8.0;
        for (command, text) in [
            (Command::OpenFolder, "Open Folder…"),
            (Command::Save, "Save"),
            (Command::SaveAll, "Save All"),
            (Command::Find, "Find"),
            (Command::GoToLine, "Go to Line"),
            (Command::Refresh, "Refresh Folder"),
        ] {
            let button_width = host.measure(text, theme::UI_FONT) + 24.0;
            layout.buttons.push((
                command,
                Rect {
                    x,
                    y: top + 6.0,
                    width: button_width,
                    height: theme::TOOLBAR_HEIGHT - 12.0,
                },
            ));
            x += button_width + 6.0;
        }
        let body_top = top + theme::TOOLBAR_HEIGHT;
        let body_height = (layout.status.y - body_top).max(0.0);
        let tree_width = theme::TREE_WIDTH.min(width * 0.4);
        layout.tree = Rect {
            x: 0.0,
            y: body_top,
            width: tree_width,
            height: body_height,
        };
        let right_x = tree_width + 1.0;
        let right_width = (width - right_x).max(0.0);
        layout.tabs = Rect {
            x: right_x,
            y: body_top,
            width: right_width,
            height: theme::TAB_HEIGHT,
        };
        self.layout_tabs(&mut layout, host);
        let mut y = body_top + theme::TAB_HEIGHT;
        if let Some(bar) = self.layout_bar(right_x, y, right_width, host, &mut layout) {
            y = bar.bottom();
            layout.bar = Some(bar);
        }
        layout.editor = Rect {
            x: right_x,
            y,
            width: right_width,
            height: (layout.status.y - y).max(0.0),
        };
        if let Some(tree) = &mut self.tree {
            tree.bounds = layout.tree;
        }
        let editor_rect = layout.editor;
        let measure = |text: &str, size: u16| host.measure(text, size);
        if let Some(buffer) = self.active_buffer_mut() {
            buffer.area.set_bounds(editor_rect);
            buffer.area.relayout(measure);
        }
        if let (Some(find), Some(field)) = (&mut self.find, layout.bar_field) {
            find.field.set_bounds(field);
            find.field.relayout(measure);
        }
        if let (Some(goto), Some(field)) = (&mut self.goto, layout.bar_field) {
            goto.set_bounds(field);
            goto.relayout(measure);
        }
        self.layout = layout;
    }

    fn layout_tabs(&self, layout: &mut Layout, host: &dyn EditorHost) {
        if self.buffers.is_empty() {
            return;
        }
        let natural: Vec<f32> = self
            .buffers
            .iter()
            .map(|buffer| (host.measure(&buffer.title, theme::UI_FONT) + 48.0).clamp(90.0, 240.0))
            .collect();
        let total: f32 = natural.iter().sum();
        let scale = if total > layout.tabs.width {
            (layout.tabs.width / total).max(0.4)
        } else {
            1.0
        };
        let mut x = layout.tabs.x;
        for width in natural {
            let width = (width * scale).floor();
            let rect = Rect {
                x,
                y: layout.tabs.y,
                width,
                height: theme::TAB_HEIGHT,
            };
            let close = Rect {
                x: rect.right() - 26.0,
                y: rect.y + (rect.height - 18.0) / 2.0,
                width: 18.0,
                height: 18.0,
            };
            layout.tab_slots.push(TabSlot { rect, close });
            x += width;
        }
    }

    /// Lays out the one bar shown under the tabs, if any: the close prompt,
    /// the conflict prompt, find or go-to-line.
    fn layout_bar(
        &self,
        x: f32,
        y: f32,
        width: f32,
        host: &dyn EditorHost,
        layout: &mut Layout,
    ) -> Option<Rect> {
        let commands: Vec<(Command, &str)> = if self.quit_prompt {
            vec![
                (Command::QuitSaveAll, "Save All and Quit"),
                (Command::QuitDiscard, "Quit Without Saving"),
                (Command::QuitCancel, "Cancel"),
            ]
        } else if self.close_prompt.is_some() {
            vec![
                (Command::PromptSave, "Save"),
                (Command::PromptDiscard, "Don't Save"),
                (Command::PromptCancel, "Cancel"),
            ]
        } else if self.active_buffer().is_some_and(|buffer| buffer.conflict) {
            vec![
                (Command::ConflictKeepMine, "Keep Mine and Save"),
                (Command::ConflictReload, "Reload from Disk"),
            ]
        } else if self.find.is_some() {
            vec![
                (Command::FindPrevious, "Previous"),
                (Command::FindNext, "Next"),
                (Command::CloseBar, "Close"),
            ]
        } else if self.goto.is_some() {
            vec![(Command::GotoGo, "Go"), (Command::CloseBar, "Close")]
        } else {
            return None;
        };
        let bar = Rect {
            x,
            y,
            width,
            height: theme::BAR_HEIGHT,
        };
        let mut right = bar.right() - 8.0;
        for (command, text) in commands.iter().rev() {
            let button_width = host.measure(text, theme::UI_FONT) + 24.0;
            right -= button_width;
            layout.bar_buttons.push((
                *command,
                Rect {
                    x: right,
                    y: bar.y + 6.0,
                    width: button_width,
                    height: bar.height - 12.0,
                },
            ));
            right -= 6.0;
        }
        layout.bar_buttons.reverse();
        let shows_field = self.find.is_some() || self.goto.is_some();
        if shows_field
            && !self.quit_prompt
            && self.close_prompt.is_none()
            && !self.active_buffer().is_some_and(|b| b.conflict)
        {
            let label_width = 110.0;
            layout.bar_field = Some(Rect {
                x: bar.x + label_width,
                y: bar.y + 5.0,
                width: (right - bar.x - label_width - 8.0).clamp(80.0, 420.0),
                height: bar.height - 10.0,
            });
        }
        Some(bar)
    }

    // ----- input -----

    fn route_pointer(&mut self, input: &InputSnapshot, host: &dyn EditorHost) {
        let point = Point {
            x: input.mouse_x,
            y: input.mouse_y,
        };
        if self.route_drawn_menu_pointer(input, point, host) {
            self.pointer = point;
            return;
        }
        let moved = point != self.pointer;
        self.pointer = point;
        let state = PointerState::mouse(point, input.modifiers);
        if moved || input.mouse_down {
            if let Some(tree) = &mut self.tree {
                tree.pointer_moved(point);
            }
            let editor_rect = self.layout.editor;
            if let Some(buffer) = self.active_buffer_mut() {
                let area = &mut buffer.area;
                if editor_rect.contains(point)
                    || area.drag_selecting
                    || area.horizontal_drag.is_some()
                    || area.vertical_drag.is_some()
                {
                    let _ = area.handle_event(UiEvent::PointerMoved(state));
                } else if area.hover {
                    let _ = area.handle_event(UiEvent::PointerLeft);
                }
            }
            for field in self.bar_fields() {
                let _ = field.handle_event(UiEvent::PointerMoved(state));
            }
        }
        if input.mouse_pressed {
            self.pointer_pressed(point, state, host);
        }
        if input.mouse_released {
            let released = UiEvent::PointerReleased {
                button: PointerButton::Primary,
                state,
            };
            if let Some(buffer) = self.active_buffer_mut() {
                let _ = buffer.area.handle_event(released.clone());
            }
            for field in self.bar_fields() {
                let _ = field.handle_event(released.clone());
            }
        }
        if input.mouse_wheel_y != 0.0 || input.mouse_wheel_x != 0.0 {
            self.wheel(point, input);
        }
    }

    fn bar_fields(&mut self) -> Vec<&mut TextFieldModel> {
        let mut fields = Vec::new();
        if let Some(find) = &mut self.find {
            fields.push(&mut find.field);
        }
        if let Some(goto) = &mut self.goto {
            fields.push(goto);
        }
        fields
    }

    /// Offers pointer input to the drawn menu bar; true when it took it.
    fn route_drawn_menu_pointer(
        &mut self,
        input: &InputSnapshot,
        point: Point,
        host: &dyn EditorHost,
    ) -> bool {
        let Some(menu) = &mut self.drawn_menu else {
            return false;
        };
        let state = PointerState::mouse(point, input.modifiers);
        let mut events = Vec::new();
        if point != self.pointer {
            events.push(UiEvent::PointerMoved(state));
        }
        if input.mouse_pressed {
            events.push(UiEvent::PointerPressed {
                button: PointerButton::Primary,
                state,
            });
        }
        if input.mouse_released {
            events.push(UiEvent::PointerReleased {
                button: PointerButton::Primary,
                state,
            });
        }
        let mut consumed = false;
        let mut chosen = None;
        for event in &events {
            let response = menu.handle_event(event);
            // Hover over a closed bar is not taken; a press or an open menu is.
            if response.consumed && !matches!(event, UiEvent::PointerMoved(_)) {
                consumed = true;
            }
            if response.consumed && menu.is_open() {
                consumed = true;
            }
            chosen = chosen.or(response.command);
        }
        if let Some(command) = chosen.and_then(Command::from_menu) {
            self.run(command, host);
            consumed = true;
        }
        consumed
    }

    fn pointer_pressed(&mut self, point: Point, state: PointerState, host: &dyn EditorHost) {
        let hit_command = self
            .layout
            .bar_buttons
            .iter()
            .chain(self.layout.buttons.iter())
            .find(|(_, rect)| rect.contains(point))
            .map(|(command, _)| *command);
        if let Some(command) = hit_command {
            self.run(command, host);
            return;
        }
        if let Some(index) = self
            .layout
            .tab_slots
            .iter()
            .position(|slot| slot.rect.contains(point))
        {
            if self.layout.tab_slots[index].close.contains(point) {
                self.request_close(index);
            } else {
                self.activate(index);
            }
            return;
        }
        if self.layout.tree.contains(point) {
            let action = self.tree.as_mut().and_then(|tree| tree.click(point));
            match action {
                Some(TreeAction::Open(path)) => self.open_file(path),
                Some(TreeAction::List(dir)) => self.outbox.push(IoRequest::ListDir { dir }),
                None => {}
            }
            self.touch_session();
            return;
        }
        let pressed = UiEvent::PointerPressed {
            button: PointerButton::Primary,
            state,
        };
        if let Some(field) = self.layout.bar_field.filter(|rect| rect.contains(point)) {
            let _ = field;
            let target = if self.find.is_some() {
                Focus::Find
            } else {
                Focus::Goto
            };
            self.set_focus(target);
            for field in self.bar_fields() {
                let _ = field.handle_event(pressed.clone());
            }
            return;
        }
        if self.layout.editor.contains(point) {
            self.set_focus(Focus::Editor);
            if let Some(buffer) = self.active_buffer_mut() {
                let _ = buffer.area.handle_event(pressed);
            }
        }
    }

    fn set_focus(&mut self, focus: Focus) {
        self.focus = focus;
        let editor_focused = focus == Focus::Editor;
        if let Some(buffer) = self.active_buffer_mut() {
            if buffer.area.focused != editor_focused {
                let _ = buffer
                    .area
                    .handle_event(UiEvent::FocusChanged(editor_focused));
            }
        }
        if let Some(find) = &mut self.find {
            let _ = find
                .field
                .handle_event(UiEvent::FocusChanged(focus == Focus::Find));
        }
        if let Some(goto) = &mut self.goto {
            let _ = goto.handle_event(UiEvent::FocusChanged(focus == Focus::Goto));
        }
    }

    fn wheel(&mut self, point: Point, input: &InputSnapshot) {
        let row = theme::TREE_ROW_HEIGHT;
        let pixels_y = if input.mouse_wheel_precise {
            -input.mouse_wheel_y
        } else {
            -input.mouse_wheel_y * row * 3.0
        };
        let pixels_x = if input.mouse_wheel_precise {
            -input.mouse_wheel_x
        } else {
            -input.mouse_wheel_x * 36.0
        };
        if self.layout.tree.contains(point) {
            if let Some(tree) = &mut self.tree {
                tree.scroll_by(pixels_y);
            }
        } else if self.layout.editor.contains(point) {
            let shift = input.modifiers.shift;
            if let Some(buffer) = self.active_buffer_mut() {
                if shift && pixels_x == 0.0 {
                    buffer.area.scroll_horizontal(pixels_y);
                } else {
                    buffer.area.scroll_vertical(pixels_y);
                    if pixels_x != 0.0 {
                        buffer.area.scroll_horizontal(pixels_x);
                    }
                }
            }
        }
    }

    fn route_key(
        &mut self,
        key: loadngo_host_core::HostKey,
        modifiers: Modifiers,
        host: &dyn EditorHost,
    ) {
        let Some(key) = key.ui_key() else {
            return;
        };
        if let Some(menu) = &mut self.drawn_menu {
            let response = menu.handle_event(&UiEvent::KeyPressed { key, modifiers });
            if let Some(command) = response.command.and_then(Command::from_menu) {
                self.run(command, host);
            }
            if response.consumed {
                return;
            }
        }
        if key == Key::Escape {
            if self.quit_prompt {
                self.quit_prompt = false;
            } else if self.close_prompt.take().is_none() {
                self.find = None;
                self.goto = None;
            }
            self.set_focus(Focus::Editor);
            return;
        }
        let event = UiEvent::KeyPressed { key, modifiers };
        match self.focus {
            Focus::Editor => {
                if let Some(buffer) = self.active_buffer_mut() {
                    if buffer.loaded {
                        let _ = buffer.area.handle_event(event);
                    }
                }
            }
            Focus::Find => {
                if key == Key::Enter {
                    let step = if modifiers.shift {
                        Command::FindPrevious
                    } else {
                        Command::FindNext
                    };
                    self.run(step, host);
                } else if let Some(find) = &mut self.find {
                    let _ = find.field.handle_event(event);
                }
            }
            Focus::Goto => {
                if key == Key::Enter {
                    self.run(Command::GotoGo, host);
                } else if let Some(goto) = &mut self.goto {
                    let _ = goto.handle_event(event);
                }
            }
        }
    }

    fn focused_area_ref(&self) -> Option<&TextAreaModel> {
        match self.focus {
            Focus::Editor => self
                .active_buffer()
                .filter(|buffer| buffer.loaded)
                .map(|buffer| &buffer.area),
            Focus::Find => self.find.as_ref().map(|find| &find.field.area),
            Focus::Goto => self.goto.as_ref().map(|goto| &goto.area),
        }
    }

    fn focused_area(&mut self) -> Option<&mut TextAreaModel> {
        match self.focus {
            Focus::Editor => self
                .active_buffer_mut()
                .filter(|buffer| buffer.loaded)
                .map(|buffer| &mut buffer.area),
            Focus::Find => self.find.as_mut().map(|find| &mut find.field.area),
            Focus::Goto => self.goto.as_mut().map(|goto| &mut goto.area),
        }
    }

    fn copy(&mut self, cut: bool, host: &dyn EditorHost) {
        let Some(area) = self.focused_area() else {
            return;
        };
        let Some((start, end)) = area.selection_range() else {
            return;
        };
        host.write_clipboard(&area.document.slice_chars(start, end));
        if cut {
            let _ = area.handle_event(UiEvent::KeyPressed {
                key: Key::Backspace,
                modifiers: Modifiers::default(),
            });
        }
    }

    fn paste(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n");
        match self.focus {
            Focus::Editor => {
                if let Some(area) = self.focused_area() {
                    area.insert_text(&text);
                }
            }
            Focus::Find | Focus::Goto => self.route_text(&text),
        }
    }

    fn route_text(&mut self, text: &str) {
        let event = UiEvent::TextInput {
            text: text.to_string(),
        };
        match self.focus {
            Focus::Editor => {
                if let Some(buffer) = self.active_buffer_mut() {
                    if buffer.loaded {
                        let _ = buffer.area.handle_event(event);
                    }
                }
            }
            Focus::Find => {
                if let Some(find) = &mut self.find {
                    let _ = find.field.handle_event(event);
                }
            }
            Focus::Goto => {
                if let Some(goto) = &mut self.goto {
                    let _ = goto.handle_event(event);
                }
            }
        }
    }

    fn run(&mut self, command: Command, host: &dyn EditorHost) {
        match command {
            Command::OpenFolder => self.show_folder_dialog(),
            Command::CloseTab => {
                if let Some(index) = self.active {
                    self.request_close(index);
                }
            }
            Command::Quit => {
                if self.buffers.iter().any(Buffer::dirty) {
                    self.quit_prompt = true;
                } else {
                    self.quit_now();
                }
            }
            Command::QuitSaveAll => {
                self.quit_prompt = false;
                self.quit_after_save = true;
                self.save_all();
                if !self.buffers.iter().any(Buffer::dirty) {
                    self.quit_now();
                }
            }
            Command::QuitDiscard => {
                // Discarding on purpose: the backups go too, or the next
                // launch would bring the edits back.
                if let Some(state_dir) = self.state_dir.clone() {
                    for buffer in &mut self.buffers {
                        if buffer.has_backup_file || buffer.dirty() {
                            self.outbox.push(IoRequest::RemoveState {
                                path: session::backup_path(&state_dir, &buffer.path),
                            });
                            buffer.has_backup_file = false;
                        }
                    }
                }
                self.backup_due = None;
                self.quit_now();
            }
            Command::QuitCancel => self.quit_prompt = false,
            Command::Undo | Command::Redo | Command::SelectAll => {
                if let Some(area) = self.focused_area() {
                    match command {
                        Command::Undo => {
                            area.undo();
                        }
                        Command::Redo => {
                            area.redo();
                        }
                        _ => area.select_all(),
                    }
                }
            }
            Command::Cut | Command::Copy => self.copy(command == Command::Cut, host),
            Command::Paste => {
                if let Some(text) = host.read_clipboard() {
                    self.paste(&text);
                }
            }
            Command::Save => {
                if let Some(index) = self.active {
                    self.save(index);
                }
            }
            Command::SaveAll => self.save_all(),
            Command::Find => {
                self.goto = None;
                let seed = self.active_buffer().and_then(|buffer| {
                    let (start, end) = buffer.area.selection_range()?;
                    let text = buffer.area.document.slice_chars(start, end);
                    (!text.contains('\n')).then_some(text)
                });
                let find = self.find.get_or_insert_with(|| FindBar {
                    field: TextFieldModel::new("", Rect::default()),
                    matches: Vec::new(),
                    matches_for: None,
                });
                style_field(&mut find.field);
                if let Some(seed) = seed {
                    find.field.set_text(&seed);
                }
                find.field.select_all();
                self.set_focus(Focus::Find);
            }
            Command::GoToLine => {
                self.find = None;
                let goto = self
                    .goto
                    .get_or_insert_with(|| TextFieldModel::new("", Rect::default()));
                style_field(goto);
                goto.select_all();
                self.set_focus(Focus::Goto);
            }
            Command::Refresh => {
                if let Some(tree) = &mut self.tree {
                    tree.refresh();
                }
            }
            Command::PromptSave => {
                if let Some(index) = self
                    .close_prompt
                    .take()
                    .and_then(|id| self.buffer_index(id))
                {
                    self.buffers[index].close_after_save = true;
                    self.save(index);
                }
            }
            Command::PromptDiscard => {
                if let Some(index) = self
                    .close_prompt
                    .take()
                    .and_then(|id| self.buffer_index(id))
                {
                    self.close(index);
                }
            }
            Command::PromptCancel => self.close_prompt = None,
            Command::ConflictKeepMine => {
                if let Some(index) = self.active {
                    let buffer = &mut self.buffers[index];
                    buffer.conflict = false;
                    buffer.disk = None;
                    self.save(index);
                }
            }
            Command::ConflictReload => {
                if let Some(buffer) = self.active_buffer_mut() {
                    let path = buffer.path.clone();
                    self.outbox.push(IoRequest::ReadFile { path });
                }
            }
            Command::FindNext | Command::FindPrevious => {
                self.update_find_matches();
                self.step_find(command == Command::FindNext);
            }
            Command::CloseBar => {
                self.find = None;
                self.goto = None;
                self.set_focus(Focus::Editor);
            }
            Command::GotoGo => self.go_to_line(),
        }
    }

    fn update_find_matches(&mut self) {
        let Some(find) = &mut self.find else {
            return;
        };
        let Some(buffer) = self.active.and_then(|index| self.buffers.get(index)) else {
            find.matches.clear();
            find.matches_for = None;
            return;
        };
        let query = find.field.text();
        let key = (buffer.id, buffer.area.revision(), query.clone());
        if find.matches_for.as_ref() == Some(&key) {
            return;
        }
        find.matches = find_matches(&buffer.area.text(), &query);
        find.matches_for = Some(key);
    }

    fn step_find(&mut self, forward: bool) {
        let Some(find) = &self.find else {
            return;
        };
        if find.matches.is_empty() {
            self.set_status("No matches.", StatusKind::Info);
            return;
        }
        let matches = find.matches.clone();
        let Some(buffer) = self.active_buffer_mut() else {
            return;
        };
        let (sel_start, sel_end) = buffer
            .area
            .selection_range()
            .unwrap_or((buffer.area.caret(), buffer.area.caret()));
        let chosen = if forward {
            matches
                .iter()
                .find(|(start, _)| *start >= sel_end && (*start, *start) != (sel_start, sel_end))
                .or_else(|| matches.first())
        } else {
            matches
                .iter()
                .rev()
                .find(|(start, _)| *start < sel_start)
                .or_else(|| matches.last())
        };
        if let Some(&(start, end)) = chosen {
            buffer.area.select_range(start, end);
        }
    }

    fn go_to_line(&mut self) {
        let Some(goto) = &self.goto else {
            return;
        };
        let Ok(line) = goto.text().trim().parse::<usize>() else {
            self.set_status("Type a line number.", StatusKind::Info);
            return;
        };
        if let Some(buffer) = self.active_buffer_mut() {
            let target = buffer.area.line_start_char(line.saturating_sub(1));
            buffer.area.set_caret(target);
        }
        self.goto = None;
        self.set_focus(Focus::Editor);
    }

    // ----- painting -----

    pub fn paint(&mut self, scene: &mut Vec<PaintOp>) {
        let layout = std::mem::take(&mut self.layout);
        self.paint_toolbar(scene, &layout);
        if let Some(tree) = &mut self.tree {
            tree.paint(scene);
        } else {
            draw::fill(scene, layout.tree, theme::PANEL);
        }
        draw::vline(
            scene,
            layout.tree.right() + 0.5,
            layout.tree.y,
            layout.tree.height,
            theme::BORDER,
        );
        self.paint_tabs(scene, &layout);
        self.paint_bar(scene, &layout);
        match self.active_buffer() {
            Some(buffer) if buffer.loaded => buffer.area.paint(scene),
            Some(_) => self.paint_hint(scene, layout.editor, "Loading…"),
            None => self.paint_hint(
                scene,
                layout.editor,
                "Open a file from the folder on the left.",
            ),
        }
        self.paint_status(scene, &layout);
        if let Some(menu) = &self.drawn_menu {
            menu.paint(scene);
        }
        if let Some(dialog) = &self.folder_dialog {
            FileDialogModel::paint_scrim(
                Rect {
                    x: 0.0,
                    y: 0.0,
                    width: layout.toolbar.width,
                    height: layout.status.bottom(),
                },
                scene,
            );
            dialog.paint(scene);
        }
        self.layout = layout;
    }

    fn paint_hint(&self, scene: &mut Vec<PaintOp>, rect: Rect, text: &str) {
        draw::fill(scene, rect, theme::EDITOR_BACKGROUND);
        draw::label(
            scene,
            text,
            Rect {
                x: rect.x,
                y: rect.y + 40.0,
                width: rect.width,
                height: 24.0,
            },
            theme::TEXT_DIM,
            theme::UI_FONT,
            HorizontalAlign::Center,
        );
    }

    fn paint_toolbar(&self, scene: &mut Vec<PaintOp>, layout: &Layout) {
        draw::fill(scene, layout.toolbar, theme::PANEL);
        draw::hline(
            scene,
            0.0,
            layout.toolbar.bottom() - 0.5,
            layout.toolbar.width,
            theme::BORDER,
        );
        let any_dirty = self.buffers.iter().any(Buffer::dirty);
        let active_dirty = self.active_buffer().is_some_and(Buffer::dirty);
        let mut right_edge = 0.0f32;
        for (command, rect) in &layout.buttons {
            let (text, enabled) = match command {
                Command::OpenFolder => ("Open Folder…", true),
                Command::Save => ("Save", active_dirty),
                Command::SaveAll => ("Save All", any_dirty),
                Command::Find => ("Find", self.active.is_some()),
                Command::GoToLine => ("Go to Line", self.active.is_some()),
                Command::Refresh => ("Refresh Folder", self.tree.is_some()),
                _ => continue,
            };
            draw::button(scene, *rect, text, rect.contains(self.pointer), enabled);
            right_edge = right_edge.max(rect.right());
        }
        if let Some(tree) = &self.tree {
            draw::label(
                scene,
                &tree.root().display().to_string(),
                Rect {
                    x: right_edge + 16.0,
                    y: layout.toolbar.y,
                    width: (layout.toolbar.width - right_edge - 28.0).max(0.0),
                    height: layout.toolbar.height,
                },
                theme::TEXT_DIM,
                theme::UI_FONT,
                HorizontalAlign::Right,
            );
        }
    }

    fn paint_tabs(&self, scene: &mut Vec<PaintOp>, layout: &Layout) {
        draw::fill(scene, layout.tabs, theme::BACKGROUND);
        for (index, (slot, buffer)) in layout.tab_slots.iter().zip(&self.buffers).enumerate() {
            let active = self.active == Some(index);
            let hover = slot.rect.contains(self.pointer);
            let fill = if active {
                theme::EDITOR_BACKGROUND
            } else if hover {
                theme::HOVER
            } else {
                theme::PANEL
            };
            let clipped = draw::intersect(slot.rect, layout.tabs);
            draw::fill(scene, clipped, fill);
            draw::vline(
                scene,
                slot.rect.right() - 0.5,
                slot.rect.y,
                slot.rect.height,
                theme::BORDER,
            );
            if active {
                draw::hline(
                    scene,
                    slot.rect.x,
                    slot.rect.y + 1.0,
                    slot.rect.width - 1.0,
                    theme::ACCENT,
                );
            }
            draw::label_in(
                scene,
                &buffer.title,
                Rect {
                    x: slot.rect.x + 12.0,
                    y: slot.rect.y,
                    width: (slot.close.x - slot.rect.x - 16.0).max(0.0),
                    height: slot.rect.height,
                },
                layout.tabs,
                if active || buffer.dirty() {
                    theme::TEXT
                } else {
                    theme::TEXT_DIM
                },
                theme::UI_FONT,
                HorizontalAlign::Left,
            );
            let center = Point {
                x: slot.close.x + slot.close.width / 2.0,
                y: slot.close.y + slot.close.height / 2.0,
            };
            if !layout.tabs.contains(center) {
                continue;
            }
            if buffer.dirty() && !slot.close.contains(self.pointer) {
                scene.push(PaintOp::FillCircle {
                    center,
                    radius: 4.0,
                    color: if buffer.conflict {
                        theme::WARNING
                    } else {
                        theme::TEXT
                    },
                });
            } else if hover || active {
                if slot.close.contains(self.pointer) {
                    draw::fill(scene, slot.close, theme::BUTTON_HOVER);
                }
                let r = 4.0;
                for (dx, dy) in [(r, r), (r, -r)] {
                    scene.push(PaintOp::Line {
                        from: Point {
                            x: center.x - dx,
                            y: center.y - dy,
                        },
                        to: Point {
                            x: center.x + dx,
                            y: center.y + dy,
                        },
                        color: theme::TEXT_DIM,
                    });
                }
            }
        }
        draw::hline(
            scene,
            layout.tabs.x,
            layout.tabs.bottom() - 0.5,
            layout.tabs.width,
            theme::BORDER,
        );
    }

    fn paint_bar(&self, scene: &mut Vec<PaintOp>, layout: &Layout) {
        let Some(bar) = layout.bar else {
            return;
        };
        draw::fill(scene, bar, theme::PANEL);
        draw::hline(scene, bar.x, bar.bottom() - 0.5, bar.width, theme::BORDER);
        let message_rect = Rect {
            x: bar.x + 12.0,
            y: bar.y,
            width: layout
                .bar_buttons
                .first()
                .map_or(bar.width, |(_, rect)| rect.x - bar.x - 24.0)
                .max(0.0),
            height: bar.height,
        };
        let title = self
            .active_buffer()
            .map(|buffer| buffer.title.clone())
            .unwrap_or_default();
        if self.quit_prompt {
            let dirty = self.buffers.iter().filter(|buffer| buffer.dirty()).count();
            let text = if dirty == 1 {
                "1 file has unsaved changes.".to_string()
            } else {
                format!("{dirty} files have unsaved changes.")
            };
            draw::label(
                scene,
                &text,
                message_rect,
                theme::WARNING,
                theme::UI_FONT,
                HorizontalAlign::Left,
            );
        } else if let Some(id) = self.close_prompt {
            let name = self
                .buffer_index(id)
                .map(|index| self.buffers[index].title.clone())
                .unwrap_or_default();
            draw::label(
                scene,
                &format!("{name} has unsaved changes."),
                message_rect,
                theme::WARNING,
                theme::UI_FONT,
                HorizontalAlign::Left,
            );
        } else if self.active_buffer().is_some_and(|buffer| buffer.conflict) {
            draw::label(
                scene,
                &format!("{title} changed on disk while you had unsaved changes."),
                message_rect,
                theme::WARNING,
                theme::UI_FONT,
                HorizontalAlign::Left,
            );
        } else if let Some(find) = &self.find {
            let count = match find.matches.len() {
                _ if find.field.text().is_empty() => String::new(),
                0 => "No matches".to_string(),
                1 => "1 match".to_string(),
                n => format!("{n} matches"),
            };
            draw::label(
                scene,
                "Find",
                Rect {
                    width: 90.0,
                    ..message_rect
                },
                theme::TEXT_DIM,
                theme::UI_FONT,
                HorizontalAlign::Left,
            );
            find.field.paint(scene);
            if let Some(field) = layout.bar_field {
                let first_button = layout.bar_buttons.first().map_or(bar.right(), |(_, r)| r.x);
                draw::label(
                    scene,
                    &count,
                    Rect {
                        x: field.right() + 10.0,
                        y: bar.y,
                        width: (first_button - field.right() - 20.0).max(0.0),
                        height: bar.height,
                    },
                    theme::TEXT_DIM,
                    theme::UI_FONT,
                    HorizontalAlign::Left,
                );
            }
        } else if let Some(goto) = &self.goto {
            draw::label(
                scene,
                "Go to line",
                Rect {
                    width: 90.0,
                    ..message_rect
                },
                theme::TEXT_DIM,
                theme::UI_FONT,
                HorizontalAlign::Left,
            );
            goto.paint(scene);
        }
        for (command, rect) in &layout.bar_buttons {
            let text = match command {
                Command::QuitSaveAll => "Save All and Quit",
                Command::QuitDiscard => "Quit Without Saving",
                Command::QuitCancel => "Cancel",
                Command::PromptSave => "Save",
                Command::PromptDiscard => "Don't Save",
                Command::PromptCancel => "Cancel",
                Command::ConflictKeepMine => "Keep Mine and Save",
                Command::ConflictReload => "Reload from Disk",
                Command::FindPrevious => "Previous",
                Command::FindNext => "Next",
                Command::CloseBar => "Close",
                Command::GotoGo => "Go",
                _ => continue,
            };
            draw::button(scene, *rect, text, rect.contains(self.pointer), true);
        }
    }

    fn paint_status(&self, scene: &mut Vec<PaintOp>, layout: &Layout) {
        let rect = layout.status;
        draw::fill(scene, rect, theme::PANEL);
        draw::hline(scene, rect.x, rect.y + 0.5, rect.width, theme::BORDER);
        let mut right_text = String::new();
        if let Some(buffer) = self.active_buffer().filter(|buffer| buffer.loaded) {
            let (line, column) = caret_line_column(&buffer.area);
            right_text = format!(
                "Ln {line}, Col {column}    {}    UTF-8",
                buffer.line_ending.label()
            );
        }
        let right_width = 260.0;
        draw::label(
            scene,
            &right_text,
            Rect {
                x: rect.right() - right_width - 12.0,
                y: rect.y,
                width: right_width,
                height: rect.height,
            },
            theme::TEXT_DIM,
            theme::UI_FONT,
            HorizontalAlign::Right,
        );
        let (left_text, color) = match &self.status {
            Some((text, kind)) => (
                text.clone(),
                match kind {
                    StatusKind::Info => theme::TEXT_DIM,
                    StatusKind::Warning => theme::WARNING,
                    StatusKind::Error => theme::ERROR,
                },
            ),
            None => (
                self.active_buffer()
                    .map(|buffer| buffer.path.display().to_string())
                    .unwrap_or_default(),
                theme::TEXT_DIM,
            ),
        };
        draw::label(
            scene,
            &left_text,
            Rect {
                x: rect.x + 12.0,
                y: rect.y,
                width: (rect.width - right_width - 36.0).max(0.0),
                height: rect.height,
            },
            color,
            theme::UI_FONT,
            HorizontalAlign::Left,
        );
    }
}

/// A text area set up for code.
fn code_area(text: &str) -> TextAreaModel {
    let mut area = TextAreaModel::new(text, Rect::default());
    area.style.font_size = theme::CODE_FONT;
    area.style.color = theme::TEXT;
    area.line_spacing = 4.0;
    area.background = Some(theme::EDITOR_BACKGROUND);
    area.border = None;
    area.selection_fill = theme::SELECTION_FILL;
    area.caret_color = theme::TEXT;
    area.show_line_numbers = true;
    area.line_number_color = theme::TEXT_DIM;
    area.line_number_gutter_fill = Some(theme::EDITOR_BACKGROUND);
    area.line_number_gutter_border = Some(theme::BORDER);
    area.tab_text = INDENT.to_string();
    area.auto_indent = true;
    area
}

fn style_field(field: &mut TextFieldModel) {
    field.area.style.font_size = theme::UI_FONT;
    field.area.background = Some(theme::EDITOR_BACKGROUND);
    field.area.border = Some(theme::BORDER);
    field.area.selection_fill = theme::SELECTION_FILL;
    field.area.caret_color = theme::TEXT;
    field.area.style.color = theme::TEXT;
}

/// One-based line and column of the caret.
fn caret_line_column(area: &TextAreaModel) -> (usize, usize) {
    let caret = area.caret();
    let mut line = 1;
    let mut line_start = 0;
    let mut index = 0;
    area.document.for_each_chunk(|chunk| {
        for ch in chunk.chars() {
            if index >= caret {
                return;
            }
            index += 1;
            if ch == '\n' {
                line += 1;
                line_start = index;
            }
        }
    });
    (line, caret - line_start + 1)
}

/// Character ranges of every match of `query` in `text`. Matching ignores
/// case unless the query has an uppercase letter.
pub fn find_matches(text: &str, query: &str) -> Vec<(usize, usize)> {
    if query.is_empty() {
        return Vec::new();
    }
    let ignore_case = !query.chars().any(char::is_uppercase);
    let (haystack, needle) = if ignore_case {
        (lowercase_same_length(text), query.to_lowercase())
    } else {
        (text.to_string(), query.to_string())
    };
    let needle_chars = needle.chars().count();
    let mut matches = Vec::new();
    let mut char_index = 0;
    let mut byte_cursor = 0;
    let mut search_from = 0;
    while let Some(found) = haystack[search_from..].find(&needle) {
        let byte = search_from + found;
        char_index += haystack[byte_cursor..byte].chars().count();
        byte_cursor = byte;
        matches.push((char_index, char_index + needle_chars));
        search_from = byte + needle.len().max(1);
    }
    matches
}

/// Lowercases character by character, keeping a character whose lowercase
/// form is longer than one character as it is, so character positions in
/// the result match the original.
fn lowercase_same_length(text: &str) -> String {
    text.chars()
        .map(|ch| {
            let mut lower = ch.to_lowercase();
            match (lower.next(), lower.next()) {
                (Some(single), None) => single,
                _ => ch,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use loadngo_host_core::{HostKey, HostKeyEvent};
    use std::cell::RefCell;
    use std::fs;
    use ui_core::{MenuBar, MenuItem};

    struct FakeHost {
        clipboard: RefCell<Option<String>>,
    }

    impl EditorHost for FakeHost {
        fn measure(&self, text: &str, font_size: u16) -> f32 {
            text.chars().count() as f32 * font_size as f32 * 0.5
        }
        fn read_clipboard(&self) -> Option<String> {
            self.clipboard.borrow().clone()
        }
        fn write_clipboard(&self, text: &str) {
            *self.clipboard.borrow_mut() = Some(text.to_string());
        }
    }

    struct Rig {
        editor: Editor,
        now: Instant,
        host: FakeHost,
    }

    impl Rig {
        fn new(state_dir: Option<PathBuf>, root: Option<PathBuf>) -> Self {
            let now = Instant::now();
            let mut rig = Self {
                editor: Editor::new(state_dir, root, PathBuf::from("/nonexistent"), now),
                now,
                host: FakeHost {
                    clipboard: RefCell::new(None),
                },
            };
            rig.settle();
            rig
        }

        /// Runs file work to completion, with a frame after each round.
        fn settle(&mut self) {
            for _ in 0..20 {
                self.frame(InputSnapshot::default());
                let requests = self.editor.take_requests();
                if requests.is_empty() {
                    return;
                }
                for request in requests {
                    self.editor.apply(fs_ops::perform(request), self.now);
                }
            }
            panic!("file work did not settle");
        }

        fn frame(&mut self, input: InputSnapshot) {
            self.editor
                .frame(&input, true, (1200.0, 800.0), self.now, &self.host);
        }

        fn later(&mut self, seconds: u64) {
            self.now += Duration::from_secs(seconds);
            self.settle();
        }

        fn key(&mut self, key: HostKey, modifiers: Modifiers) {
            self.frame(InputSnapshot {
                key_events: vec![HostKeyEvent { key, modifiers }],
                ..InputSnapshot::default()
            });
            self.settle();
        }

        fn type_text(&mut self, text: &str) {
            self.frame(InputSnapshot {
                typed_text: text.to_string(),
                ..InputSnapshot::default()
            });
        }

        fn click(&mut self, point: Point) {
            self.frame(InputSnapshot {
                mouse_x: point.x,
                mouse_y: point.y,
                mouse_pressed: true,
                mouse_down: true,
                ..InputSnapshot::default()
            });
            self.frame(InputSnapshot {
                mouse_x: point.x,
                mouse_y: point.y,
                mouse_released: true,
                ..InputSnapshot::default()
            });
            self.settle();
        }

        fn active(&self) -> &Buffer {
            self.editor.active_buffer().expect("a tab is open")
        }

        fn status(&self) -> String {
            self.editor
                .status
                .as_ref()
                .map(|(text, _)| text.clone())
                .unwrap_or_default()
        }

        fn bar_button(&self, command: Command) -> Point {
            let rect = self
                .editor
                .layout
                .bar_buttons
                .iter()
                .find(|(c, _)| *c == command)
                .map(|(_, rect)| *rect)
                .unwrap_or_else(|| panic!("no {command:?} button"));
            Point {
                x: rect.x + 2.0,
                y: rect.y + 2.0,
            }
        }
    }

    fn cmd() -> Modifiers {
        Modifiers {
            meta: true,
            ..Modifiers::default()
        }
    }

    fn workspace(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (name, text) in files {
            let path = dir.path().join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        dir
    }

    #[test]
    fn edit_and_save_with_cmd_s_and_cmd_shift_s() {
        let dir = workspace(&[("a.rs", "fn a() {}\n"), ("b.rs", "fn b() {}\n")]);
        let mut rig = Rig::new(None, Some(dir.path().to_path_buf()));
        rig.editor.open_file(dir.path().join("a.rs"));
        rig.settle();
        rig.type_text("// a\n");
        assert!(rig.active().dirty());
        rig.key(HostKey::S, cmd());
        assert_eq!(
            fs::read_to_string(dir.path().join("a.rs")).unwrap(),
            "// a\nfn a() {}\n"
        );
        assert!(!rig.active().dirty());
        assert_eq!(rig.status(), "Saved a.rs.");

        rig.type_text("x");
        rig.editor.open_file(dir.path().join("b.rs"));
        rig.settle();
        rig.type_text("y");
        rig.key(
            HostKey::S,
            Modifiers {
                shift: true,
                ..cmd()
            },
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("a.rs")).unwrap(),
            "// a\nxfn a() {}\n"
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("b.rs")).unwrap(),
            "yfn b() {}\n"
        );
        assert!(rig.editor.buffers.iter().all(|buffer| !buffer.dirty()));
    }

    #[test]
    fn crlf_files_keep_their_line_endings() {
        let dir = workspace(&[("w.txt", "one\r\ntwo\r\n")]);
        let mut rig = Rig::new(None, Some(dir.path().to_path_buf()));
        rig.editor.open_file(dir.path().join("w.txt"));
        rig.settle();
        assert_eq!(rig.active().area.text(), "one\ntwo\n");
        rig.type_text("zero\n");
        rig.key(HostKey::S, cmd());
        assert_eq!(
            fs::read(dir.path().join("w.txt")).unwrap(),
            b"zero\r\none\r\ntwo\r\n"
        );
    }

    #[test]
    fn undo_back_to_the_saved_text_is_clean() {
        let dir = workspace(&[("a.rs", "abc")]);
        let mut rig = Rig::new(None, Some(dir.path().to_path_buf()));
        rig.editor.open_file(dir.path().join("a.rs"));
        rig.settle();
        rig.type_text("x");
        assert!(rig.active().dirty());
        rig.key(HostKey::Z, cmd());
        assert!(!rig.active().dirty());
    }

    #[test]
    fn closing_a_dirty_tab_asks_and_save_closes_it_after_writing() {
        let dir = workspace(&[("a.rs", "abc")]);
        let mut rig = Rig::new(None, Some(dir.path().to_path_buf()));
        rig.editor.open_file(dir.path().join("a.rs"));
        rig.settle();
        rig.type_text("x");
        let close = rig.editor.layout.tab_slots[0].close;
        rig.click(Point {
            x: close.x + 4.0,
            y: close.y + 4.0,
        });
        assert_eq!(rig.editor.buffers.len(), 1, "a prompt, not a close");
        assert!(rig.editor.close_prompt.is_some());
        rig.click(rig.bar_button(Command::PromptSave));
        assert!(rig.editor.buffers.is_empty());
        assert_eq!(fs::read_to_string(dir.path().join("a.rs")).unwrap(), "xabc");
    }

    #[test]
    fn dont_save_discards_and_cancel_keeps_the_tab() {
        let dir = workspace(&[("a.rs", "abc")]);
        let mut rig = Rig::new(None, Some(dir.path().to_path_buf()));
        rig.editor.open_file(dir.path().join("a.rs"));
        rig.settle();
        rig.type_text("x");
        let index = rig.editor.active.unwrap();
        rig.editor.request_close(index);
        rig.settle();
        rig.click(rig.bar_button(Command::PromptCancel));
        assert_eq!(rig.editor.buffers.len(), 1);
        rig.editor.request_close(index);
        rig.settle();
        rig.click(rig.bar_button(Command::PromptDiscard));
        assert!(rig.editor.buffers.is_empty());
        assert_eq!(fs::read_to_string(dir.path().join("a.rs")).unwrap(), "abc");
    }

    #[test]
    fn a_clean_file_changed_on_disk_reloads_when_the_window_regains_focus() {
        let dir = workspace(&[("a.rs", "old")]);
        let mut rig = Rig::new(None, Some(dir.path().to_path_buf()));
        rig.editor.open_file(dir.path().join("a.rs"));
        rig.settle();
        fs::write(dir.path().join("a.rs"), "new contents").unwrap();
        rig.editor.frame(
            &InputSnapshot::default(),
            false,
            (1200.0, 800.0),
            rig.now,
            &rig.host,
        );
        rig.settle();
        assert_eq!(rig.active().area.text(), "new contents");
        assert!(!rig.active().dirty());
    }

    #[test]
    fn a_dirty_file_changed_on_disk_asks_and_keep_mine_saves_over_it() {
        let dir = workspace(&[("a.rs", "old")]);
        let mut rig = Rig::new(None, Some(dir.path().to_path_buf()));
        rig.editor.open_file(dir.path().join("a.rs"));
        rig.settle();
        rig.type_text("mine ");
        fs::write(dir.path().join("a.rs"), "theirs, longer").unwrap();
        // Saving without a focus change finds the conflict at write time.
        rig.key(HostKey::S, cmd());
        assert!(rig.active().conflict);
        assert_eq!(
            fs::read_to_string(dir.path().join("a.rs")).unwrap(),
            "theirs, longer"
        );
        rig.click(rig.bar_button(Command::ConflictKeepMine));
        assert_eq!(
            fs::read_to_string(dir.path().join("a.rs")).unwrap(),
            "mine old"
        );
        assert!(!rig.active().conflict);
    }

    #[test]
    fn reload_from_disk_replaces_unsaved_edits_and_undo_brings_them_back() {
        let dir = workspace(&[("a.rs", "old")]);
        let mut rig = Rig::new(None, Some(dir.path().to_path_buf()));
        rig.editor.open_file(dir.path().join("a.rs"));
        rig.settle();
        rig.type_text("mine ");
        fs::write(dir.path().join("a.rs"), "theirs, longer").unwrap();
        rig.editor.frame(
            &InputSnapshot::default(),
            false,
            (1200.0, 800.0),
            rig.now,
            &rig.host,
        );
        rig.settle();
        assert!(rig.active().conflict);
        rig.click(rig.bar_button(Command::ConflictReload));
        assert_eq!(rig.active().area.text(), "theirs, longer");
        assert!(!rig.active().dirty());
        rig.key(HostKey::Z, cmd());
        assert_eq!(rig.active().area.text(), "mine old");
    }

    #[test]
    fn binary_files_are_not_opened() {
        let dir = workspace(&[]);
        fs::write(dir.path().join("x.bin"), [0u8, 1, 2]).unwrap();
        let mut rig = Rig::new(None, Some(dir.path().to_path_buf()));
        rig.editor.open_file(dir.path().join("x.bin"));
        rig.settle();
        assert!(rig.editor.buffers.is_empty());
        assert_eq!(rig.status(), "x.bin was not opened: a binary file.");
    }

    #[test]
    fn unsaved_edits_and_tabs_come_back_after_a_restart() {
        let dir = workspace(&[("src/a.rs", "fn a() {}\n"), ("b.rs", "b")]);
        let state = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        {
            let mut rig = Rig::new(Some(state.path().to_path_buf()), Some(root.clone()));
            rig.editor.open_file(root.join("b.rs"));
            rig.editor.open_file(root.join("src/a.rs"));
            rig.settle();
            rig.type_text("// unsaved\n");
            rig.later(3);
            assert!(
                rig.editor.buffers[1].has_backup_file,
                "backed up after typing stopped"
            );
            // The window closes here: nothing saved.
        }
        let mut rig = Rig::new(Some(state.path().to_path_buf()), None);
        let tree_root = rig.editor.tree.as_ref().unwrap().root().to_path_buf();
        assert_eq!(tree_root, root);
        let paths: Vec<_> = rig.editor.buffers.iter().map(|b| b.path.clone()).collect();
        assert_eq!(paths, vec![root.join("b.rs"), root.join("src/a.rs")]);
        assert_eq!(rig.active().path, root.join("src/a.rs"));
        assert_eq!(rig.active().area.text(), "// unsaved\nfn a() {}\n");
        assert!(rig.active().dirty());
        assert_eq!(
            fs::read_to_string(root.join("src/a.rs")).unwrap(),
            "fn a() {}\n"
        );
        // Undo returns to the file's text; the backup goes once it is clean.
        rig.key(HostKey::Z, cmd());
        assert!(!rig.active().dirty());
        rig.later(3);
        let backup = session::backup_path(state.path(), &root.join("src/a.rs"));
        assert!(!backup.exists());
    }

    #[test]
    fn saving_removes_the_backup() {
        let dir = workspace(&[("a.rs", "a")]);
        let state = tempfile::tempdir().unwrap();
        let mut rig = Rig::new(
            Some(state.path().to_path_buf()),
            Some(dir.path().to_path_buf()),
        );
        rig.editor.open_file(dir.path().join("a.rs"));
        rig.settle();
        rig.type_text("x");
        rig.later(3);
        let backup = session::backup_path(state.path(), &dir.path().join("a.rs"));
        assert!(backup.exists());
        rig.key(HostKey::S, cmd());
        assert!(!backup.exists());
    }

    #[test]
    fn find_steps_through_matches_ignoring_case_for_lowercase_queries() {
        let dir = workspace(&[("a.rs", "let Value = value + VALUE;")]);
        let mut rig = Rig::new(None, Some(dir.path().to_path_buf()));
        rig.editor.open_file(dir.path().join("a.rs"));
        rig.settle();
        rig.editor.run(
            Command::Find,
            &FakeHost {
                clipboard: RefCell::new(None),
            },
        );
        rig.type_text("value");
        rig.frame(InputSnapshot::default());
        assert_eq!(rig.editor.find.as_ref().unwrap().matches.len(), 3);
        rig.key(HostKey::Enter, Modifiers::default());
        assert_eq!(rig.active().area.selection_range(), Some((4, 9)));
        rig.key(HostKey::Enter, Modifiers::default());
        assert_eq!(rig.active().area.selection_range(), Some((12, 17)));
        rig.key(
            HostKey::Enter,
            Modifiers {
                shift: true,
                ..Modifiers::default()
            },
        );
        assert_eq!(rig.active().area.selection_range(), Some((4, 9)));
        assert_eq!(find_matches("Value value", "Value"), vec![(0, 5)]);
    }

    #[test]
    fn go_to_line_moves_the_caret() {
        let dir = workspace(&[("a.rs", "one\ntwo\nthree\n")]);
        let mut rig = Rig::new(None, Some(dir.path().to_path_buf()));
        rig.editor.open_file(dir.path().join("a.rs"));
        rig.settle();
        rig.editor.run(
            Command::GoToLine,
            &FakeHost {
                clipboard: RefCell::new(None),
            },
        );
        rig.type_text("3");
        rig.key(HostKey::Enter, Modifiers::default());
        assert_eq!(rig.active().area.caret(), 8);
        assert!(rig.editor.goto.is_none());
        assert_eq!(caret_line_column(&rig.active().area), (3, 1));
    }

    #[test]
    fn copy_cut_and_paste_use_the_host_clipboard() {
        let dir = workspace(&[("a.rs", "alpha beta")]);
        let mut rig = Rig::new(None, Some(dir.path().to_path_buf()));
        rig.editor.open_file(dir.path().join("a.rs"));
        rig.settle();
        rig.editor
            .active_buffer_mut()
            .unwrap()
            .area
            .select_range(0, 5);
        rig.key(HostKey::X, cmd());
        assert_eq!(rig.host.read_clipboard().as_deref(), Some("alpha"));
        assert_eq!(rig.active().area.text(), " beta");
        rig.editor.active_buffer_mut().unwrap().area.set_caret(5);
        *rig.host.clipboard.borrow_mut() = Some("\r\ngamma".to_string());
        rig.key(HostKey::V, cmd());
        assert_eq!(rig.active().area.text(), " beta\ngamma");
    }

    #[test]
    fn an_idle_editor_needs_no_frames() {
        let dir = workspace(&[("a.rs", "a")]);
        let mut rig = Rig::new(None, Some(dir.path().to_path_buf()));
        rig.editor.open_file(dir.path().join("a.rs"));
        rig.settle();
        rig.type_text("x");
        assert!(rig.editor.next_wake(rig.now).is_some(), "the caret blinks");
        rig.now += BLINK_FOR + Duration::from_secs(1);
        rig.frame(InputSnapshot::default());
        assert_eq!(rig.editor.next_wake(rig.now), None);
        assert!(rig.active().area.show_caret, "a still caret is shown");
    }

    fn menu_input(command: Command) -> InputSnapshot {
        InputSnapshot {
            menu_commands: vec![command.menu_command()],
            ..InputSnapshot::default()
        }
    }

    impl Rig {
        fn outcome(&mut self, input: InputSnapshot) -> FrameOutcome {
            let outcome = self
                .editor
                .frame(&input, true, (1200.0, 800.0), self.now, &self.host);
            for _ in 0..20 {
                let requests = self.editor.take_requests();
                if requests.is_empty() {
                    break;
                }
                for request in requests {
                    self.editor.apply(fs_ops::perform(request), self.now);
                }
            }
            outcome
        }
    }

    #[test]
    fn system_menu_commands_drive_the_editor_and_menus_follow_its_state() {
        let dir = workspace(&[("a.rs", "abc")]);
        let mut rig = Rig::new(None, Some(dir.path().to_path_buf()));
        rig.editor.set_native_menu(true);
        assert!(rig.editor.drawn_menu.is_none());
        rig.editor.open_file(dir.path().join("a.rs"));
        rig.settle();
        let save_enabled = |bar: &MenuBar| {
            bar.menus[0].items.iter().any(|item| {
                matches!(item, MenuItem::Command { title, enabled: true, .. } if title == "Save")
            })
        };
        let published = rig
            .outcome(InputSnapshot {
                typed_text: "x".to_string(),
                ..InputSnapshot::default()
            })
            .menu_bar;
        assert!(
            published.as_ref().is_some_and(save_enabled),
            "Save turns on"
        );
        assert_eq!(
            rig.outcome(InputSnapshot::default()).menu_bar,
            None,
            "unchanged"
        );
        rig.outcome(menu_input(Command::Save));
        assert_eq!(fs::read_to_string(dir.path().join("a.rs")).unwrap(), "xabc");
        let published = rig.outcome(InputSnapshot::default()).menu_bar;
        assert!(
            published.is_some_and(|bar| !save_enabled(&bar)),
            "Save turns off"
        );
        rig.editor
            .active_buffer_mut()
            .unwrap()
            .area
            .select_range(0, 2);
        rig.outcome(menu_input(Command::Copy));
        assert_eq!(rig.host.read_clipboard().as_deref(), Some("xa"));
        rig.outcome(menu_input(Command::Undo));
        assert_eq!(rig.active().area.text(), "abc");
    }

    #[test]
    fn quit_with_nothing_unsaved_finishes_and_writes_the_session() {
        let dir = workspace(&[("a.rs", "abc")]);
        let state = tempfile::tempdir().unwrap();
        let mut rig = Rig::new(
            Some(state.path().to_path_buf()),
            Some(dir.path().to_path_buf()),
        );
        rig.editor.open_file(dir.path().join("a.rs"));
        rig.settle();
        assert!(rig.outcome(menu_input(Command::Quit)).quit);
        let session: Session =
            serde_json::from_slice(&fs::read(session::session_path(state.path())).unwrap())
                .unwrap();
        assert_eq!(session.tabs.len(), 1);
    }

    #[test]
    fn quit_with_unsaved_work_asks_first() {
        let dir = workspace(&[("a.rs", "abc"), ("b.rs", "b")]);
        let state = tempfile::tempdir().unwrap();
        let mut rig = Rig::new(
            Some(state.path().to_path_buf()),
            Some(dir.path().to_path_buf()),
        );
        rig.editor.open_file(dir.path().join("a.rs"));
        rig.settle();
        rig.type_text("x");
        rig.later(3);
        let backup = session::backup_path(state.path(), &dir.path().join("a.rs"));
        assert!(backup.exists());

        assert!(!rig.outcome(menu_input(Command::Quit)).quit);
        assert!(rig.editor.quit_prompt);
        rig.click(rig.bar_button(Command::QuitCancel));
        assert!(!rig.editor.quit_prompt);

        rig.outcome(menu_input(Command::Quit));
        rig.click(rig.bar_button(Command::QuitSaveAll));
        assert!(rig.outcome(InputSnapshot::default()).quit);
        assert_eq!(fs::read_to_string(dir.path().join("a.rs")).unwrap(), "xabc");
        assert!(!backup.exists());
    }

    #[test]
    fn quit_without_saving_drops_the_backups() {
        let dir = workspace(&[("a.rs", "abc")]);
        let state = tempfile::tempdir().unwrap();
        let mut rig = Rig::new(
            Some(state.path().to_path_buf()),
            Some(dir.path().to_path_buf()),
        );
        rig.editor.open_file(dir.path().join("a.rs"));
        rig.settle();
        rig.type_text("x");
        rig.later(3);
        let backup = session::backup_path(state.path(), &dir.path().join("a.rs"));
        assert!(backup.exists());
        rig.outcome(menu_input(Command::Quit));
        rig.click(rig.bar_button(Command::QuitDiscard));
        assert!(rig.outcome(InputSnapshot::default()).quit);
        assert!(!backup.exists());
        assert_eq!(fs::read_to_string(dir.path().join("a.rs")).unwrap(), "abc");
    }

    #[test]
    fn open_folder_switches_the_tree_to_the_chosen_folder() {
        let dir = workspace(&[("one/a.rs", "a"), ("two/b.rs", "b")]);
        let root = dir.path().canonicalize().unwrap();
        let mut rig = Rig::new(None, Some(root.join("one")));
        rig.outcome(menu_input(Command::OpenFolder));
        assert!(rig.editor.folder_dialog.is_some());
        // Typing a path in the dialog and pressing Enter chooses it.
        rig.type_text(&root.join("two").display().to_string());
        rig.key(HostKey::Enter, Modifiers::default());
        assert!(rig.editor.folder_dialog.is_none());
        rig.settle();
        let tree = rig.editor.tree.as_mut().unwrap();
        assert_eq!(tree.root(), root.join("two"));
        let names: Vec<_> = tree.rows().iter().map(|row| row.name.clone()).collect();
        assert_eq!(names, vec!["b.rs"]);
    }

    #[test]
    fn escape_closes_the_folder_dialog() {
        let dir = workspace(&[("a.rs", "a")]);
        let mut rig = Rig::new(None, Some(dir.path().to_path_buf()));
        rig.outcome(menu_input(Command::OpenFolder));
        rig.key(HostKey::Escape, Modifiers::default());
        assert!(rig.editor.folder_dialog.is_none());
        assert_eq!(rig.editor.tree.as_ref().unwrap().root(), dir.path());
    }

    #[test]
    fn the_drawn_menu_bar_opens_and_runs_items() {
        let dir = workspace(&[("a.rs", "abc"), ("b.rs", "b")]);
        let mut rig = Rig::new(None, Some(dir.path().to_path_buf()));
        rig.editor.open_file(dir.path().join("a.rs"));
        rig.editor.open_file(dir.path().join("b.rs"));
        rig.settle();
        rig.type_text("y");
        // The toolbar sits below the drawn menu bar.
        assert_eq!(rig.editor.layout.toolbar.y, MenuBarModel::height());
        let file_title = Point { x: 20.0, y: 10.0 };
        rig.click(file_title);
        assert!(rig.editor.drawn_menu.as_ref().unwrap().is_open());
        // Down to "Save" (past Open Folder… and Refresh Folder) and Enter.
        for _ in 0..3 {
            rig.key(HostKey::Down, Modifiers::default());
        }
        rig.key(HostKey::Enter, Modifiers::default());
        assert_eq!(fs::read_to_string(dir.path().join("b.rs")).unwrap(), "yb");
        assert!(!rig.editor.drawn_menu.as_ref().unwrap().is_open());
        assert_eq!(rig.active().area.text(), "yb", "the keys went to the menu");
    }
}
