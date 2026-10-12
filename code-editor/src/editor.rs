//! The editor: an open folder, one tab per open file, and the bars and
//! buttons around them. It never touches the filesystem or the window:
//! input comes in through [`Editor::frame`], file work goes out as
//! [`IoRequest`]s and comes back through [`Editor::apply`], and the scene
//! goes out through [`Editor::paint`]. The app runs the requests on the
//! host's offload workers.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use loadngo_host_core::InputSnapshot;
use ui_core::{
    FileDialogModel, FileDialogOutcome, FileDialogPlace, HorizontalAlign, Key, Menu, MenuBar,
    MenuBarModel, MenuCommand, MenuItem, Modifiers, PaintOp, Point, PointerButton, PointerState,
    Rect, Shortcut, StdDirectorySource, TextAreaModel, TextFieldModel, UiEvent,
};

use crate::cargo_check::{CheckOutcome, Diagnostic, Level};
use crate::draw;
use crate::file_tree::{FileTree, TreeAction};
use crate::fs_ops::{self, DiskStamp, FileOp, IoRequest, IoResponse, ReadError, WriteError};
use crate::highlight::Highlighter;
use crate::lsp::{CompletionItem, LspEvent, LspSession, Position, TextEdit};
use crate::search::{SearchMatch, SearchOutcome};
use crate::session::{self, Backup, Session, SessionTab};
use crate::terminal_panel::{TerminalPanel, TerminalRequest};
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
    /// The file. For a page not yet saved, a name that is never a real path
    /// ("Untitled 1"): it keys the page's backup and session tab, and every
    /// file operation checks [`Buffer::untitled`] first.
    path: PathBuf,
    title: String,
    /// A page not yet saved to a file, by its number ("Untitled 1").
    untitled: Option<u32>,
    /// A page given its file: the tree lists the file once it is written.
    newly_named: bool,
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
    /// Syntax colors, for languages that have them.
    highlighter: Option<Highlighter>,
}

impl Buffer {
    fn new(id: u64, path: PathBuf) -> Self {
        let title = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        let highlighter = Highlighter::for_path(&path);
        Self {
            id,
            path,
            title,
            untitled: None,
            newly_named: false,
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
            highlighter,
        }
    }

    /// A blank page, "Untitled `number`": loaded at once, with nothing on
    /// disk until it is saved.
    fn untitled(id: u64, number: u32) -> Self {
        let name = untitled_name(number);
        let mut buffer = Self::new(id, PathBuf::from(&name));
        buffer.title = name;
        buffer.untitled = Some(number);
        buffer.highlighter = None;
        buffer.loaded = true;
        buffer.saved_revision = buffer.area.revision();
        buffer.seen_revision = buffer.saved_revision;
        buffer
    }

    fn dirty(&self) -> bool {
        self.loaded && self.area.revision() != self.saved_revision
    }

    /// A blank page nobody has typed into: opening a file replaces it.
    fn untouched_page(&self) -> bool {
        self.untitled.is_some() && !self.dirty() && self.area.document.len_chars() == 0
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

fn untitled_name(number: u32) -> String {
    format!("Untitled {number}")
}

/// What the file dialog is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DialogFor {
    OpenFolder,
    /// Naming the file a page (by buffer id) is saved to.
    SaveAs(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Editor,
    Find,
    Goto,
    Search,
    Terminal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    Save,
    SaveAll,
    Find,
    GoToLine,
    Refresh,
    Check,
    ToggleProblems,
    GoToDefinition,
    GoBack,
    RenameSymbol,
    FindInFiles,
    RunSearch,
    ToggleSplit,
    ToggleTerminal,
    NewTerminal,
    NewPage,
    NewFile,
    NewFolder,
    RenameFile,
    TrashFile,
    TrashConfirm,
    TrashCancel,
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
    const IN_MENUS: [Command; 28] = [
        Command::NewPage,
        Command::ToggleSplit,
        Command::ToggleTerminal,
        Command::NewTerminal,
        Command::FindInFiles,
        Command::NewFile,
        Command::NewFolder,
        Command::RenameFile,
        Command::TrashFile,
        Command::RenameSymbol,
        Command::GoToDefinition,
        Command::GoBack,
        Command::Check,
        Command::ToggleProblems,
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
    problems: Option<Rect>,
    search: Option<Rect>,
    search_field: Option<Rect>,
    search_button: Option<Rect>,
    status_check: Option<Rect>,
    /// The pane without focus, when split.
    other_pane: Option<Rect>,
    terminal: Option<Rect>,
}

/// A `cargo check` the app should run (see `cargo_check::run`), replacing
/// any still running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckRequest {
    pub id: u64,
    /// The check covers the package holding this file.
    pub file: PathBuf,
}

/// How the last check went, for the status bar.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CheckSummary {
    errors: usize,
    warnings: usize,
    elapsed: Duration,
}

/// A diagnostic's marks on screen: a bar in the gutter and an underline.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Mark {
    bar: Rect,
    underline: Option<(f32, f32, f32)>,
    level: Level,
}

const PROBLEMS_HEIGHT: f32 = 190.0;
const PROBLEM_ROW_HEIGHT: f32 = 22.0;

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
    dialog: Option<FileDialogModel>,
    dialog_for: DialogFor,
    /// Where the last page was saved: the next save starts there.
    last_save_dir: Option<PathBuf>,
    /// Pages (by buffer id) still to be named and saved, one dialog at a time
    /// (Save All, quitting).
    save_queue: Vec<u64>,
    dialog_clock: Instant,
    dialog_animating: bool,
    quit_prompt: bool,
    quit_after_save: bool,
    quitting: bool,
    check_outbox: Option<CheckRequest>,
    next_check_id: u64,
    check_running: Option<u64>,
    diagnostics: Vec<Diagnostic>,
    check_summary: Option<CheckSummary>,
    problems_open: bool,
    problems_scroll: usize,
    marks: Vec<Mark>,
    /// Go here (file, one-based line and column) once the file has loaded.
    /// And select this many characters there (a search match).
    pending_goto: Option<(PathBuf, usize, usize, usize)>,
    /// The rust-analyzer conversation, while one runs.
    lsp: Option<LspSession>,
    /// The folder a server should be started for (the app starts it).
    lsp_start: Option<PathBuf>,
    /// The running server should be stopped (folder switch, quit).
    lsp_stop: bool,
    /// Why the server stopped, if it did; it is not restarted on its own.
    lsp_failure: Option<String>,
    hover: Option<(String, Point)>,
    /// The pointer resting on text since: where, which character, when.
    hover_wait: Option<(Point, usize, Instant)>,
    hover_request: Option<u64>,
    definition_request: Option<u64>,
    /// Where Go to Definition and problem jumps came from, for Back.
    back_stack: Vec<(PathBuf, usize)>,
    /// The time of the frame being handled.
    frame_time: Instant,
    completion: Option<CompletionPopup>,
    /// What the Go to Line bar is doing: going to a line, or renaming.
    bar_mode: BarMode,
    rename_request: Option<u64>,
    /// A rename's edits for files still loading.
    pending_edits: HashMap<PathBuf, Vec<TextEdit>>,
    /// Asking before moving this to the Trash.
    trash_prompt: Option<PathBuf>,
    search: Option<SearchPanel>,
    search_open: bool,
    search_outbox: Option<SearchRequest>,
    next_search_id: u64,
    /// The second pane, when the editor is split.
    split: Option<Split>,
    /// The buffer active before the current one, for a new split.
    previous_active: Option<u64>,
    /// A press on a tree entry or a tab, which a move turns into a drag.
    item_drag: Option<ItemDrag>,
    /// Files from the file manager are being dragged over the window.
    file_drag_over: bool,
    terminal: TerminalPanel,
    terminal_open: bool,
}

/// What a press can drag.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DragItem {
    /// A file or folder in the tree.
    Entry(PathBuf),
    /// A tab, by buffer id.
    Tab(u64),
}

#[derive(Debug, Clone, PartialEq)]
struct ItemDrag {
    item: DragItem,
    origin: Point,
    /// The pointer has moved far enough to be dragging.
    moving: bool,
}

/// How far the pointer moves before a press drags.
const DRAG_THRESHOLD: f32 = 4.0;

/// Where a dragged tab would go.
#[derive(Debug, Clone, Copy, PartialEq)]
enum TabDrop {
    /// Into the tab row, before this tab index (the end when past them).
    Row(usize),
    /// Shown in the focused pane, or the other pane.
    Pane { other: bool },
    /// Split the editor, the tab showing on the right.
    SplitRight,
}

/// Two panes side by side: the focused one shows the active buffer, the
/// other this one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Split {
    /// The buffer (by id) in the pane without focus.
    other: u64,
    /// Whether the focused pane is the right one.
    focused_right: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum BarMode {
    GoToLine,
    /// Renaming the symbol whose name starts at this character.
    Rename {
        at: usize,
    },
    NewFile {
        dir: PathBuf,
    },
    NewFolder {
        dir: PathBuf,
    },
    RenameFile {
        from: PathBuf,
    },
}

impl BarMode {
    fn label(&self) -> &'static str {
        match self {
            BarMode::GoToLine => "Go to line",
            BarMode::Rename { .. } => "Rename to",
            BarMode::NewFile { .. } => "New file",
            BarMode::NewFolder { .. } => "New folder",
            BarMode::RenameFile { .. } => "Rename to",
        }
    }

    fn action(&self) -> &'static str {
        match self {
            BarMode::GoToLine => "Go",
            BarMode::Rename { .. } | BarMode::RenameFile { .. } => "Rename",
            BarMode::NewFile { .. } | BarMode::NewFolder { .. } => "Create",
        }
    }
}

/// A search to run (see `search::search`), replacing any still running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchRequest {
    pub id: u64,
    pub root: PathBuf,
    pub query: String,
}

/// The Find in Files panel.
struct SearchPanel {
    field: TextFieldModel,
    results: Vec<SearchMatch>,
    /// The search running, if one is.
    running: Option<u64>,
    /// How the last search went.
    summary: Option<SearchOutcome>,
    scroll: usize,
}

const SEARCH_HEADER_HEIGHT: f32 = 34.0;

/// The completion list under the caret.
struct CompletionPopup {
    items: Vec<CompletionItem>,
    selected: usize,
    /// Where the word being completed starts.
    anchor: usize,
    /// The newest request, while it is unanswered.
    request: Option<u64>,
    /// Rows on screen, for clicks: item index and rect.
    rows: Vec<(usize, Rect)>,
}

const COMPLETION_ROWS: usize = 10;
const COMPLETION_ROW_HEIGHT: f32 = 22.0;

/// How long the pointer rests on a symbol before its hover shows.
const HOVER_DELAY: Duration = Duration::from_millis(450);

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
            dialog: None,
            dialog_for: DialogFor::OpenFolder,
            last_save_dir: None,
            save_queue: Vec::new(),
            dialog_clock: now,
            dialog_animating: false,
            quit_prompt: false,
            quit_after_save: false,
            quitting: false,
            check_outbox: None,
            next_check_id: 1,
            check_running: None,
            diagnostics: Vec::new(),
            check_summary: None,
            problems_open: false,
            problems_scroll: 0,
            marks: Vec::new(),
            pending_goto: None,
            lsp: None,
            lsp_start: None,
            lsp_stop: false,
            lsp_failure: None,
            hover: None,
            hover_wait: None,
            hover_request: None,
            definition_request: None,
            back_stack: Vec::new(),
            frame_time: now,
            completion: None,
            bar_mode: BarMode::GoToLine,
            rename_request: None,
            pending_edits: HashMap::new(),
            trash_prompt: None,
            search: None,
            search_open: false,
            search_outbox: None,
            next_search_id: 1,
            split: None,
            previous_active: None,
            item_drag: None,
            file_drag_over: false,
            terminal: TerminalPanel::default(),
            terminal_open: false,
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
                editor.ensure_page();
            }
            None => editor.ensure_page(),
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
        if let Some((_, _, since)) = self.hover_wait {
            consider(since + HOVER_DELAY);
        }
        wake.map(|at| at.saturating_duration_since(now))
    }

    fn caret_blinking(&self, now: Instant) -> bool {
        self.window_focused
            && now.duration_since(self.last_input) < BLINK_FOR
            && self.focus != Focus::Terminal
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
        if self.lsp.take().is_some() {
            self.lsp_stop = true;
        }
        self.lsp_failure = None;
        self.tree = Some(FileTree::new(root));
        self.touch_session();
    }

    /// Opens `path` in a tab (or switches to its tab).
    pub fn open_file(&mut self, path: PathBuf) {
        if let Some(index) = self
            .buffers
            .iter()
            .position(|buffer| buffer.untitled.is_none() && buffer.path == path)
        {
            self.activate(index);
            return;
        }
        let id = self.next_id;
        self.next_id += 1;
        self.buffers.push(Buffer::new(id, path.clone()));
        self.outbox.push(IoRequest::ReadFile { path });
        // A blank page nobody typed in gives way to the file.
        while let Some(index) = self.buffers.iter().position(Buffer::untouched_page) {
            self.close(index);
        }
        if let Some(index) = self.buffer_index(id) {
            self.activate(index);
        }
    }

    fn activate(&mut self, index: usize) {
        if let Some(previous) = self.active_buffer_mut() {
            let _ = previous.area.handle_event(UiEvent::FocusChanged(false));
        }
        let previous_id = self.active_buffer().map(|buffer| buffer.id);
        if previous_id != Some(self.buffers[index].id) {
            self.previous_active = previous_id;
        }
        self.active = Some(index);
        self.focus = Focus::Editor;
        let path = self.buffers[index]
            .untitled
            .is_none()
            .then(|| self.buffers[index].path.clone());
        let area = &mut self.buffers[index].area;
        let _ = area.handle_event(UiEvent::FocusChanged(true));
        if let Some(tree) = &mut self.tree {
            tree.selected = path;
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
        if let Some(lsp) = &mut self.lsp {
            lsp.close_document(&buffer.path);
        }
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
        let was_active = self.active == Some(index);
        self.active = match self.active {
            _ if self.buffers.is_empty() => None,
            Some(active) if active > index => Some(active - 1),
            // The focused pane's file closed: in a split it takes the other
            // pane's file.
            Some(_) if was_active && self.split.is_some() => self
                .split
                .and_then(|split| self.buffer_index(split.other))
                .or(Some(index.min(self.buffers.len() - 1))),
            Some(active) if active == index => Some(index.min(self.buffers.len() - 1)),
            other => other,
        };
        if let Some(active) = self.active {
            self.activate(active);
        }
        self.repair_split();
        self.ensure_page();
        self.touch_session();
    }

    /// Keeps the second pane showing another open file than the focused
    /// one; with no other file left the split closes.
    fn repair_split(&mut self) {
        let Some(split) = self.split else {
            return;
        };
        let active = self.active_buffer().map(|buffer| buffer.id);
        if self.buffer_index(split.other).is_some() && Some(split.other) != active {
            return;
        }
        let replacement = self
            .previous_active
            .filter(|id| Some(*id) != active && self.buffer_index(*id).is_some())
            .or_else(|| {
                self.buffers
                    .iter()
                    .map(|buffer| buffer.id)
                    .find(|id| Some(*id) != active)
            });
        self.split = replacement.map(|other| Split { other, ..split });
    }

    fn save(&mut self, index: usize) {
        if self.buffers[index].untitled.is_some() {
            let id = self.buffers[index].id;
            if self.dialog.is_none() {
                self.show_save_dialog(id);
            } else if !self.save_queue.contains(&id) {
                self.save_queue.push(id);
            }
            return;
        }
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
        // Files are written at once; pages wait for their names, one dialog
        // after another.
        for &index in &dirty {
            if self.buffers[index].untitled.is_none() {
                self.save(index);
            }
        }
        for index in dirty {
            let id = self.buffers[index].id;
            if self.buffers[index].untitled.is_some() && !self.save_queue.contains(&id) {
                self.save_queue.push(id);
            }
        }
        self.next_in_save_queue();
    }

    /// A new blank page in a tab of its own.
    fn new_page(&mut self) {
        let number = (1..)
            .find(|n| {
                !self
                    .buffers
                    .iter()
                    .any(|buffer| buffer.untitled == Some(*n))
            })
            .expect("a free number");
        let id = self.next_id;
        self.next_id += 1;
        self.buffers.push(Buffer::untitled(id, number));
        self.activate(self.buffers.len() - 1);
    }

    /// With no tab open (a first launch, or the last tab closed), a blank page
    /// to start writing in.
    fn ensure_page(&mut self) {
        if self.buffers.is_empty() && !self.awaiting_session && !self.quitting {
            self.new_page();
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
            IoResponse::FileOpDone { op, result } => self.apply_file_op(op, result),
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
                if let Some(edits) = self.pending_edits.remove(&path) {
                    self.apply_edits(index, &edits);
                }
                if self
                    .pending_goto
                    .as_ref()
                    .is_some_and(|(file, _, _, _)| *file == path)
                {
                    if let Some((_, line, column, length)) = self.pending_goto.take() {
                        self.go_to(index, line, column);
                        if length > 0 {
                            let area = &mut self.buffers[index].area;
                            let start = area.caret();
                            area.select_range(start, start + length);
                        }
                    }
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
                if std::mem::take(&mut buffer.newly_named) {
                    if let Some(tree) = &mut self.tree {
                        tree.refresh();
                    }
                }
                let buffer = &mut self.buffers[index];
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
                if !self.quitting && path.extension().is_some_and(|ext| ext == "rs") {
                    self.request_check(path.to_path_buf());
                }
                if let Some(lsp) = &mut self.lsp {
                    lsp.save_document(path);
                }
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
            if let Some(caret) = buffer.restore_caret.take() {
                buffer.area.set_caret(caret);
            }
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
            self.ensure_page();
            return;
        };
        self.last_save_dir = session.last_save_dir.clone();
        match &self.tree {
            Some(tree) if tree.root() != session.root => {
                self.ensure_page();
                return;
            }
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
            match tab.untitled {
                Some(number) => {
                    let id = self.next_id;
                    self.next_id += 1;
                    let mut page = Buffer::untitled(id, number);
                    // Its text comes with its backup; the caret goes back then.
                    page.restore_caret = Some(tab.caret);
                    self.buffers.push(page);
                }
                None => {
                    self.open_file(tab.path.clone());
                    if let Some(buffer) = self.buffers.last_mut() {
                        buffer.restore_caret = Some(tab.caret);
                    }
                }
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
            .filter(|tab| tab.untitled.is_none())
            .map(|tab| tab.path.clone());
        if let Some(index) = session.active.filter(|&index| index < self.buffers.len()) {
            self.activate(index);
        }
        self.ensure_page();
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
        self.frame_time = now;
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
            || input.mouse_wheel_x != 0.0
            || !input.control_text.is_empty()
            || !input.dropped_files.is_empty()
            || input.file_drag_over != self.file_drag_over;
        if had_input {
            self.last_input = now;
            self.blink_origin = now;
        }

        for &command in &input.menu_commands {
            if let Some(command) = Command::from_menu(command) {
                self.run(command, host);
            }
        }
        if self.dialog.is_some() {
            self.route_dialog(input, now);
        } else {
            self.route_file_drops(input);
            self.route_pointer(input, host);
            for event in &input.key_events {
                self.route_key(event.key, event.modifiers, host);
            }
            if !input.typed_text.is_empty() {
                self.route_text(&input.typed_text);
            }
            if self.focus == Focus::Terminal && !input.control_text.is_empty() {
                self.terminal.text(&input.control_text);
            }
        }

        self.relayout(surface, host);
        if let Some(buffer) = self.active_buffer_mut() {
            if let (true, Some(highlighter)) = (buffer.loaded, &mut buffer.highlighter) {
                highlighter.update(&mut buffer.area);
            }
        }
        if let Some(index) = self.split.and_then(|split| self.buffer_index(split.other)) {
            let buffer = &mut self.buffers[index];
            if let (true, Some(highlighter)) = (buffer.loaded, &mut buffer.highlighter) {
                highlighter.update(&mut buffer.area);
            }
        }
        self.update_marks();
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
        self.sync_language_server();
        self.update_hover(now);
        FrameOutcome {
            text_cursor: self.wants_text_cursor(),
            menu_bar: self.publish_menu(),
            quit: self.quitting,
        }
    }

    // ----- rust-analyzer -----

    /// The folder to start rust-analyzer for, once, when it is wanted.
    pub fn take_lsp_start(&mut self) -> Option<PathBuf> {
        self.lsp_start.take()
    }

    /// Whether the running server should be stopped (asked once).
    pub fn take_lsp_stop(&mut self) -> bool {
        std::mem::take(&mut self.lsp_stop)
    }

    /// Messages for the server.
    pub fn lsp_outgoing(&mut self) -> Vec<serde_json::Value> {
        self.lsp
            .as_mut()
            .map(LspSession::take_outgoing)
            .unwrap_or_default()
    }

    /// Messages from the server.
    pub fn apply_lsp(&mut self, messages: Vec<serde_json::Value>) {
        let Some(lsp) = &mut self.lsp else {
            return;
        };
        for message in messages {
            lsp.handle(message);
        }
        for event in lsp.take_events() {
            self.lsp_event(event);
        }
    }

    /// The server's output ended: it is not restarted on its own; opening
    /// the folder again starts a new one.
    pub fn lsp_ended(&mut self, reason: String) {
        if self.lsp.take().is_some() {
            self.set_status(
                format!("rust-analyzer stopped: {reason}"),
                StatusKind::Warning,
            );
            self.lsp_failure = Some(reason);
        }
    }

    /// Starts a session for the folder once a Rust file is open, and keeps
    /// every open Rust file's text in step with the server.
    fn sync_language_server(&mut self) {
        let wants_server = self
            .buffers
            .iter()
            .any(|buffer| buffer.loaded && buffer.path.extension().is_some_and(|ext| ext == "rs"));
        if self.lsp.is_none() && wants_server && self.lsp_failure.is_none() && !self.quitting {
            if let Some(tree) = &self.tree {
                let root = tree.root().to_path_buf();
                self.lsp = Some(LspSession::new(&root));
                self.lsp_start = Some(root);
            }
        }
        let Some(lsp) = &mut self.lsp else {
            return;
        };
        for buffer in &self.buffers {
            if !buffer.loaded || buffer.path.extension().is_none_or(|ext| ext != "rs") {
                continue;
            }
            let revision = buffer.area.revision();
            if !lsp.is_open(&buffer.path) {
                lsp.open_document(&buffer.path, &buffer.area.text(), revision);
            } else if !lsp.in_sync(&buffer.path, revision) {
                lsp.change_document(&buffer.path, &buffer.area.text(), revision);
            }
        }
    }

    fn lsp_event(&mut self, event: LspEvent) {
        match event {
            LspEvent::Hover { request, text } if self.hover_request == Some(request) => {
                self.hover_request = None;
                if let Some((point, _, _)) = self.hover_wait.take() {
                    if !text.trim().is_empty() {
                        self.hover = Some((text, point));
                    }
                }
            }
            LspEvent::Definition { request, locations }
                if self.definition_request == Some(request) =>
            {
                self.definition_request = None;
                match locations.first() {
                    Some(location) => {
                        self.remember_place();
                        let path = location.path.clone();
                        let (line, column) = (
                            location.range.start.line + 1,
                            location.range.start.character + 1,
                        );
                        self.open_file(path.clone());
                        if let Some(index) = self.active {
                            if self.buffers[index].loaded {
                                self.go_to(index, line, column);
                            } else {
                                self.pending_goto = Some((path, line, column, 0));
                            }
                        }
                    }
                    None => self.set_status("No definition found.", StatusKind::Info),
                }
            }
            LspEvent::Completion { request, items } => {
                if let Some(popup) = &mut self.completion {
                    if popup.request == Some(request) {
                        popup.request = None;
                        if items.is_empty() {
                            self.completion = None;
                        } else {
                            popup.items = items;
                            popup.selected = 0;
                        }
                    }
                }
            }
            LspEvent::Edit { request, edits } if self.rename_request == Some(request) => {
                self.rename_request = None;
                self.apply_workspace_edit(edits);
            }
            LspEvent::Failed { request, message } => {
                if Some(request) == self.rename_request {
                    self.rename_request = None;
                    self.set_status(format!("Rename: {message}"), StatusKind::Warning);
                }
                if Some(request) == self.definition_request {
                    self.definition_request = None;
                    self.set_status(format!("Go to Definition: {message}"), StatusKind::Warning);
                }
                if Some(request) == self.hover_request {
                    self.hover_request = None;
                }
            }
            _ => {}
        }
    }

    /// Where buffer `index`'s character `char_index` is, as LSP counts.
    fn lsp_position(&mut self, index: usize, char_index: usize) -> Position {
        let area = &mut self.buffers[index].area;
        let line = area.line_of_char(char_index);
        Position {
            line,
            character: char_index - area.line_start_char(line),
        }
    }

    fn request_definition(&mut self, char_index: usize) {
        let Some(index) = self.active else {
            return;
        };
        self.sync_language_server();
        let at = self.lsp_position(index, char_index);
        let path = self.buffers[index].path.clone();
        let Some(lsp) = &mut self.lsp else {
            self.set_status("rust-analyzer is not running.", StatusKind::Info);
            return;
        };
        self.definition_request = lsp.definition(&path, at);
        if self.definition_request.is_none() {
            self.set_status("rust-analyzer is still starting.", StatusKind::Info);
        }
    }

    /// Cmd-click (Ctrl elsewhere) on a name: go to its definition. True
    /// when the click was on text.
    fn definition_at_point(&mut self, point: Point) -> bool {
        let Some(char_index) = self
            .active_buffer()
            .filter(|buffer| buffer.loaded)
            .and_then(|buffer| buffer.area.char_at_point(point))
        else {
            return false;
        };
        self.request_definition(char_index);
        true
    }

    /// After text was typed into the editor: ask for completions when it
    /// continues a name or follows `.` or `::`; close the list otherwise.
    fn after_typing(&mut self, text: &str) {
        let Some(last) = text.chars().last() else {
            return;
        };
        let Some(index) = self.active else {
            return;
        };
        let buffer = &self.buffers[index];
        if !buffer.loaded || buffer.highlighter.is_none() {
            return;
        }
        let caret = buffer.area.caret();
        let before = |n: usize| {
            caret
                .checked_sub(n)
                .map(|start| buffer.area.document.slice_chars(start, caret))
        };
        let continues_name = last.is_alphanumeric() || last == '_';
        let after_path = last == '.' || before(2).as_deref() == Some("::");
        if !continues_name && !after_path {
            self.completion = None;
            return;
        }
        // The word being completed starts after the last non-name character.
        let line_start = {
            let area = &mut self.buffers[index].area;
            let line = area.line_of_char(caret);
            area.line_start_char(line)
        };
        let typed: Vec<char> = self.buffers[index]
            .area
            .document
            .slice_chars(line_start, caret)
            .chars()
            .collect();
        let word = typed
            .iter()
            .rev()
            .take_while(|ch| ch.is_alphanumeric() || **ch == '_')
            .count();
        let anchor = caret - word;
        // The server must have the text just typed before it is asked.
        self.sync_language_server();
        let at = self.lsp_position(index, caret);
        let path = self.buffers[index].path.clone();
        let Some(request) = self
            .lsp
            .as_mut()
            .filter(|lsp| lsp.ready)
            .and_then(|lsp| lsp.completion(&path, at))
        else {
            return;
        };
        match &mut self.completion {
            Some(popup) if popup.anchor == anchor => popup.request = Some(request),
            _ => {
                self.completion = Some(CompletionPopup {
                    items: Vec::new(),
                    selected: 0,
                    anchor,
                    request: Some(request),
                    rows: Vec::new(),
                });
            }
        }
    }

    /// After a deletion with the list open: ask again for what is left of
    /// the word, or close the list once the caret is before it.
    fn refresh_completion(&mut self) {
        let (Some(anchor), Some(index)) = (self.completion.as_ref().map(|p| p.anchor), self.active)
        else {
            return;
        };
        let caret = self.buffers[index].area.caret();
        if caret < anchor {
            self.completion = None;
            return;
        }
        self.sync_language_server();
        let at = self.lsp_position(index, caret);
        let path = self.buffers[index].path.clone();
        let request = self.lsp.as_mut().and_then(|lsp| lsp.completion(&path, at));
        if let Some(popup) = &mut self.completion {
            popup.request = request;
        }
    }

    /// The word typed since the completion list opened.
    fn completion_prefix(&self) -> String {
        match (&self.completion, self.active_buffer()) {
            (Some(popup), Some(buffer)) => {
                let caret = buffer.area.caret();
                if caret >= popup.anchor {
                    buffer.area.document.slice_chars(popup.anchor, caret)
                } else {
                    String::new()
                }
            }
            _ => String::new(),
        }
    }

    /// The items the list shows: those still matching what was typed since
    /// the server answered.
    fn visible_completions(&self) -> Vec<usize> {
        let Some(popup) = &self.completion else {
            return Vec::new();
        };
        let prefix = self.completion_prefix().to_lowercase();
        popup
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.label.to_lowercase().contains(&prefix))
            .map(|(index, _)| index)
            .collect()
    }

    /// A key while the list is open; true when the list took it.
    fn completion_key(&mut self, key: Key) -> bool {
        let visible = self.visible_completions();
        let Some(popup) = &mut self.completion else {
            return false;
        };
        let position = visible.iter().position(|&item| item == popup.selected);
        match key {
            Key::Up | Key::Down if !visible.is_empty() => {
                let next = match (key, position) {
                    (Key::Down, Some(at)) => (at + 1).min(visible.len() - 1),
                    (Key::Up, Some(at)) => at.saturating_sub(1),
                    _ => 0,
                };
                popup.selected = visible[next];
                true
            }
            Key::Enter | Key::Tab if !visible.is_empty() => {
                if position.is_none() {
                    popup.selected = visible[0];
                }
                self.accept_completion();
                true
            }
            Key::Escape => {
                self.completion = None;
                true
            }
            Key::Backspace => {
                // Let the deletion happen, then ask again or close.
                false
            }
            _ => {
                self.completion = None;
                false
            }
        }
    }

    /// Inserts the selected completion: its edit, stretched to the caret
    /// (the user may have typed on since asking), and its further edits.
    fn accept_completion(&mut self) {
        let Some(popup) = self.completion.take() else {
            return;
        };
        let Some(item) = popup.items.get(popup.selected).cloned() else {
            return;
        };
        let Some(index) = self.active else {
            return;
        };
        let caret = self.buffers[index].area.caret();
        let (start, text) = match &item.edit {
            Some(edit) => (
                self.char_index(index, edit.range.start),
                edit.new_text.clone(),
            ),
            None => (popup.anchor, item.insert_text.clone()),
        };
        let mut edits: Vec<(usize, usize, String)> = vec![(start.min(caret), caret, text)];
        for extra in &item.additional_edits {
            let from = self.char_index(index, extra.range.start);
            let to = self.char_index(index, extra.range.end);
            edits.push((from, to, extra.new_text.clone()));
        }
        // From the end of the file back, so earlier positions stay valid.
        edits.sort_by_key(|edit| std::cmp::Reverse(edit.0));
        let main_start = start.min(caret);
        let mut caret_after = None;
        let mut shift: isize = 0;
        for (from, to, text) in &edits {
            let area = &mut self.buffers[index].area;
            area.replace_range(*from, *to, text);
            if *from == main_start && caret_after.is_none() {
                caret_after = Some(from + text.chars().count());
            } else if *from < main_start {
                shift += text.chars().count() as isize - (to - from) as isize;
            }
        }
        if let Some(after) = caret_after {
            let target = (after as isize + shift).max(0) as usize;
            self.buffers[index].area.set_caret(target);
        }
    }

    /// The character index of LSP position `at` in buffer `index`.
    fn char_index(&mut self, index: usize, at: Position) -> usize {
        let area = &mut self.buffers[index].area;
        let start = area.line_start_char(at.line);
        let next = area.line_start_char(at.line + 1);
        let end = if next > start {
            next - 1
        } else {
            area.document.len_chars()
        };
        (start + at.character).min(end)
    }

    fn paint_completion(&mut self, scene: &mut Vec<PaintOp>, bounds: Rect) {
        let visible = self.visible_completions();
        let caret = self
            .active_buffer()
            .and_then(|buffer| buffer.area.layout_cache.caret_rect);
        let Some(popup) = &mut self.completion else {
            return;
        };
        popup.rows.clear();
        let (Some(caret), false) = (caret, visible.is_empty()) else {
            return;
        };
        let selected_at = visible
            .iter()
            .position(|&item| item == popup.selected)
            .unwrap_or(0);
        let first = selected_at.saturating_sub(COMPLETION_ROWS - 1);
        let shown: Vec<usize> = visible
            .iter()
            .copied()
            .skip(first)
            .take(COMPLETION_ROWS)
            .collect();
        let width = 440.0f32.min(bounds.width - 16.0);
        let height = shown.len() as f32 * COMPLETION_ROW_HEIGHT + 6.0;
        let x = caret
            .x
            .min(bounds.right() - width - 4.0)
            .max(bounds.x + 4.0);
        let below = caret.y + caret.height + 4.0;
        let y = if below + height <= bounds.bottom() {
            below
        } else {
            (caret.y - height - 4.0).max(bounds.y)
        };
        let rect = Rect {
            x,
            y,
            width,
            height,
        };
        draw::fill(scene, rect, theme::PANEL);
        scene.push(PaintOp::StrokeRect {
            rect,
            color: theme::BORDER,
        });
        for (row, item_index) in shown.iter().enumerate() {
            let item = &popup.items[*item_index];
            let row_rect = Rect {
                x: x + 3.0,
                y: y + 3.0 + row as f32 * COMPLETION_ROW_HEIGHT,
                width: width - 6.0,
                height: COMPLETION_ROW_HEIGHT,
            };
            if *item_index == popup.selected || (row == 0 && !visible.contains(&popup.selected)) {
                draw::fill(scene, row_rect, theme::SELECTED);
            }
            draw::label(
                scene,
                &item.label,
                Rect {
                    x: row_rect.x + 8.0,
                    width: row_rect.width * 0.55,
                    ..row_rect
                },
                theme::TEXT,
                theme::UI_FONT,
                HorizontalAlign::Left,
            );
            if let Some(detail) = &item.detail {
                draw::label(
                    scene,
                    detail,
                    Rect {
                        x: row_rect.x + row_rect.width * 0.55 + 12.0,
                        width: row_rect.width * 0.45 - 20.0,
                        ..row_rect
                    },
                    theme::TEXT_DIM,
                    theme::UI_FONT,
                    HorizontalAlign::Right,
                );
            }
            popup.rows.push((*item_index, row_rect));
        }
    }

    /// Focuses the other pane: its buffer becomes the active one.
    fn switch_pane(&mut self) {
        let Some(split) = self.split else {
            return;
        };
        let Some(other_index) = self.buffer_index(split.other) else {
            return;
        };
        let current = self.active_buffer().map(|buffer| buffer.id);
        self.activate(other_index);
        self.split = Some(Split {
            other: current.unwrap_or(split.other),
            focused_right: !split.focused_right,
        });
    }

    // ----- terminal -----

    fn new_terminal(&mut self) {
        let cwd = self
            .tree
            .as_ref()
            .map(|tree| tree.root().to_path_buf())
            .unwrap_or_else(|| self.fallback_root.clone());
        self.terminal.open(cwd, shell_name());
    }

    /// What the app should do with the terminals' programs.
    pub fn take_terminal_requests(&mut self) -> Vec<TerminalRequest> {
        self.terminal.take_requests()
    }

    /// Output from terminal `id`'s program.
    pub fn terminal_output(&mut self, id: u64, bytes: &[u8]) {
        self.terminal.output(id, bytes);
    }

    /// Terminal `id`'s program ended (or never started).
    pub fn terminal_ended(&mut self, id: u64, reason: String) {
        self.terminal.ended(id, reason);
    }

    // ----- find in files -----

    /// The search to start, if one was asked for since the last call.
    pub fn take_search_request(&mut self) -> Option<SearchRequest> {
        self.search_outbox.take()
    }

    fn run_search(&mut self) {
        let Some(root) = self.tree.as_ref().map(|tree| tree.root().to_path_buf()) else {
            return;
        };
        let Some(search) = &mut self.search else {
            return;
        };
        let query = search.field.text();
        if query.is_empty() {
            return;
        }
        let id = self.next_search_id;
        self.next_search_id += 1;
        search.running = Some(id);
        search.scroll = 0;
        self.search_outbox = Some(SearchRequest { id, root, query });
    }

    /// Takes in a finished search; a replaced one's result is ignored.
    pub fn apply_search(&mut self, id: u64, outcome: SearchOutcome) {
        let Some(search) = &mut self.search else {
            return;
        };
        if search.running != Some(id) {
            return;
        }
        search.running = None;
        search.results = outcome.matches.clone();
        search.summary = Some(outcome);
    }

    fn search_panel_pressed(
        &mut self,
        point: Point,
        panel: Rect,
        state: PointerState,
        host: &dyn EditorHost,
    ) {
        if self
            .layout
            .search_button
            .is_some_and(|rect| rect.contains(point))
        {
            self.run(Command::RunSearch, host);
            return;
        }
        if self
            .layout
            .search_field
            .is_some_and(|rect| rect.contains(point))
        {
            self.set_focus(Focus::Search);
            if let Some(search) = &mut self.search {
                let _ = search.field.handle_event(UiEvent::PointerPressed {
                    button: PointerButton::Primary,
                    state,
                });
            }
            return;
        }
        let row = ((point.y - panel.y - SEARCH_HEADER_HEIGHT) / PROBLEM_ROW_HEIGHT).floor();
        let found = self.search.as_ref().and_then(|search| {
            (row >= 0.0)
                .then(|| search.results.get(search.scroll + row as usize).cloned())
                .flatten()
        });
        if let Some(found) = found {
            self.remember_place();
            self.open_file(found.path.clone());
            let Some(index) = self.active else {
                return;
            };
            if self.buffers[index].loaded {
                self.select_match(index, &found);
            } else {
                self.pending_goto = Some((
                    found.path.clone(),
                    found.line,
                    found.column,
                    found.end_column - found.column,
                ));
            }
        }
    }

    /// Selects a search match in buffer `index`.
    fn select_match(&mut self, index: usize, found: &SearchMatch) {
        self.go_to(index, found.line, found.column);
        let area = &mut self.buffers[index].area;
        let start = area.caret();
        area.select_range(start, start + (found.end_column - found.column));
    }

    fn paint_search(&self, scene: &mut Vec<PaintOp>, panel: Rect, layout: &Layout) {
        let Some(search) = &self.search else {
            return;
        };
        draw::fill(scene, panel, theme::PANEL);
        draw::hline(scene, panel.x, panel.y + 0.5, panel.width, theme::BORDER);
        draw::label(
            scene,
            "Find in files",
            Rect {
                x: panel.x + 12.0,
                y: panel.y,
                width: 104.0,
                height: SEARCH_HEADER_HEIGHT,
            },
            theme::TEXT_DIM,
            theme::UI_FONT,
            HorizontalAlign::Left,
        );
        search.field.paint(scene);
        if let Some(button) = layout.search_button {
            draw::button(scene, button, "Search", button.contains(self.pointer), true);
            let summary = match (&search.summary, search.running) {
                (_, Some(_)) => "Searching…".to_string(),
                (Some(outcome), None) => {
                    let files: std::collections::HashSet<&Path> =
                        outcome.matches.iter().map(|m| m.path.as_path()).collect();
                    format!(
                        "{} {} in {} of {} files{} ({:.1} s)",
                        outcome.matches.len(),
                        if outcome.matches.len() == 1 {
                            "match"
                        } else {
                            "matches"
                        },
                        files.len(),
                        outcome.files_searched,
                        if outcome.truncated {
                            ", stopped at the limit"
                        } else {
                            ""
                        },
                        outcome.elapsed.as_secs_f32()
                    )
                }
                (None, None) => "Searches files as saved; build output is skipped.".to_string(),
            };
            draw::label(
                scene,
                &summary,
                Rect {
                    x: button.right() + 14.0,
                    y: panel.y,
                    width: (panel.right() - button.right() - 26.0).max(0.0),
                    height: SEARCH_HEADER_HEIGHT,
                },
                theme::TEXT_DIM,
                theme::UI_FONT,
                HorizontalAlign::Left,
            );
        }
        let rows = ((panel.height - SEARCH_HEADER_HEIGHT) / PROBLEM_ROW_HEIGHT)
            .floor()
            .max(0.0) as usize;
        let place_width = (panel.width * 0.3).min(320.0);
        for (row, found) in search
            .results
            .iter()
            .skip(search.scroll)
            .take(rows)
            .enumerate()
        {
            let rect = Rect {
                x: panel.x,
                y: panel.y + SEARCH_HEADER_HEIGHT + PROBLEM_ROW_HEIGHT * row as f32,
                width: panel.width,
                height: PROBLEM_ROW_HEIGHT,
            };
            if rect.contains(self.pointer) {
                draw::fill(scene, rect, theme::HOVER);
            }
            draw::label(
                scene,
                &format!("{}:{}", self.shown_path(&found.path), found.line),
                Rect {
                    x: rect.x + 12.0,
                    width: place_width,
                    ..rect
                },
                theme::TEXT_DIM,
                theme::UI_FONT,
                HorizontalAlign::Left,
            );
            draw::label(
                scene,
                &found.text,
                Rect {
                    x: rect.x + 24.0 + place_width,
                    width: (rect.width - place_width - 36.0).max(0.0),
                    ..rect
                },
                theme::TEXT,
                theme::UI_FONT,
                HorizontalAlign::Left,
            );
        }
    }

    /// What the file commands act on: the tree's selection, else the open
    /// file.
    fn file_target(&self) -> Option<PathBuf> {
        self.tree
            .as_ref()
            .and_then(|tree| tree.selected.clone())
            .filter(|path| self.tree.as_ref().is_some_and(|tree| path != tree.root()))
            .or_else(|| self.active_buffer().map(|buffer| buffer.path.clone()))
    }

    /// `path` relative to the folder, for messages.
    fn shown_path(&self, path: &Path) -> String {
        self.tree
            .as_ref()
            .and_then(|tree| path.strip_prefix(tree.root()).ok())
            .unwrap_or(path)
            .display()
            .to_string()
    }

    /// Opens the name bar for a new file or folder, in the selected folder
    /// (or the selected file's folder, or the folder's root).
    fn start_file_bar(&mut self, file: bool) {
        let Some(root) = self.tree.as_ref().map(|tree| tree.root().to_path_buf()) else {
            return;
        };
        let dir = match self.file_target() {
            Some(path) if self.tree_is_dir(&path) => path,
            Some(path) => path.parent().map_or(root.clone(), Path::to_path_buf),
            None => root,
        };
        self.find = None;
        self.bar_mode = if file {
            BarMode::NewFile { dir }
        } else {
            BarMode::NewFolder { dir }
        };
        let field = self
            .goto
            .get_or_insert_with(|| TextFieldModel::new("", Rect::default()));
        style_field(field);
        field.set_text("");
        self.set_focus(Focus::Goto);
    }

    fn tree_is_dir(&mut self, path: &Path) -> bool {
        self.tree
            .as_mut()
            .is_some_and(|tree| tree.rows().iter().any(|row| row.path == path && row.is_dir))
    }

    fn start_rename_file(&mut self) {
        let Some(from) = self.file_target() else {
            return;
        };
        let name = from
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.find = None;
        self.bar_mode = BarMode::RenameFile { from };
        let field = self
            .goto
            .get_or_insert_with(|| TextFieldModel::new("", Rect::default()));
        style_field(field);
        field.set_text(&name);
        // Select the stem, so typing replaces the name and keeps the
        // extension.
        let stem = Path::new(&name)
            .file_stem()
            .map_or(0, |stem| stem.to_string_lossy().chars().count());
        field.select_range(0, stem);
        self.set_focus(Focus::Goto);
    }

    /// The name bar's Enter for file operations: sends `op` for the typed
    /// name.
    fn finish_file_op(&mut self, op: impl FnOnce(&str) -> FileOp) {
        let name = self
            .goto
            .as_ref()
            .map(|field| field.text().trim().to_string())
            .unwrap_or_default();
        self.goto = None;
        self.bar_mode = BarMode::GoToLine;
        self.set_focus(Focus::Editor);
        if name.is_empty() || name.contains("..") {
            if !name.is_empty() {
                self.set_status("Names cannot contain \"..\".", StatusKind::Warning);
            }
            return;
        }
        self.outbox.push(IoRequest::FileOp(op(&name)));
    }

    fn apply_file_op(&mut self, op: FileOp, result: Result<(), String>) {
        if let Err(error) = result {
            let what = match &op {
                FileOp::CreateFile(path) | FileOp::CreateFolder(path) => {
                    format!("Could not create {}", self.shown_path(path))
                }
                FileOp::Rename { from, .. } => {
                    format!("Could not rename {}", self.shown_path(from))
                }
                FileOp::Trash(path) => {
                    format!("Could not move {} to the Trash", self.shown_path(path))
                }
                FileOp::Copy { from, .. } => format!("Could not copy {}", self.shown_path(from)),
            };
            self.set_status(format!("{what}: {error}"), StatusKind::Error);
            return;
        }
        if let Some(tree) = &mut self.tree {
            tree.refresh();
        }
        match op {
            FileOp::CreateFile(path) => {
                if let Some(tree) = &mut self.tree {
                    tree.reveal(&path);
                }
                self.open_file(path);
            }
            FileOp::CreateFolder(path) => {
                if let Some(tree) = &mut self.tree {
                    tree.reveal(&path);
                    tree.set_expanded(&path, true);
                }
            }
            FileOp::Rename { from, to } => self.renamed(&from, &to),
            FileOp::Copy { to, .. } => {
                if let Some(tree) = &mut self.tree {
                    tree.reveal(&to);
                    tree.selected = Some(to.clone());
                }
                let shown = self.shown_path(&to);
                self.set_status(format!("Copied to {shown}."), StatusKind::Info);
            }
            FileOp::Trash(path) => {
                let shown = self.shown_path(&path);
                while let Some(index) = self.buffers.iter().position(|b| b.path.starts_with(&path))
                {
                    self.close(index);
                }
                if let Some(tree) = &mut self.tree {
                    tree.selected = None;
                }
                self.set_status(format!("Moved {shown} to the Trash."), StatusKind::Info);
            }
        }
        self.touch_session();
    }

    /// Open files at or under `from` now live under `to`.
    fn renamed(&mut self, from: &Path, to: &Path) {
        let state_dir = self.state_dir.clone();
        for buffer in &mut self.buffers {
            let Ok(rest) = buffer.path.strip_prefix(from) else {
                continue;
            };
            let old = buffer.path.clone();
            let new = if rest.as_os_str().is_empty() {
                to.to_path_buf()
            } else {
                to.join(rest)
            };
            if let Some(lsp) = &mut self.lsp {
                lsp.close_document(&old);
            }
            if buffer.has_backup_file {
                if let Some(state_dir) = &state_dir {
                    self.outbox.push(IoRequest::RemoveState {
                        path: session::backup_path(state_dir, &old),
                    });
                }
                buffer.has_backup_file = false;
                buffer.backup_revision = None;
            }
            buffer.title = new
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            buffer.highlighter = Highlighter::for_path(&new);
            // The disk stamp (modified time, size) moves with the file.
            buffer.path = new;
        }
        if let Some(tree) = &mut self.tree {
            tree.reveal(to);
        }
        let shown = self.shown_path(to);
        let verb = if from.parent() == to.parent() {
            "Renamed"
        } else {
            "Moved"
        };
        self.set_status(format!("{verb} to {shown}."), StatusKind::Info);
        self.backup_due = Some(self.frame_time);
    }

    /// Opens the rename bar with the name under the caret.
    fn start_rename(&mut self) {
        let Some(buffer) = self.active_buffer().filter(|buffer| buffer.loaded) else {
            return;
        };
        let text: Vec<char> = buffer.area.text().chars().collect();
        let caret = buffer.area.caret().min(text.len());
        let is_name = |ch: char| ch.is_alphanumeric() || ch == '_';
        let mut start = caret;
        while start > 0 && is_name(text[start - 1]) {
            start -= 1;
        }
        let mut end = caret;
        while end < text.len() && is_name(text[end]) {
            end += 1;
        }
        if start == end {
            self.set_status("Put the caret on a name to rename it.", StatusKind::Info);
            return;
        }
        let name: String = text[start..end].iter().collect();
        self.find = None;
        self.bar_mode = BarMode::Rename { at: start };
        let field = self
            .goto
            .get_or_insert_with(|| TextFieldModel::new("", Rect::default()));
        style_field(field);
        field.set_text(&name);
        field.select_all();
        self.set_focus(Focus::Goto);
    }

    fn request_rename(&mut self, at: usize) {
        let new_name = self
            .goto
            .as_ref()
            .map(|field| field.text().trim().to_string())
            .unwrap_or_default();
        self.goto = None;
        self.bar_mode = BarMode::GoToLine;
        self.set_focus(Focus::Editor);
        let Some(index) = self.active else {
            return;
        };
        if new_name.is_empty() {
            return;
        }
        self.sync_language_server();
        let position = self.lsp_position(index, at);
        let path = self.buffers[index].path.clone();
        self.rename_request = self
            .lsp
            .as_mut()
            .and_then(|lsp| lsp.rename(&path, position, &new_name));
        if self.rename_request.is_none() {
            self.set_status("rust-analyzer is not ready.", StatusKind::Info);
        }
    }

    /// Applies a rename's edits: to open files now, to the rest once
    /// opened. Every changed file is left unsaved for Save All.
    fn apply_workspace_edit(&mut self, edits: Vec<(PathBuf, Vec<TextEdit>)>) {
        let files = edits.len();
        let active = self.active;
        for (path, file_edits) in edits {
            match self.buffers.iter().position(|buffer| buffer.path == path) {
                Some(index) if self.buffers[index].loaded => self.apply_edits(index, &file_edits),
                _ => {
                    self.pending_edits.insert(path.clone(), file_edits);
                    self.open_file(path);
                }
            }
        }
        if let Some(active) = active.filter(|&index| index < self.buffers.len()) {
            self.activate(active);
        }
        self.set_status(
            if files == 1 {
                "Renamed in 1 file; save to keep it.".to_string()
            } else {
                format!("Renamed in {files} files; Save All keeps them.")
            },
            StatusKind::Info,
        );
    }

    /// Applies `edits` to buffer `index`, from the end of the file back so
    /// each position is read before the text before it moves.
    fn apply_edits(&mut self, index: usize, edits: &[TextEdit]) {
        let caret = self.buffers[index].area.caret();
        let mut ranges: Vec<(usize, usize, String)> = edits
            .iter()
            .map(|edit| {
                (
                    self.char_index(index, edit.range.start),
                    self.char_index(index, edit.range.end),
                    edit.new_text.clone(),
                )
            })
            .collect();
        ranges.sort_by_key(|range| std::cmp::Reverse(range.0));
        let mut shift: isize = 0;
        for (from, to, text) in &ranges {
            self.buffers[index].area.replace_range(*from, *to, text);
            if *to <= caret {
                shift += text.chars().count() as isize - (to - from) as isize;
            }
        }
        let caret = (caret as isize + shift).max(0) as usize;
        self.buffers[index].area.set_caret(caret);
    }

    /// Pushes the active file and caret for Back.
    fn remember_place(&mut self) {
        if let Some(buffer) = self.active_buffer().filter(|buffer| buffer.loaded) {
            let place = (buffer.path.clone(), buffer.area.caret());
            if self.back_stack.last() != Some(&place) {
                self.back_stack.push(place);
                if self.back_stack.len() > 100 {
                    self.back_stack.remove(0);
                }
            }
        }
    }

    /// The pointer moved (or pressed, or scrolled): any hover goes, and
    /// resting on text starts the wait for a new one.
    fn track_hover(&mut self, point: Point, pressing: bool) {
        if let Some((_, at)) = &self.hover {
            if (at.x - point.x).abs() + (at.y - point.y).abs() > 6.0 || pressing {
                self.hover = None;
            }
        }
        self.hover_request = None;
        self.hover_wait = None;
        if pressing || self.lsp.as_ref().is_none_or(|lsp| !lsp.ready) || self.hover.is_some() {
            return;
        }
        let Some(char_index) = self
            .active_buffer()
            .filter(|buffer| buffer.loaded && self.layout.editor.contains(point))
            .and_then(|buffer| buffer.area.char_at_point(point))
        else {
            return;
        };
        self.hover_wait = Some((point, char_index, self.frame_time));
    }

    /// Asks for the hover once the pointer has rested long enough.
    fn update_hover(&mut self, now: Instant) {
        let Some((_, char_index, since)) = self.hover_wait else {
            return;
        };
        if self.hover_request.is_some() || now.duration_since(since) < HOVER_DELAY {
            return;
        }
        let Some(index) = self.active else {
            return;
        };
        self.sync_language_server();
        let at = self.lsp_position(index, char_index);
        let path = self.buffers[index].path.clone();
        self.hover_request = self.lsp.as_mut().and_then(|lsp| lsp.hover(&path, at));
        if self.hover_request.is_none() {
            self.hover_wait = None;
        }
    }

    /// Cargo's diagnostics and rust-analyzer's, for marks and the panel.
    fn shown_diagnostics(&self) -> Vec<&Diagnostic> {
        let mut shown: Vec<&Diagnostic> = self.diagnostics.iter().collect();
        if let Some(lsp) = &self.lsp {
            shown.extend(lsp.all_diagnostics());
        }
        shown
    }

    fn request_check(&mut self, file: PathBuf) {
        let id = self.next_check_id;
        self.next_check_id += 1;
        self.check_running = Some(id);
        self.check_outbox = Some(CheckRequest { id, file });
    }

    /// The `cargo check` to start, if one was asked for since the last call.
    /// Starting it replaces any check still running.
    pub fn take_check_request(&mut self) -> Option<CheckRequest> {
        self.check_outbox.take()
    }

    /// Takes in a finished check; a result for a check since replaced is
    /// ignored.
    pub fn apply_check(&mut self, id: u64, outcome: CheckOutcome) {
        if self.check_running != Some(id) {
            return;
        }
        self.check_running = None;
        if outcome.cancelled {
            return;
        }
        if let Some(failure) = &outcome.failure {
            self.set_status(format!("cargo check failed: {failure}"), StatusKind::Error);
        }
        let errors = outcome
            .diagnostics
            .iter()
            .filter(|d| d.level == Level::Error)
            .count();
        self.check_summary = Some(CheckSummary {
            errors,
            warnings: outcome.diagnostics.len() - errors,
            elapsed: outcome.elapsed,
        });
        self.diagnostics = outcome.diagnostics;
        self.problems_scroll = 0;
    }

    fn go_to_diagnostic(&mut self, diagnostic: &Diagnostic) {
        self.remember_place();
        self.open_file(diagnostic.file.clone());
        let Some(index) = self.active else {
            return;
        };
        if self.buffers[index].loaded {
            self.go_to(index, diagnostic.line, diagnostic.column);
        } else {
            self.pending_goto = Some((
                diagnostic.file.clone(),
                diagnostic.line,
                diagnostic.column,
                0,
            ));
        }
    }

    /// Puts buffer `index`'s caret at one-based `line` and `column`.
    fn go_to(&mut self, index: usize, line: usize, column: usize) {
        let area = &mut self.buffers[index].area;
        let line_start = area.line_start_char(line.saturating_sub(1));
        let next_line = area.line_start_char(line);
        let line_end = if next_line > line_start {
            next_line - 1
        } else {
            area.document.len_chars()
        };
        area.set_caret((line_start + column.saturating_sub(1)).min(line_end));
        self.set_focus(Focus::Editor);
    }

    /// Where the active file's diagnostics show on screen.
    fn update_marks(&mut self) {
        self.marks.clear();
        let Some(index) = self.active else {
            return;
        };
        if !self.buffers[index].loaded {
            return;
        }
        let path = self.buffers[index].path.clone();
        let diagnostics: Vec<Diagnostic> = self
            .shown_diagnostics()
            .into_iter()
            .filter(|d| d.file == path)
            .cloned()
            .collect();
        let area = &mut self.buffers[index].area;
        let content = area.layout_cache.content_rect;
        for diagnostic in &diagnostics {
            let line_start = area.line_start_char(diagnostic.line.saturating_sub(1));
            let Some(line) = area
                .layout_cache
                .lines
                .iter()
                .find(|line| line.source_start == line_start)
            else {
                continue;
            };
            let bar = Rect {
                x: area.bounds.x + 1.0,
                y: line.rect.y,
                width: 3.0,
                height: line.rect.height,
            };
            let chars = line.char_offsets.len().saturating_sub(1);
            let mut start = diagnostic.column.saturating_sub(1).min(chars);
            let mut end = if diagnostic.end_line == diagnostic.line {
                diagnostic.end_column.saturating_sub(1).min(chars)
            } else {
                chars
            };
            // A point (rust-analyzer's syntax errors) still gets one
            // character underlined: the one at it, or before it at a line's
            // end.
            if end <= start {
                if start == chars {
                    start = start.saturating_sub(1);
                }
                end = (start + 1).min(chars);
            }
            let underline = (end > start).then(|| {
                let x0 = (line.rect.x + line.char_offsets[start] - area.scroll_x).max(content.x);
                let x1 =
                    (line.rect.x + line.char_offsets[end] - area.scroll_x).min(content.right());
                (x0, x1, line.rect.y + line.rect.height - 1.0)
            });
            self.marks.push(Mark {
                bar,
                underline: underline.filter(|(x0, x1, _)| x1 > x0),
                level: diagnostic.level,
            });
        }
    }

    /// The diagnostic on the caret's line in the active file, if any.
    fn diagnostic_at_caret(&self) -> Option<&Diagnostic> {
        let buffer = self.active_buffer().filter(|buffer| buffer.loaded)?;
        let (line, _) = caret_line_column(&buffer.area);
        self.shown_diagnostics()
            .into_iter()
            .find(|d| d.file == buffer.path && d.line == line)
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
        let editing = self.focused_area_ref().is_some() && self.dialog.is_none();
        let in_terminal = self.focus == Focus::Terminal && self.terminal_open;
        let item = |command: Command, title: &str| MenuItem::command(command.menu_command(), title);
        let mut file = vec![
            item(Command::NewPage, "New Page").with_shortcut(Shortcut::primary('n')),
            item(Command::NewFile, "New File in Folder…").enabled(self.tree.is_some()),
            item(Command::NewFolder, "New Folder…").enabled(self.tree.is_some()),
            item(Command::RenameFile, "Rename…").enabled(self.file_target().is_some()),
            item(Command::TrashFile, "Move to Trash").enabled(self.file_target().is_some()),
            MenuItem::Separator,
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
                .enabled(editing || in_terminal),
            item(Command::Paste, "Paste")
                .with_shortcut(Shortcut::primary('v'))
                .enabled(editing || in_terminal),
            item(Command::SelectAll, "Select All")
                .with_shortcut(Shortcut::primary('a'))
                .enabled(editing || in_terminal),
            MenuItem::Separator,
            item(Command::Find, "Find…")
                .with_shortcut(Shortcut::primary('f'))
                .enabled(has_file),
            item(Command::GoToLine, "Go to Line…").enabled(has_file),
            item(Command::FindInFiles, "Find in Files…")
                .with_shortcut(Shortcut::primary_shift('f'))
                .enabled(self.tree.is_some()),
        ];
        MenuBar {
            menus: vec![
                Menu::new("File", file),
                Menu::new("Edit", edit),
                Menu::new(
                    "View",
                    vec![
                        item(
                            Command::ToggleSplit,
                            if self.split.is_some() {
                                "Unsplit Editor"
                            } else {
                                "Split Editor"
                            },
                        )
                        .enabled(self.active.is_some()),
                        MenuItem::Separator,
                        item(
                            Command::ToggleTerminal,
                            if self.terminal_open {
                                "Hide Terminal"
                            } else {
                                "Show Terminal"
                            },
                        ),
                        item(Command::NewTerminal, "New Terminal"),
                    ],
                ),
                Menu::new(
                    "Navigate",
                    vec![
                        item(Command::GoToDefinition, "Go to Definition")
                            .enabled(has_file && self.lsp.as_ref().is_some_and(|lsp| lsp.ready)),
                        item(Command::RenameSymbol, "Rename Symbol…")
                            .enabled(has_file && self.lsp.as_ref().is_some_and(|lsp| lsp.ready)),
                        item(Command::GoBack, "Back")
                            .with_shortcut(Shortcut::primary('['))
                            .enabled(!self.back_stack.is_empty()),
                    ],
                ),
                Menu::new(
                    "Build",
                    vec![
                        item(Command::Check, "Check (cargo check)")
                            .enabled(self.tree.is_some() && self.check_running.is_none()),
                        item(Command::ToggleProblems, "Show or Hide Problems"),
                    ],
                ),
            ],
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

    /// Input while a file dialog (Open Folder, or saving a page) is up: all
    /// of it goes to the dialog.
    fn route_dialog(&mut self, input: &InputSnapshot, now: Instant) {
        let Some(dialog) = &mut self.dialog else {
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
        let Some(outcome) = outcome else {
            return;
        };
        self.dialog = None;
        self.dialog_animating = false;
        match (self.dialog_for, outcome) {
            (DialogFor::OpenFolder, FileDialogOutcome::Confirmed(folder)) => {
                self.set_status(format!("Opened {}.", folder.display()), StatusKind::Info);
                self.open_folder(folder);
            }
            (DialogFor::SaveAs(id), FileDialogOutcome::Confirmed(path)) => {
                self.save_page_as(id, path);
                self.next_in_save_queue();
            }
            (DialogFor::SaveAs(id), FileDialogOutcome::Cancelled) => {
                // Not saving one page stops what was waiting on it: the rest
                // of Save All, a quit, closing its tab.
                self.save_queue.clear();
                self.quit_after_save = false;
                if let Some(index) = self.buffer_index(id) {
                    self.buffers[index].close_after_save = false;
                    let title = self.buffers[index].title.clone();
                    self.set_status(format!("{title} was not saved."), StatusKind::Info);
                }
            }
            (DialogFor::OpenFolder, FileDialogOutcome::Cancelled) => {}
        }
    }

    /// Asks where to save the page `id`, starting in the folder the last page
    /// was saved to, else the open folder, else home.
    fn show_save_dialog(&mut self, id: u64) {
        let Some(index) = self.buffer_index(id) else {
            return;
        };
        self.activate(index);
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let start = self
            .last_save_dir
            .clone()
            .or_else(|| self.tree.as_ref().map(|tree| tree.root().to_path_buf()))
            .or_else(|| home.clone())
            .unwrap_or_else(|| PathBuf::from("/"));
        let title = format!("Save {}", self.buffers[index].title);
        let suggested = self.buffers[index].title.clone();
        let mut dialog =
            FileDialogModel::save(title, start, &suggested, None, Box::new(StdDirectorySource));
        let mut places = Vec::new();
        if let Some(home) = home {
            places.push(FileDialogPlace::new("Home", home));
        }
        if let Some(tree) = &self.tree {
            places.push(FileDialogPlace::new("This folder", tree.root()));
        }
        dialog.set_places(places);
        self.dialog = Some(dialog);
        self.dialog_for = DialogFor::SaveAs(id);
        self.dialog_animating = false;
    }

    /// The page `id` becomes the file at `path` (the dialog has already
    /// confirmed replacing one that exists) and is written there.
    fn save_page_as(&mut self, id: u64, path: PathBuf) {
        let Some(index) = self.buffer_index(id) else {
            return;
        };
        if self.buffers.iter().any(|buffer| buffer.path == path) {
            self.save_queue.clear();
            self.quit_after_save = false;
            self.buffers[index].close_after_save = false;
            self.set_status(
                format!(
                    "{} is open in another tab; close it first, or save under another name.",
                    path.display()
                ),
                StatusKind::Warning,
            );
            return;
        }
        // The page's backup was kept under its old name.
        if self.buffers[index].has_backup_file {
            if let Some(state_dir) = &self.state_dir {
                self.outbox.push(IoRequest::RemoveState {
                    path: session::backup_path(state_dir, &self.buffers[index].path),
                });
            }
            self.buffers[index].has_backup_file = false;
            self.buffers[index].backup_revision = None;
            self.backup_due = Some(self.frame_time + BACKUP_DELAY);
        }
        self.last_save_dir = path.parent().map(Path::to_path_buf);
        let buffer = &mut self.buffers[index];
        buffer.title = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        buffer.highlighter = Highlighter::for_path(&path);
        buffer.untitled = None;
        buffer.newly_named = true;
        buffer.disk = None;
        buffer.path = path.clone();
        if let Some(tree) = &mut self.tree {
            tree.selected = Some(path);
        }
        self.touch_session();
        self.save(index);
    }

    /// Opens the save dialog for the next page waiting in the queue.
    fn next_in_save_queue(&mut self) {
        while self.dialog.is_none() && !self.save_queue.is_empty() {
            let id = self.save_queue.remove(0);
            if self
                .buffer_index(id)
                .is_some_and(|index| self.buffers[index].untitled.is_some())
            {
                self.show_save_dialog(id);
            }
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
        self.dialog = Some(dialog);
        self.dialog_for = DialogFor::OpenFolder;
        self.dialog_animating = false;
    }

    /// Quits now: the session is written at once, and the app stops once
    /// the file work in flight completes.
    fn quit_now(&mut self) {
        if let Some(mut lsp) = self.lsp.take() {
            lsp.shutdown();
            self.lsp_stop = true;
        }
        self.quit_prompt = false;
        self.quit_after_save = false;
        self.quitting = true;
        if self.state_dir.is_some() {
            self.session_due = Some(self.frame_time);
            self.run_due(self.frame_time);
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
            .filter(|buffer| buffer.loaded && buffer.untitled.is_none())
            .map(|buffer| buffer.path.clone())
            .collect();
        if !paths.is_empty() {
            self.outbox.push(IoRequest::StatFiles { paths });
        }
    }

    fn touch_session(&mut self) {
        if self.state_dir.is_some() && !self.awaiting_session {
            self.session_due
                .get_or_insert(self.frame_time + SESSION_DELAY);
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
                    untitled: buffer.untitled,
                })
                .collect(),
            active: self.active,
            expanded: tree.expanded_dirs(),
            last_save_dir: self.last_save_dir.clone(),
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
        if let Some(dialog) = &mut self.dialog {
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
            (Command::NewPage, "New Page"),
            (Command::NewFile, "New File"),
            (Command::Save, "Save"),
            (Command::SaveAll, "Save All"),
            (Command::Find, "Find"),
            (Command::GoToLine, "Go to Line"),
            (Command::Check, "Check"),
            (Command::ToggleTerminal, "Terminal"),
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
        let mut bottom = layout.status.y;
        if self.terminal_open {
            let height = ((bottom - y) * 0.42).max(160.0).min((bottom - y) * 0.8);
            bottom -= height;
            let panel = Rect {
                x: right_x,
                y: bottom,
                width: right_width,
                height,
            };
            layout.terminal = Some(panel);
            self.terminal
                .layout(panel, &|text: &str, size: u16| host.measure(text, size));
        } else if self.problems_open || self.search_open {
            let height = if self.search_open {
                (PROBLEMS_HEIGHT + 60.0).min((bottom - y) * 0.6)
            } else {
                PROBLEMS_HEIGHT.min((bottom - y) * 0.6)
            };
            bottom -= height;
            let panel = Rect {
                x: right_x,
                y: bottom,
                width: right_width,
                height,
            };
            if self.search_open {
                layout.search = Some(panel);
                let field = Rect {
                    x: panel.x + 120.0,
                    y: panel.y + 5.0,
                    width: (panel.width - 260.0).clamp(120.0, 420.0),
                    height: SEARCH_HEADER_HEIGHT - 10.0,
                };
                layout.search_field = Some(field);
                layout.search_button = Some(Rect {
                    x: field.right() + 8.0,
                    y: field.y,
                    width: host.measure("Search", theme::UI_FONT) + 24.0,
                    height: field.height,
                });
            } else {
                layout.problems = Some(panel);
            }
        }
        layout.editor = Rect {
            x: right_x,
            y,
            width: right_width,
            height: (bottom - y).max(0.0),
        };
        let lsp_diagnostics = self
            .lsp
            .as_ref()
            .is_some_and(|lsp| lsp.all_diagnostics().next().is_some());
        if self.check_running.is_some() || self.check_summary.is_some() || lsp_diagnostics {
            layout.status_check = Some(Rect {
                x: layout.status.right() - 280.0 - 200.0,
                y: layout.status.y,
                width: 200.0,
                height: layout.status.height,
            });
        }
        if let Some(tree) = &mut self.tree {
            tree.bounds = layout.tree;
        }
        if let Some(split) = self.split {
            let whole = layout.editor;
            let half = ((whole.width - 1.0) / 2.0).floor();
            let left = Rect {
                width: half,
                ..whole
            };
            let right = Rect {
                x: whole.x + half + 1.0,
                width: whole.width - half - 1.0,
                ..whole
            };
            let (focused, other) = if split.focused_right {
                (right, left)
            } else {
                (left, right)
            };
            layout.editor = focused;
            layout.other_pane = Some(other);
            if let Some(index) = self.buffer_index(split.other) {
                if Some(index) != self.active {
                    let area = &mut self.buffers[index].area;
                    area.set_bounds(other);
                    area.relayout(|text: &str, size: u16| host.measure(text, size));
                }
            }
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
        if let (Some(search), Some(field)) = (&mut self.search, layout.search_field) {
            search.field.set_bounds(field);
            search.field.relayout(measure);
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
        let commands: Vec<(Command, &str)> = if self.trash_prompt.is_some() {
            vec![
                (Command::TrashConfirm, "Move to Trash"),
                (Command::TrashCancel, "Cancel"),
            ]
        } else if self.quit_prompt {
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
            vec![
                (Command::GotoGo, self.bar_mode.action()),
                (Command::CloseBar, "Close"),
            ]
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
            && self.trash_prompt.is_none()
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
        if self.route_item_drag(input, point) {
            return;
        }
        let state = PointerState::mouse(point, input.modifiers);
        if moved || input.mouse_pressed || input.mouse_wheel_y != 0.0 {
            self.track_hover(point, input.mouse_down || input.mouse_pressed);
        }
        if moved || input.mouse_down {
            self.terminal
                .moved(point, input.mouse_down, input.modifiers);
            if let Some(tree) = &mut self.tree {
                tree.pointer_moved(point);
            }
            let editor_rect = self.layout.editor;
            if let Some(buffer) = self.active_buffer_mut() {
                let area = &mut buffer.area;
                if editor_rect.contains(point)
                    || area.drag_selecting
                    || area.text_drag.is_some()
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
            self.pointer_pressed(point, input.mouse_button, state, host);
        }
        if input.mouse_released {
            let released = UiEvent::PointerReleased {
                button: PointerButton::Primary,
                state,
            };
            self.drop_text_in_other_pane(point);
            self.terminal.released(point, input.modifiers);
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

    // ----- drag and drop -----

    /// Files dropped from the file manager: onto the tree they are copied
    /// into the folder under the pointer; anywhere else a folder opens as
    /// the workspace and files open in tabs.
    fn route_file_drops(&mut self, input: &InputSnapshot) {
        let point = Point {
            x: input.mouse_x,
            y: input.mouse_y,
        };
        self.file_drag_over = input.file_drag_over;
        let over_tree = self.layout.tree.contains(point);
        if let Some(tree) = &mut self.tree {
            tree.drop_target = (input.file_drag_over && over_tree).then(|| tree.drop_dir_at(point));
        }
        if input.dropped_files.is_empty() {
            return;
        }
        if over_tree {
            let Some(dir) = self.tree.as_mut().map(|tree| tree.drop_dir_at(point)) else {
                return;
            };
            for from in &input.dropped_files {
                let Some(name) = from.file_name() else {
                    continue;
                };
                self.outbox.push(IoRequest::FileOp(FileOp::Copy {
                    from: from.clone(),
                    to: dir.join(name),
                }));
            }
            return;
        }
        let mut folder = None;
        for path in &input.dropped_files {
            if path.is_dir() {
                folder.get_or_insert_with(|| path.clone());
            } else {
                self.open_file(path.clone());
            }
        }
        if let Some(folder) = folder {
            self.open_folder(folder);
        }
    }

    /// Tree entries and tabs being pressed or dragged; true when the
    /// pointer input went to the drag.
    fn route_item_drag(&mut self, input: &InputSnapshot, point: Point) -> bool {
        let Some(drag) = &mut self.item_drag else {
            return false;
        };
        if !drag.moving && input.mouse_down {
            let dx = point.x - drag.origin.x;
            let dy = point.y - drag.origin.y;
            drag.moving = dx * dx + dy * dy >= DRAG_THRESHOLD * DRAG_THRESHOLD;
        }
        let moving = drag.moving;
        let item = drag.item.clone();
        if moving {
            let over_tree = self.layout.tree.contains(point);
            if let Some(tree) = &mut self.tree {
                tree.drop_target = (matches!(item, DragItem::Entry(_)) && over_tree)
                    .then(|| tree.drop_dir_at(point));
            }
        }
        if !input.mouse_released {
            return moving;
        }
        self.item_drag = None;
        if let Some(tree) = &mut self.tree {
            tree.drop_target = None;
        }
        match (item, moving) {
            (DragItem::Entry(path), true) => self.drop_entry(path, point),
            (DragItem::Entry(_), false) => {
                let action = self.tree.as_mut().and_then(|tree| tree.click(point));
                match action {
                    Some(TreeAction::Open(path)) => self.open_file(path),
                    Some(TreeAction::List(dir)) => self.outbox.push(IoRequest::ListDir { dir }),
                    None => {}
                }
                self.touch_session();
            }
            (DragItem::Tab(id), true) => {
                if let Some(target) = self.tab_drop_at(point) {
                    self.drop_tab(id, target);
                }
            }
            (DragItem::Tab(_), false) => {}
        }
        true
    }

    /// A tree entry dropped at `point`: into a folder of the tree it moves
    /// (open tabs follow); onto the editor a file opens.
    fn drop_entry(&mut self, from: PathBuf, point: Point) {
        if self.layout.tree.contains(point) {
            let Some(dir) = self.tree.as_mut().map(|tree| tree.drop_dir_at(point)) else {
                return;
            };
            let Some(name) = from.file_name() else {
                return;
            };
            if from.parent() == Some(dir.as_path()) {
                return;
            }
            if dir.starts_with(&from) {
                self.set_status("A folder cannot go inside itself.", StatusKind::Error);
                return;
            }
            let to = dir.join(name);
            self.outbox
                .push(IoRequest::FileOp(FileOp::Rename { from, to }));
            return;
        }
        let over_editor = self.layout.editor.contains(point)
            || self
                .layout
                .other_pane
                .is_some_and(|pane| pane.contains(point));
        if over_editor && !self.tree_is_dir(&from) {
            self.open_file(from);
        }
    }

    /// Where a tab dropped at `point` would go, if anywhere.
    fn tab_drop_at(&self, point: Point) -> Option<TabDrop> {
        if self.layout.tabs.contains(point) {
            let index = self
                .layout
                .tab_slots
                .iter()
                .position(|slot| point.x < slot.rect.x + slot.rect.width / 2.0)
                .unwrap_or(self.layout.tab_slots.len());
            return Some(TabDrop::Row(index));
        }
        if self
            .layout
            .other_pane
            .is_some_and(|pane| pane.contains(point))
        {
            return Some(TabDrop::Pane { other: true });
        }
        let editor = self.layout.editor;
        if !editor.contains(point) {
            return None;
        }
        if self.split.is_some() {
            return Some(TabDrop::Pane { other: false });
        }
        (point.x > editor.x + editor.width / 2.0 && self.buffers.len() > 1)
            .then_some(TabDrop::SplitRight)
    }

    fn drop_tab(&mut self, id: u64, target: TabDrop) {
        let Some(index) = self.buffer_index(id) else {
            return;
        };
        match target {
            TabDrop::Row(before) => {
                let active = self.active_buffer().map(|buffer| buffer.id);
                let buffer = self.buffers.remove(index);
                let before = if before > index { before - 1 } else { before };
                self.buffers.insert(before.min(self.buffers.len()), buffer);
                self.active = active.and_then(|id| self.buffer_index(id));
                self.touch_session();
            }
            TabDrop::Pane { other: false } => {
                let Some(split) = self.split else {
                    return;
                };
                let current = self.active_buffer().map(|buffer| buffer.id);
                self.activate(index);
                if split.other == id {
                    // It came from the other pane, which takes this pane's.
                    if let Some(current) = current {
                        self.split = Some(Split {
                            other: current,
                            ..split
                        });
                    }
                }
                self.repair_split();
            }
            TabDrop::Pane { other: true } => {
                let Some(split) = self.split else {
                    return;
                };
                if self.active_buffer().map(|buffer| buffer.id) == Some(id) {
                    // It leaves the focused pane, which takes the other's.
                    if let Some(other) = self.buffer_index(split.other) {
                        self.activate(other);
                    }
                }
                self.split = Some(Split { other: id, ..split });
                self.repair_split();
            }
            TabDrop::SplitRight => {
                if self.active_buffer().map(|buffer| buffer.id) == Some(id) {
                    let replacement = self
                        .previous_active
                        .filter(|other| *other != id)
                        .and_then(|other| self.buffer_index(other))
                        .or_else(|| self.buffers.iter().position(|buffer| buffer.id != id));
                    let Some(replacement) = replacement else {
                        return;
                    };
                    self.activate(replacement);
                }
                self.split = Some(Split {
                    other: id,
                    focused_right: false,
                });
                self.repair_split();
            }
        }
    }

    /// Selected text dragged from the focused pane and released over the
    /// other pane lands there: moved, or copied with Option held.
    fn drop_text_in_other_pane(&mut self, point: Point) {
        if !self
            .layout
            .other_pane
            .is_some_and(|pane| pane.contains(point))
        {
            return;
        }
        let Some(other) = self.split.and_then(|split| self.buffer_index(split.other)) else {
            return;
        };
        let Some(active) = self.active.filter(|active| *active != other) else {
            return;
        };
        let source = &mut self.buffers[active].area;
        let Some(drag) = source.text_drag.filter(|drag| drag.moving) else {
            return;
        };
        source.text_drag = None;
        let Some((start, end)) = source.selection_range() else {
            return;
        };
        let text = source
            .text()
            .chars()
            .skip(start)
            .take(end - start)
            .collect::<String>();
        if !self.buffers[other].loaded {
            return;
        }
        let target = &mut self.buffers[other].area;
        let at = target.char_at_point(point).unwrap_or(target.caret());
        target.replace_range(at, at, &text);
        target.select_range(at, at + (end - start));
        if !drag.copy {
            self.buffers[active].area.replace_range(start, end, "");
        }
        self.switch_pane();
        self.set_focus(Focus::Editor);
    }

    fn paint_drags(&self, scene: &mut Vec<PaintOp>, layout: &Layout) {
        if self.file_drag_over {
            let over_editor = layout.editor.contains(self.pointer)
                || layout
                    .other_pane
                    .is_some_and(|pane| pane.contains(self.pointer));
            if over_editor {
                let whole = layout.other_pane.map_or(layout.editor, |pane| {
                    let x = layout.editor.x.min(pane.x);
                    Rect {
                        x,
                        width: layout.editor.right().max(pane.right()) - x,
                        ..layout.editor
                    }
                });
                draw::outline(scene, whole, theme::ACCENT);
            }
        }
        let Some(drag) = self.item_drag.as_ref().filter(|drag| drag.moving) else {
            return;
        };
        let name = match &drag.item {
            DragItem::Entry(path) => path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            DragItem::Tab(id) => self
                .buffer_index(*id)
                .map(|index| self.buffers[index].title.clone())
                .unwrap_or_default(),
        };
        if let DragItem::Tab(_) = drag.item {
            match self.tab_drop_at(self.pointer) {
                Some(TabDrop::Row(before)) => {
                    let x = layout.tab_slots.get(before).map_or_else(
                        || {
                            layout
                                .tab_slots
                                .last()
                                .map_or(layout.tabs.x, |slot| slot.rect.right())
                        },
                        |slot| slot.rect.x,
                    );
                    draw::fill(
                        scene,
                        Rect {
                            x: x - 1.0,
                            y: layout.tabs.y,
                            width: 2.0,
                            height: theme::TAB_HEIGHT,
                        },
                        theme::ACCENT,
                    );
                }
                Some(TabDrop::Pane { other }) => {
                    let pane = if other {
                        layout.other_pane.unwrap_or(layout.editor)
                    } else {
                        layout.editor
                    };
                    draw::outline(scene, pane, theme::ACCENT);
                }
                Some(TabDrop::SplitRight) => {
                    let editor = layout.editor;
                    let half = Rect {
                        x: editor.x + editor.width / 2.0,
                        width: editor.width / 2.0,
                        ..editor
                    };
                    draw::fill(scene, half, theme::SELECTION_FILL);
                    draw::outline(scene, half, theme::ACCENT);
                }
                None => {}
            }
        }
        let ghost = Rect {
            x: self.pointer.x + 12.0,
            y: self.pointer.y + 8.0,
            width: 220.0,
            height: 24.0,
        };
        draw::fill(scene, ghost, theme::BUTTON_HOVER);
        draw::outline(scene, ghost, theme::BORDER);
        draw::label(
            scene,
            &name,
            Rect {
                x: ghost.x + 8.0,
                width: ghost.width - 16.0,
                ..ghost
            },
            theme::TEXT,
            theme::UI_FONT,
            HorizontalAlign::Left,
        );
    }

    fn bar_fields(&mut self) -> Vec<&mut TextFieldModel> {
        let mut fields = Vec::new();
        if let Some(find) = &mut self.find {
            fields.push(&mut find.field);
        }
        if let Some(goto) = &mut self.goto {
            fields.push(goto);
        }
        if let Some(search) = &mut self.search {
            fields.push(&mut search.field);
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

    fn pointer_pressed(
        &mut self,
        point: Point,
        button: PointerButton,
        state: PointerState,
        host: &dyn EditorHost,
    ) {
        if let Some(popup) = &self.completion {
            let row = popup
                .rows
                .iter()
                .find(|(_, rect)| rect.contains(point))
                .map(|(item, _)| *item);
            match row {
                Some(item) => {
                    if let Some(popup) = &mut self.completion {
                        popup.selected = item;
                    }
                    self.accept_completion();
                    return;
                }
                None => self.completion = None,
            }
        }
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
        if self
            .layout
            .status_check
            .is_some_and(|rect| rect.contains(point))
        {
            self.problems_open = !self.problems_open;
            self.search_open = false;
            return;
        }
        if self
            .layout
            .terminal
            .is_some_and(|rect| rect.contains(point))
        {
            let root = self.tree.as_ref().map(|tree| tree.root().to_path_buf());
            let fallback = self.fallback_root.clone();
            let cwd = move || {
                (
                    root.clone().unwrap_or_else(|| fallback.clone()),
                    shell_name(),
                )
            };
            self.terminal.pressed(point, button, state.modifiers, &cwd);
            self.set_focus(Focus::Terminal);
            return;
        }
        if let Some(panel) = self.layout.search.filter(|rect| rect.contains(point)) {
            self.search_panel_pressed(point, panel, state, host);
            return;
        }
        if let Some(panel) = self.layout.problems.filter(|rect| rect.contains(point)) {
            let row = ((point.y - panel.y - PROBLEM_ROW_HEIGHT) / PROBLEM_ROW_HEIGHT).floor();
            if row >= 0.0 {
                let index = self.problems_scroll + row as usize;
                if let Some(diagnostic) = self.shown_diagnostics().get(index).copied().cloned() {
                    self.go_to_diagnostic(&diagnostic);
                }
            }
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
                self.item_drag = Some(ItemDrag {
                    item: DragItem::Tab(self.buffers[index].id),
                    origin: point,
                    moving: false,
                });
            }
            return;
        }
        if self.layout.tree.contains(point) {
            // The click acts on release, so a folder dragged elsewhere does
            // not also open or close.
            if let Some(row) = self.tree.as_mut().and_then(|tree| tree.entry_at(point)) {
                self.item_drag = Some(ItemDrag {
                    item: DragItem::Entry(row.path),
                    origin: point,
                    moving: false,
                });
            }
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
        if self
            .layout
            .other_pane
            .is_some_and(|pane| pane.contains(point))
        {
            self.switch_pane();
            // The press then lands in the pane now focused.
            self.layout.editor = self.layout.other_pane.take().unwrap_or(self.layout.editor);
        }
        if self.layout.editor.contains(point)
            && (state.modifiers.meta || state.modifiers.ctrl)
            && self.definition_at_point(point)
        {
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
        if let Some(search) = &mut self.search {
            let _ = search
                .field
                .handle_event(UiEvent::FocusChanged(focus == Focus::Search));
        }
    }

    fn wheel(&mut self, point: Point, input: &InputSnapshot) {
        if self
            .layout
            .terminal
            .is_some_and(|panel| panel.contains(point))
        {
            self.terminal.wheel(
                point,
                input.mouse_wheel_y,
                input.mouse_wheel_precise,
                input.modifiers,
            );
            return;
        }
        if self
            .layout
            .search
            .is_some_and(|panel| panel.contains(point))
        {
            if let Some(search) = &mut self.search {
                let rows = if input.mouse_wheel_precise {
                    (-input.mouse_wheel_y / PROBLEM_ROW_HEIGHT).round() as isize
                } else {
                    (-input.mouse_wheel_y * 3.0).round() as isize
                };
                let last = search.results.len().saturating_sub(1);
                search.scroll = (search.scroll as isize + rows).clamp(0, last as isize) as usize;
            }
            return;
        }
        if self
            .layout
            .problems
            .is_some_and(|panel| panel.contains(point))
        {
            let rows = if input.mouse_wheel_precise {
                (-input.mouse_wheel_y / PROBLEM_ROW_HEIGHT).round() as isize
            } else {
                (-input.mouse_wheel_y * 3.0).round() as isize
            };
            let last = self.shown_diagnostics().len().saturating_sub(1);
            self.problems_scroll =
                (self.problems_scroll as isize + rows).clamp(0, last as isize) as usize;
            return;
        }
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
        } else if let Some(index) = self
            .layout
            .other_pane
            .filter(|pane| pane.contains(point))
            .and(self.split)
            .and_then(|split| self.buffer_index(split.other))
        {
            self.buffers[index].area.scroll_vertical(pixels_y);
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
        if self.focus == Focus::Terminal {
            // Ctrl and a letter is a control character for the program, not
            // a drawn menu's shortcut; Ctrl-Shift-C and -V copy and paste.
            if modifiers.ctrl && modifiers.shift && !modifiers.meta {
                match key {
                    loadngo_host_core::HostKey::C => return self.run(Command::Copy, host),
                    loadngo_host_core::HostKey::V => return self.run(Command::Paste, host),
                    _ => {}
                }
            }
            if self.terminal.key(key, modifiers) || (modifiers.ctrl && !modifiers.meta) {
                return;
            }
        }
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
        if self.focus == Focus::Editor && self.completion.is_some() && self.completion_key(key) {
            return;
        }
        if key == Key::Escape {
            if self.trash_prompt.take().is_some() {
            } else if self.quit_prompt {
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
                if key == Key::Backspace {
                    self.refresh_completion();
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
            Focus::Search => {
                if key == Key::Enter {
                    self.run(Command::RunSearch, host);
                } else if let Some(search) = &mut self.search {
                    let _ = search.field.handle_event(event);
                }
            }
            Focus::Terminal => {}
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
            Focus::Search => self.search.as_ref().map(|search| &search.field.area),
            Focus::Terminal => None,
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
            Focus::Search => self.search.as_mut().map(|search| &mut search.field.area),
            Focus::Terminal => None,
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
            Focus::Find | Focus::Goto | Focus::Search => self.route_text(&text),
            Focus::Terminal => self.terminal.paste(&text),
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
                self.after_typing(text);
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
            Focus::Search => {
                if let Some(search) = &mut self.search {
                    let _ = search.field.handle_event(event);
                }
            }
            Focus::Terminal => self.terminal.text(text),
        }
    }

    fn run(&mut self, command: Command, host: &dyn EditorHost) {
        match command {
            Command::OpenFolder => self.show_folder_dialog(),
            Command::NewPage => self.new_page(),
            Command::GoToDefinition => {
                if let Some(buffer) = self.active_buffer() {
                    let caret = buffer.area.caret();
                    self.request_definition(caret);
                }
            }
            Command::GoBack => {
                if let Some((path, caret)) = self.back_stack.pop() {
                    self.open_file(path);
                    if let Some(buffer) = self.active_buffer_mut() {
                        if buffer.loaded {
                            buffer.area.set_caret(caret);
                        } else {
                            buffer.restore_caret = Some(caret);
                        }
                    }
                }
            }
            Command::Check => {
                let file = self
                    .active_buffer()
                    .filter(|buffer| buffer.untitled.is_none())
                    .map(|buffer| buffer.path.clone())
                    .or_else(|| {
                        self.tree
                            .as_ref()
                            .map(|tree| tree.root().join("Cargo.toml"))
                    });
                if let Some(file) = file {
                    self.request_check(file);
                }
            }
            Command::ToggleProblems => {
                self.problems_open = !self.problems_open;
                self.search_open = false;
                self.terminal_open = false;
                self.problems_scroll = 0;
            }
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
            Command::SelectAll if self.focus == Focus::Terminal => self.terminal.select_all(),
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
            Command::Cut | Command::Copy if self.focus == Focus::Terminal => {
                if let Some(text) = self.terminal.copy() {
                    host.write_clipboard(&text);
                }
            }
            Command::ToggleTerminal => {
                self.terminal_open = !self.terminal_open;
                if self.terminal_open {
                    self.problems_open = false;
                    self.search_open = false;
                    if self.terminal.is_empty() {
                        self.new_terminal();
                    }
                    self.set_focus(Focus::Terminal);
                } else if self.focus == Focus::Terminal {
                    self.set_focus(Focus::Editor);
                }
            }
            Command::NewTerminal => {
                self.terminal_open = true;
                self.problems_open = false;
                self.search_open = false;
                self.new_terminal();
                self.set_focus(Focus::Terminal);
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
                self.bar_mode = BarMode::GoToLine;
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
            Command::GotoGo => match self.bar_mode.clone() {
                BarMode::GoToLine => self.go_to_line(),
                BarMode::Rename { at } => self.request_rename(at),
                BarMode::NewFile { dir } => {
                    self.finish_file_op(|name| FileOp::CreateFile(dir.join(name)))
                }
                BarMode::NewFolder { dir } => {
                    self.finish_file_op(|name| FileOp::CreateFolder(dir.join(name)))
                }
                BarMode::RenameFile { from } => self.finish_file_op(|name| FileOp::Rename {
                    to: from
                        .parent()
                        .map_or_else(|| PathBuf::from(name), |dir| dir.join(name)),
                    from: from.clone(),
                }),
            },
            Command::FindInFiles => {
                let seed = self.active_buffer().and_then(|buffer| {
                    let (start, end) = buffer.area.selection_range()?;
                    let text = buffer.area.document.slice_chars(start, end);
                    (!text.contains('\n')).then_some(text)
                });
                let search = self.search.get_or_insert_with(|| SearchPanel {
                    field: TextFieldModel::new("", Rect::default()),
                    results: Vec::new(),
                    running: None,
                    summary: None,
                    scroll: 0,
                });
                style_field(&mut search.field);
                if let Some(seed) = seed {
                    search.field.set_text(&seed);
                }
                search.field.select_all();
                self.search_open = true;
                self.terminal_open = false;
                self.problems_open = false;
                self.set_focus(Focus::Search);
            }
            Command::RunSearch => self.run_search(),
            Command::ToggleSplit => {
                if self.split.take().is_none() {
                    if let Some(active) = self.active_buffer().map(|buffer| buffer.id) {
                        let other = self
                            .previous_active
                            .filter(|id| *id != active && self.buffer_index(*id).is_some())
                            .unwrap_or(active);
                        self.split = Some(Split {
                            other,
                            focused_right: false,
                        });
                    }
                }
            }
            Command::NewFile => self.start_file_bar(true),
            Command::NewFolder => self.start_file_bar(false),
            Command::RenameFile => self.start_rename_file(),
            Command::TrashFile => {
                if let Some(path) = self.file_target() {
                    self.trash_prompt = Some(path);
                }
            }
            Command::TrashConfirm => {
                if let Some(path) = self.trash_prompt.take() {
                    self.outbox.push(IoRequest::FileOp(FileOp::Trash(path)));
                }
            }
            Command::TrashCancel => self.trash_prompt = None,
            Command::RenameSymbol => self.start_rename(),
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
        if let (Some(pane), Some(index)) = (
            layout.other_pane,
            self.split.and_then(|split| self.buffer_index(split.other)),
        ) {
            let buffer = &self.buffers[index];
            if Some(index) == self.active {
                // The same buffer in both panes: one view, drawn once.
                self.paint_hint(
                    scene,
                    pane,
                    &format!("{} is in the other pane.", buffer.title),
                );
            } else if buffer.loaded {
                match &buffer.highlighter {
                    Some(highlighter) => buffer
                        .area
                        .paint_with_runs(scene, &|line| highlighter.runs(line)),
                    None => buffer.area.paint(scene),
                }
            } else {
                self.paint_hint(scene, pane, "Loading…");
            }
            draw::vline(
                scene,
                layout.editor.x.min(pane.x) + layout.editor.width.min(pane.width) + 0.5,
                pane.y,
                pane.height,
                theme::BORDER,
            );
        }
        match self.active_buffer() {
            Some(buffer) if buffer.loaded => match &buffer.highlighter {
                Some(highlighter) => buffer
                    .area
                    .paint_with_runs(scene, &|line| highlighter.runs(line)),
                None => buffer.area.paint(scene),
            },
            Some(_) => self.paint_hint(scene, layout.editor, "Loading…"),
            None => self.paint_hint(
                scene,
                layout.editor,
                "Open a file from the folder on the left.",
            ),
        }
        self.paint_marks(scene);
        self.paint_hover(scene, layout.editor);
        self.paint_completion(scene, layout.editor);
        if let Some(panel) = layout.problems {
            self.paint_problems(scene, panel);
        }
        if let Some(panel) = layout.search {
            self.paint_search(scene, panel, &layout);
        }
        if layout.terminal.is_some() {
            self.terminal
                .paint(scene, self.focus == Focus::Terminal && self.window_focused);
        }
        self.paint_status(scene, &layout);
        self.paint_drags(scene, &layout);
        if let Some(menu) = &self.drawn_menu {
            menu.paint(scene);
        }
        if let Some(dialog) = &self.dialog {
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

    /// The hover box near the pointer: up to 14 lines, kept inside the
    /// editor.
    fn paint_hover(&self, scene: &mut Vec<PaintOp>, bounds: Rect) {
        let Some((text, at)) = &self.hover else {
            return;
        };
        let lines: Vec<&str> = text.lines().take(14).collect();
        let line_height = 20.0;
        let width = 560.0f32.min(bounds.width - 16.0).max(120.0);
        let height = lines.len() as f32 * line_height + 16.0;
        let x = (at.x + 12.0)
            .min(bounds.right() - width - 8.0)
            .max(bounds.x + 8.0);
        let below = at.y + 18.0;
        let y = if below + height <= bounds.bottom() {
            below
        } else {
            (at.y - height - 6.0).max(bounds.y + 4.0)
        };
        let rect = Rect {
            x,
            y,
            width,
            height,
        };
        draw::fill(scene, rect, theme::PANEL);
        scene.push(PaintOp::StrokeRect {
            rect,
            color: theme::BORDER,
        });
        for (row, line) in lines.iter().enumerate() {
            draw::label(
                scene,
                line,
                Rect {
                    x: x + 10.0,
                    y: y + 8.0 + row as f32 * line_height,
                    width: width - 20.0,
                    height: line_height,
                },
                if row == 0 {
                    theme::TEXT
                } else {
                    theme::TEXT_DIM
                },
                theme::UI_FONT,
                HorizontalAlign::Left,
            );
        }
    }

    fn paint_marks(&self, scene: &mut Vec<PaintOp>) {
        for mark in &self.marks {
            let color = level_color(mark.level);
            draw::fill(scene, mark.bar, color);
            if let Some((x0, x1, y)) = mark.underline {
                draw::hline(scene, x0, y, x1 - x0, color);
                draw::hline(scene, x0, y - 1.0, x1 - x0, color);
            }
        }
    }

    fn paint_problems(&self, scene: &mut Vec<PaintOp>, panel: Rect) {
        draw::fill(scene, panel, theme::PANEL);
        draw::hline(scene, panel.x, panel.y + 0.5, panel.width, theme::BORDER);
        let shown = self.shown_diagnostics();
        let (errors, warnings) = level_counts(&shown);
        let cargo = match (&self.check_summary, self.check_running) {
            (_, Some(_)) => "cargo check running".to_string(),
            (Some(summary), None) => {
                format!("cargo check took {:.1} s", summary.elapsed.as_secs_f32())
            }
            (None, None) => "no cargo check yet".to_string(),
        };
        let header = format!("Problems: {} ({cargo})", count_text(errors, warnings));
        draw::label(
            scene,
            &header,
            Rect {
                x: panel.x + 12.0,
                y: panel.y,
                width: panel.width - 24.0,
                height: PROBLEM_ROW_HEIGHT,
            },
            theme::TEXT_DIM,
            theme::UI_FONT,
            HorizontalAlign::Left,
        );
        let root = self.tree.as_ref().map(|tree| tree.root().to_path_buf());
        let rows = ((panel.height - PROBLEM_ROW_HEIGHT) / PROBLEM_ROW_HEIGHT)
            .floor()
            .max(0.0) as usize;
        for (row, diagnostic) in shown
            .iter()
            .skip(self.problems_scroll)
            .take(rows)
            .enumerate()
        {
            let rect = Rect {
                x: panel.x,
                y: panel.y + PROBLEM_ROW_HEIGHT * (row + 1) as f32,
                width: panel.width,
                height: PROBLEM_ROW_HEIGHT,
            };
            if rect.contains(self.pointer) {
                draw::fill(scene, rect, theme::HOVER);
            }
            draw::fill(
                scene,
                Rect {
                    x: rect.x + 12.0,
                    y: rect.y + 7.0,
                    width: 8.0,
                    height: 8.0,
                },
                level_color(diagnostic.level),
            );
            let shown = root
                .as_ref()
                .and_then(|root| diagnostic.file.strip_prefix(root).ok())
                .unwrap_or(&diagnostic.file);
            let place = format!(
                "{}:{}:{}",
                shown.display(),
                diagnostic.line,
                diagnostic.column
            );
            let place_width = (panel.width * 0.25).min(280.0);
            draw::label(
                scene,
                &place,
                Rect {
                    x: rect.x + 28.0,
                    width: place_width,
                    ..rect
                },
                theme::TEXT_DIM,
                theme::UI_FONT,
                HorizontalAlign::Left,
            );
            draw::label(
                scene,
                &diagnostic_text(diagnostic),
                Rect {
                    x: rect.x + 36.0 + place_width,
                    width: (rect.width - place_width - 48.0).max(0.0),
                    ..rect
                },
                theme::TEXT,
                theme::UI_FONT,
                HorizontalAlign::Left,
            );
        }
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
                Command::NewPage => ("New Page", true),
                Command::NewFile => ("New File", self.tree.is_some()),
                Command::Save => ("Save", active_dirty),
                Command::SaveAll => ("Save All", any_dirty),
                Command::Find => ("Find", self.active.is_some()),
                Command::GoToLine => ("Go to Line", self.active.is_some()),
                Command::Check => (
                    if self.check_running.is_some() {
                        "Checking…"
                    } else {
                        "Check"
                    },
                    self.tree.is_some() && self.check_running.is_none(),
                ),
                Command::ToggleTerminal => ("Terminal", true),
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
        if let Some(path) = &self.trash_prompt {
            let shown = self.shown_path(path);
            let unsaved = self
                .buffers
                .iter()
                .any(|buffer| buffer.path.starts_with(path) && buffer.dirty());
            let text = if unsaved {
                format!("Move {shown} to the Trash? Its unsaved changes will be lost.")
            } else {
                format!("Move {shown} to the Trash?")
            };
            draw::label(
                scene,
                &text,
                message_rect,
                theme::WARNING,
                theme::UI_FONT,
                HorizontalAlign::Left,
            );
        } else if self.quit_prompt {
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
                self.bar_mode.label(),
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
                Command::TrashConfirm => "Move to Trash",
                Command::TrashCancel => "Cancel",
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
                Command::GotoGo => self.bar_mode.action(),
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
        if let Some(check_rect) = layout.status_check {
            let (errors, warnings) = level_counts(&self.shown_diagnostics());
            let (text, color) = if self.check_running.is_some() {
                ("Checking…".to_string(), theme::TEXT_DIM)
            } else {
                (
                    count_text(errors, warnings),
                    if errors > 0 {
                        theme::ERROR
                    } else if warnings > 0 {
                        theme::WARNING
                    } else {
                        theme::TEXT_DIM
                    },
                )
            };
            if check_rect.contains(self.pointer) {
                draw::fill(scene, check_rect, theme::HOVER);
            }
            draw::label(
                scene,
                &text,
                check_rect,
                color,
                theme::UI_FONT,
                HorizontalAlign::Right,
            );
        }
        let server = match (&self.lsp, &self.lsp_failure) {
            (Some(lsp), _) => format!("rust-analyzer: {}", lsp.status),
            (None, Some(_)) => "rust-analyzer: stopped".to_string(),
            (None, None) => String::new(),
        };
        let server_width = 220.0;
        draw::label(
            scene,
            &server,
            Rect {
                x: rect.right() - right_width - 236.0 - server_width,
                y: rect.y,
                width: server_width,
                height: rect.height,
            },
            theme::TEXT_DIM,
            theme::UI_FONT,
            HorizontalAlign::Right,
        );
        let at_caret = self
            .diagnostic_at_caret()
            .map(|d| (diagnostic_text(d), level_color(d.level)));
        let (left_text, color) = match (at_caret, &self.status) {
            (Some(found), _) => found,
            (None, Some((text, kind))) => (
                text.clone(),
                match kind {
                    StatusKind::Info => theme::TEXT_DIM,
                    StatusKind::Warning => theme::WARNING,
                    StatusKind::Error => theme::ERROR,
                },
            ),
            (None, None) => (
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
                width: (rect.width - right_width - 236.0 - server_width).max(0.0),
                height: rect.height,
            },
            color,
            theme::UI_FONT,
            HorizontalAlign::Left,
        );
    }
}

/// The user's shell's name, for a terminal tab.
fn shell_name() -> String {
    std::env::var_os("SHELL")
        .and_then(|shell| {
            std::path::Path::new(&shell)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "sh".to_string())
}

fn level_color(level: Level) -> ui_core::Color {
    match level {
        Level::Error => theme::ERROR,
        Level::Warning => theme::WARNING,
    }
}

fn level_counts(diagnostics: &[&Diagnostic]) -> (usize, usize) {
    let errors = diagnostics
        .iter()
        .filter(|d| d.level == Level::Error)
        .count();
    (errors, diagnostics.len() - errors)
}

fn count_text(errors: usize, warnings: usize) -> String {
    let plural = |n: usize, word: &str| {
        if n == 1 {
            format!("1 {word}")
        } else {
            format!("{n} {word}s")
        }
    };
    match (errors, warnings) {
        (0, 0) => "No problems".to_string(),
        (e, 0) => plural(e, "error"),
        (0, w) => plural(w, "warning"),
        (e, w) => format!("{}, {}", plural(e, "error"), plural(w, "warning")),
    }
}

/// `error[E0308]: mismatched types — expected `u32`, found `&str``.
fn diagnostic_text(diagnostic: &Diagnostic) -> String {
    let level = match diagnostic.level {
        Level::Error => "error",
        Level::Warning => "warning",
    };
    let code = diagnostic
        .code
        .as_ref()
        .map(|code| format!("[{code}]"))
        .unwrap_or_default();
    match &diagnostic.label {
        Some(label) if !label.is_empty() => {
            format!("{level}{code}: {} — {label}", diagnostic.message)
        }
        _ => format!("{level}{code}: {}", diagnostic.message),
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
    use ui_core::{FileDialogMode, MenuBar, MenuItem};

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

        /// What the editor shows with no file open: one empty, unsaved page.
        fn only_a_blank_page(&self) -> bool {
            self.editor.buffers.len() == 1 && self.editor.buffers[0].untouched_page()
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

    fn same_dir(a: &Path, b: &Path) -> bool {
        fs::canonicalize(a).unwrap() == fs::canonicalize(b).unwrap()
    }

    /// Saves the active page through its dialog as `name`.
    fn save_page_as(rig: &mut Rig, name: &str) {
        rig.key(HostKey::S, cmd());
        assert!(rig.editor.dialog.is_some(), "Save asks where");
        rig.type_text(name);
        rig.key(HostKey::Enter, Modifiers::default());
        rig.settle();
    }

    #[test]
    fn a_first_launch_opens_a_blank_page_to_type_in() {
        let dir = workspace(&[("a.rs", "fn a() {}\n")]);
        let mut rig = Rig::new(None, Some(dir.path().to_path_buf()));
        assert!(rig.only_a_blank_page());
        assert_eq!(rig.active().title, "Untitled 1");
        assert_eq!(rig.editor.focus, Focus::Editor);
        rig.type_text("first words");
        assert_eq!(rig.active().area.text(), "first words");
        assert!(rig.active().dirty());
        // Nothing reached the disk, and the folder is as it was.
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn closing_the_last_tab_leaves_a_blank_page() {
        let dir = workspace(&[("a.rs", "abc")]);
        let mut rig = Rig::new(None, Some(dir.path().to_path_buf()));
        rig.editor.open_file(dir.path().join("a.rs"));
        rig.settle();
        assert_eq!(
            rig.editor.buffers.len(),
            1,
            "the file replaced the untouched page"
        );
        rig.key(HostKey::W, cmd());
        assert!(rig.only_a_blank_page());
        // Closing that page brings a fresh one.
        rig.key(HostKey::W, cmd());
        assert!(rig.only_a_blank_page());
    }

    #[test]
    fn opening_a_file_keeps_a_page_that_has_text() {
        let dir = workspace(&[("a.rs", "abc")]);
        let mut rig = Rig::new(None, Some(dir.path().to_path_buf()));
        rig.type_text("keep me");
        rig.editor.open_file(dir.path().join("a.rs"));
        rig.settle();
        assert_eq!(rig.editor.buffers.len(), 2);
        assert_eq!(rig.editor.buffers[0].area.text(), "keep me");
        assert_eq!(rig.active().title, "a.rs");
    }

    #[test]
    fn saving_a_page_asks_where_and_it_becomes_that_file() {
        let dir = workspace(&[("notes/old.txt", "")]);
        let mut rig = Rig::new(None, Some(dir.path().to_path_buf()));
        rig.type_text("fn main() {}\n");
        rig.key(HostKey::S, cmd());
        let dialog = rig.editor.dialog.as_ref().expect("Save asks where");
        assert_eq!(dialog.mode(), FileDialogMode::Save);
        assert!(
            same_dir(dialog.directory(), dir.path()),
            "starts in the open folder"
        );
        assert_eq!(dialog.name_text(), "Untitled 1");
        rig.type_text("main.rs");
        rig.key(HostKey::Enter, Modifiers::default());
        rig.settle();
        assert_eq!(
            fs::read_to_string(dir.path().join("main.rs")).unwrap(),
            "fn main() {}\n"
        );
        let page = rig.active();
        assert_eq!(page.title, "main.rs");
        assert_eq!(page.untitled, None);
        assert!(!page.dirty());
        assert!(
            page.highlighter.is_some(),
            "a .rs file is highlighted once named"
        );
        assert_eq!(rig.status(), "Saved main.rs.");

        // The next page's dialog starts where this one was saved.
        rig.key(HostKey::N, cmd());
        assert_eq!(rig.active().title, "Untitled 1");
        rig.type_text("x");
        rig.key(HostKey::S, cmd());
        let dialog = rig.editor.dialog.as_ref().unwrap();
        assert!(same_dir(dialog.directory(), dir.path()));
        rig.key(HostKey::Escape, Modifiers::default());
        assert!(rig.editor.dialog.is_none());
        assert!(rig.active().dirty(), "cancelled: still an unsaved page");
        assert_eq!(rig.status(), "Untitled 1 was not saved.");
    }

    #[test]
    fn cmd_n_numbers_new_pages() {
        let dir = workspace(&[]);
        let mut rig = Rig::new(None, Some(dir.path().to_path_buf()));
        rig.type_text("one");
        rig.key(HostKey::N, cmd());
        assert_eq!(rig.active().title, "Untitled 2");
        assert_eq!(rig.editor.buffers.len(), 2);
    }

    #[test]
    fn a_name_open_in_another_tab_is_refused() {
        let dir = workspace(&[("a.rs", "abc")]);
        let mut rig = Rig::new(None, Some(dir.path().to_path_buf()));
        rig.type_text("page");
        rig.editor.open_file(dir.path().join("a.rs"));
        rig.settle();
        let page = rig
            .editor
            .buffers
            .iter()
            .position(|b| b.untitled.is_some())
            .unwrap();
        rig.editor.activate(page);
        rig.settle();
        rig.key(HostKey::S, cmd());
        rig.type_text("a.rs");
        rig.key(HostKey::Enter, Modifiers::default());
        // The dialog asks to replace the file on disk first.
        if rig.editor.dialog.is_some() {
            rig.key(HostKey::Enter, Modifiers::default());
        }
        rig.settle();
        assert_eq!(fs::read_to_string(dir.path().join("a.rs")).unwrap(), "abc");
        assert!(
            rig.status().contains("is open in another tab"),
            "{}",
            rig.status()
        );
        assert!(rig
            .editor
            .buffers
            .iter()
            .any(|b| b.untitled.is_some() && b.dirty()));
    }

    #[test]
    fn an_unsaved_page_and_the_save_folder_survive_a_restart() {
        let dir = workspace(&[]);
        let state = tempfile::tempdir().unwrap();
        let mut rig = Rig::new(
            Some(state.path().to_path_buf()),
            Some(dir.path().to_path_buf()),
        );
        fs::create_dir(dir.path().join("out")).unwrap();
        rig.type_text("saved");
        save_page_as(&mut rig, "out/first.txt");
        assert_eq!(
            fs::read_to_string(dir.path().join("out/first.txt")).unwrap(),
            "saved"
        );
        rig.key(HostKey::N, cmd());
        rig.type_text("draft text");
        rig.later(5);

        let mut rig = Rig::new(
            Some(state.path().to_path_buf()),
            Some(dir.path().to_path_buf()),
        );
        let page = rig
            .editor
            .buffers
            .iter()
            .find(|b| b.untitled.is_some())
            .expect("the page came back");
        assert_eq!(page.area.text(), "draft text");
        assert!(page.dirty(), "still unsaved");
        assert_eq!(page.area.caret(), "draft text".chars().count());
        rig.editor.activate(
            rig.editor
                .buffers
                .iter()
                .position(|b| b.untitled.is_some())
                .unwrap(),
        );
        rig.settle();
        rig.key(HostKey::S, cmd());
        let dialog = rig.editor.dialog.as_ref().unwrap();
        assert!(
            same_dir(dialog.directory(), &dir.path().join("out")),
            "the last save folder"
        );
    }

    #[test]
    fn quitting_with_an_unsaved_page_asks_for_its_name_then_quits() {
        let dir = workspace(&[]);
        let mut rig = Rig::new(None, Some(dir.path().to_path_buf()));
        rig.type_text("before quitting");
        rig.editor.run(Command::Quit, &rig.host);
        assert!(rig.editor.quit_prompt);
        rig.editor.run(Command::QuitSaveAll, &rig.host);
        assert!(rig.editor.dialog.is_some(), "the page needs a name first");
        assert!(!rig.editor.quitting);
        rig.type_text("kept.txt");
        rig.key(HostKey::Enter, Modifiers::default());
        rig.settle();
        assert_eq!(
            fs::read_to_string(dir.path().join("kept.txt")).unwrap(),
            "before quitting"
        );
        assert!(rig.editor.quitting);
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
        assert!(rig.only_a_blank_page());
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
        assert!(rig.only_a_blank_page());
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
        assert!(rig.only_a_blank_page());
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
        assert!(rig.editor.dialog.is_some());
        // Typing a path in the dialog and pressing Enter chooses it.
        rig.type_text(&root.join("two").display().to_string());
        rig.key(HostKey::Enter, Modifiers::default());
        assert!(rig.editor.dialog.is_none());
        rig.settle();
        let tree = rig.editor.tree.as_mut().unwrap();
        assert_eq!(tree.root(), root.join("two"));
        let names: Vec<_> = tree.rows().iter().map(|row| row.name.clone()).collect();
        assert_eq!(names, vec!["b.rs"]);
    }

    #[test]
    fn escape_closes_the_dialog() {
        let dir = workspace(&[("a.rs", "a")]);
        let mut rig = Rig::new(None, Some(dir.path().to_path_buf()));
        rig.outcome(menu_input(Command::OpenFolder));
        rig.key(HostKey::Escape, Modifiers::default());
        assert!(rig.editor.dialog.is_none());
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
        // Down to "Save" (past New Page, the four file commands, Open Folder…
        // and Refresh Folder; separators are skipped) and Enter.
        for _ in 0..8 {
            rig.key(HostKey::Down, Modifiers::default());
        }
        rig.key(HostKey::Enter, Modifiers::default());
        assert_eq!(fs::read_to_string(dir.path().join("b.rs")).unwrap(), "yb");
        assert!(!rig.editor.drawn_menu.as_ref().unwrap().is_open());
        assert_eq!(rig.active().area.text(), "yb", "the keys went to the menu");
    }

    fn diagnostic(file: &Path, line: usize, column: usize, level: Level) -> Diagnostic {
        Diagnostic {
            level,
            file: file.to_path_buf(),
            line,
            column,
            end_line: line,
            end_column: column + 3,
            message: "mismatched types".to_string(),
            label: Some("expected `u32`".to_string()),
            code: Some("E0308".to_string()),
        }
    }

    fn checked(diagnostics: Vec<Diagnostic>) -> CheckOutcome {
        CheckOutcome {
            diagnostics,
            success: false,
            cancelled: false,
            failure: None,
            elapsed: Duration::from_millis(1200),
            package_dir: None,
        }
    }

    #[test]
    fn saving_a_rust_file_asks_for_a_check_and_other_files_do_not() {
        let dir = workspace(&[("src/lib.rs", "pub fn a() {}"), ("notes.md", "x")]);
        let mut rig = Rig::new(None, Some(dir.path().to_path_buf()));
        rig.editor.open_file(dir.path().join("notes.md"));
        rig.settle();
        rig.type_text("y");
        rig.key(HostKey::S, cmd());
        assert_eq!(rig.editor.take_check_request(), None);
        rig.editor.open_file(dir.path().join("src/lib.rs"));
        rig.settle();
        rig.type_text("// ");
        rig.key(HostKey::S, cmd());
        let request = rig.editor.take_check_request().expect("a check");
        assert_eq!(request.file, dir.path().join("src/lib.rs"));
        assert!(rig.editor.check_running.is_some());
    }

    #[test]
    fn a_replaced_checks_result_is_ignored_and_the_latest_is_kept() {
        let dir = workspace(&[("src/lib.rs", "pub fn a() {}\n")]);
        let file = dir.path().join("src/lib.rs");
        let mut rig = Rig::new(None, Some(dir.path().to_path_buf()));
        rig.editor.open_file(file.clone());
        rig.settle();
        rig.editor.run(Command::Check, &rig.host);
        let first = rig.editor.take_check_request().unwrap();
        rig.editor.run(Command::Check, &rig.host);
        let second = rig.editor.take_check_request().unwrap();
        rig.editor.apply_check(
            first.id,
            checked(vec![diagnostic(&file, 1, 1, Level::Error)]),
        );
        assert!(rig.editor.diagnostics.is_empty(), "stale result ignored");
        rig.editor.apply_check(
            second.id,
            checked(vec![
                diagnostic(&file, 1, 8, Level::Error),
                diagnostic(&file, 1, 1, Level::Warning),
            ]),
        );
        assert_eq!(rig.editor.diagnostics.len(), 2);
        assert_eq!(rig.editor.check_running, None);
        let summary = rig.editor.check_summary.clone().unwrap();
        assert_eq!(
            count_text(summary.errors, summary.warnings),
            "1 error, 1 warning"
        );
        // The caret is on line 1, so the status shows that line's message.
        rig.frame(InputSnapshot::default());
        let at_caret = rig.editor.diagnostic_at_caret().map(diagnostic_text);
        assert_eq!(
            at_caret.as_deref(),
            Some("error[E0308]: mismatched types — expected `u32`")
        );
        // And the line is marked: a gutter bar and an underline.
        assert_eq!(rig.editor.marks.len(), 2);
        assert!(rig.editor.marks[0].underline.is_some());
    }

    #[test]
    fn a_problem_opens_its_file_at_its_line_and_column() {
        let dir = workspace(&[
            ("src/lib.rs", "pub fn a() {}\n"),
            ("src/other.rs", "one\ntwo\n    three\n"),
        ]);
        let other = dir.path().join("src/other.rs");
        let mut rig = Rig::new(None, Some(dir.path().to_path_buf()));
        rig.editor.open_file(dir.path().join("src/lib.rs"));
        rig.settle();
        rig.editor.run(Command::Check, &rig.host);
        let request = rig.editor.take_check_request().unwrap();
        rig.editor.apply_check(
            request.id,
            checked(vec![diagnostic(&other, 3, 5, Level::Warning)]),
        );
        rig.outcome(menu_input(Command::ToggleProblems));
        let panel = rig.editor.layout.problems.expect("panel shown");
        rig.click(Point {
            x: panel.x + 100.0,
            y: panel.y + PROBLEM_ROW_HEIGHT * 1.5,
        });
        assert_eq!(rig.active().path, other);
        assert_eq!(caret_line_column(&rig.active().area), (3, 5));
    }

    /// The whole path with the real cargo: save a broken file, run the check
    /// the editor asks for, and the error lands on its line.
    #[test]
    fn a_real_cargo_check_after_saving_marks_the_error() {
        let dir = workspace(&[
            (
                "Cargo.toml",
                "[package]\nname = \"broken\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[workspace]\n",
            ),
            ("src/main.rs", "fn main() {\n}\n"),
        ]);
        let file = dir.path().join("src/main.rs");
        let mut rig = Rig::new(None, Some(dir.path().to_path_buf()));
        rig.editor.open_file(file.clone());
        rig.settle();
        rig.editor.go_to(rig.editor.active.unwrap(), 1, 12);
        rig.type_text("\n    let x: u32 = \"no\";");
        rig.key(HostKey::S, cmd());
        let request = rig
            .editor
            .take_check_request()
            .expect("a check after saving");
        let outcome =
            crate::cargo_check::run(&request.file, &crate::cargo_check::Cancel::default());
        rig.editor.apply_check(request.id, outcome);
        let errors: Vec<_> = rig
            .editor
            .diagnostics
            .iter()
            .filter(|d| d.level == Level::Error)
            .collect();
        assert_eq!(errors.len(), 1, "{:?}", rig.editor.diagnostics);
        assert_eq!((errors[0].line, errors[0].column), (2, 18));
        assert_eq!(errors[0].file, file);
    }

    /// The editor's messages for the server, by method.
    fn sent(rig: &mut Rig) -> Vec<serde_json::Value> {
        rig.editor.lsp_outgoing()
    }

    fn methods(messages: &[serde_json::Value]) -> Vec<String> {
        messages
            .iter()
            .map(|m| m["method"].as_str().unwrap_or("(response)").to_string())
            .collect()
    }

    /// A rig with `files` open and a server that answered initialize and
    /// says it is ready.
    fn with_server(files: &[(&str, &str)], open: &str) -> (tempfile::TempDir, Rig) {
        let dir = workspace(files);
        let mut rig = Rig::new(None, Some(dir.path().to_path_buf()));
        rig.editor.open_file(dir.path().join(open));
        rig.settle();
        assert_eq!(rig.editor.take_lsp_start().as_deref(), Some(dir.path()));
        let first = sent(&mut rig);
        assert_eq!(methods(&first), vec!["initialize"]);
        rig.editor
            .apply_lsp(vec![serde_json::json!({"jsonrpc": "2.0", "id": 1,
            "result": {"capabilities": {"positionEncoding": "utf-32"}}})]);
        rig.editor.apply_lsp(vec![serde_json::json!({"jsonrpc": "2.0",
            "method": "experimental/serverStatus", "params": {"health": "ok", "quiescent": true}})]);
        rig.frame(InputSnapshot::default());
        (dir, rig)
    }

    #[test]
    fn rust_files_are_kept_in_step_with_the_server() {
        let (dir, mut rig) = with_server(&[("src/lib.rs", "fn a() {}\n")], "src/lib.rs");
        let opened = sent(&mut rig);
        assert_eq!(
            methods(&opened),
            vec!["initialized", "textDocument/didOpen"]
        );
        assert_eq!(opened[1]["params"]["textDocument"]["text"], "fn a() {}\n");
        rig.type_text("x");
        let changed = sent(&mut rig);
        // The change goes first; typing a name also asks for completions.
        assert_eq!(
            methods(&changed),
            vec!["textDocument/didChange", "textDocument/completion"]
        );
        assert_eq!(
            changed[0]["params"]["contentChanges"][0]["text"],
            "xfn a() {}\n"
        );
        rig.key(HostKey::S, cmd());
        assert!(methods(&sent(&mut rig)).contains(&"textDocument/didSave".to_string()));
        let index = rig.editor.active.unwrap();
        rig.editor.close(index);
        assert_eq!(methods(&sent(&mut rig)), vec!["textDocument/didClose"]);
        assert!(
            rig.editor.take_lsp_start().is_none(),
            "one server per folder"
        );
        drop(dir);
    }

    #[test]
    fn the_servers_diagnostics_are_marked_and_counted_live() {
        let (dir, mut rig) = with_server(
            &[("src/lib.rs", "fn a() {\n    let x = ;\n}\n")],
            "src/lib.rs",
        );
        let uri = crate::lsp::uri::from_path(&dir.path().join("src/lib.rs"));
        rig.editor.apply_lsp(vec![serde_json::json!({"jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics", "params": {"uri": uri, "diagnostics": [
                {"range": {"start": {"line": 1, "character": 12}, "end": {"line": 1, "character": 13}},
                 "severity": 1, "message": "Syntax Error: expected expression"}]}})]);
        rig.frame(InputSnapshot::default());
        assert_eq!(rig.editor.marks.len(), 1);
        assert_eq!(level_counts(&rig.editor.shown_diagnostics()), (1, 0));
        assert!(rig.editor.layout.status_check.is_some());
    }

    #[test]
    fn resting_the_pointer_on_a_name_shows_its_hover() {
        let (_dir, mut rig) = with_server(&[("src/lib.rs", "fn alpha() {}\n")], "src/lib.rs");
        sent(&mut rig);
        let line = rig.active().area.layout_cache.lines[0].clone();
        let point = Point {
            x: line.rect.x + line.char_offsets[5] + 1.0,
            y: line.rect.y + 4.0,
        };
        rig.frame(InputSnapshot {
            mouse_x: point.x,
            mouse_y: point.y,
            ..InputSnapshot::default()
        });
        assert!(rig
            .editor
            .next_wake(rig.now)
            .is_some_and(|wait| wait <= HOVER_DELAY));
        assert!(methods(&sent(&mut rig)).is_empty(), "not before the delay");
        rig.now += HOVER_DELAY;
        rig.frame(InputSnapshot {
            mouse_x: point.x,
            mouse_y: point.y,
            ..InputSnapshot::default()
        });
        let asked = sent(&mut rig);
        assert_eq!(methods(&asked), vec!["textDocument/hover"]);
        assert_eq!(
            asked[0]["params"]["position"],
            serde_json::json!({"line": 0, "character": 5})
        );
        let id = asked[0]["id"].clone();
        rig.editor
            .apply_lsp(vec![serde_json::json!({"jsonrpc": "2.0", "id": id,
            "result": {"contents": {"kind": "markdown", "value": "```rust\nfn alpha()\n```"}}})]);
        assert_eq!(
            rig.editor.hover.as_ref().map(|(text, _)| text.as_str()),
            Some("fn alpha()")
        );
        rig.frame(InputSnapshot {
            mouse_x: point.x + 40.0,
            mouse_y: point.y + 40.0,
            ..InputSnapshot::default()
        });
        assert!(rig.editor.hover.is_none(), "moving away hides it");
    }

    #[test]
    fn cmd_click_goes_to_the_definition_and_back_returns() {
        let (dir, mut rig) = with_server(
            &[
                (
                    "src/lib.rs",
                    "mod other;\nfn use_it() { other::target(); }\n",
                ),
                ("src/other.rs", "\n\npub fn target() {}\n"),
            ],
            "src/lib.rs",
        );
        sent(&mut rig);
        rig.editor.active_buffer_mut().unwrap().area.set_caret(3);
        rig.frame(InputSnapshot::default());
        let line = rig.active().area.layout_cache.lines[1].clone();
        let point = Point {
            x: line.rect.x + line.char_offsets[23] + 1.0,
            y: line.rect.y + 4.0,
        };
        rig.frame(InputSnapshot {
            mouse_x: point.x,
            mouse_y: point.y,
            mouse_pressed: true,
            mouse_down: true,
            modifiers: cmd(),
            ..InputSnapshot::default()
        });
        let asked = sent(&mut rig);
        assert_eq!(methods(&asked), vec!["textDocument/definition"]);
        assert_eq!(
            asked[0]["params"]["position"],
            serde_json::json!({"line": 1, "character": 23})
        );
        let other = dir.path().join("src/other.rs");
        rig.editor.apply_lsp(vec![serde_json::json!({"jsonrpc": "2.0", "id": asked[0]["id"].clone(),
            "result": [{"uri": crate::lsp::uri::from_path(&other),
                        "range": {"start": {"line": 2, "character": 7}, "end": {"line": 2, "character": 13}}}]})]);
        rig.settle();
        assert_eq!(rig.active().path, other);
        assert_eq!(caret_line_column(&rig.active().area), (3, 8));
        rig.outcome(menu_input(Command::GoBack));
        assert_eq!(rig.active().path, dir.path().join("src/lib.rs"));
        assert_eq!(rig.active().area.caret(), 3);
    }

    #[test]
    fn a_server_that_stops_is_reported_and_not_restarted() {
        let (_dir, mut rig) = with_server(&[("src/lib.rs", "fn a() {}\n")], "src/lib.rs");
        rig.editor
            .lsp_ended("the server closed its output".to_string());
        rig.frame(InputSnapshot::default());
        assert!(rig.editor.lsp.is_none());
        assert!(rig.editor.take_lsp_start().is_none());
        assert!(rig.status().starts_with("rust-analyzer stopped"));
    }

    fn completion_reply(id: &serde_json::Value) -> serde_json::Value {
        serde_json::json!({"jsonrpc": "2.0", "id": id.clone(), "result": {"isIncomplete": true, "items": [
            {"label": "HashMap", "sortText": "a", "detail": "std::collections::HashMap",
             "textEdit": {"range": {"start": {"line": 1, "character": 12}, "end": {"line": 1, "character": 14}},
                          "newText": "HashMap"},
             "additionalTextEdits": [{"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}},
                                      "newText": "use std::collections::HashMap;\n"}]},
            {"label": "HashSet", "sortText": "b"},
            {"label": "Hasher", "sortText": "c"}
        ]}})
    }

    #[test]
    fn typing_a_name_lists_completions_and_enter_inserts_with_its_import() {
        let (_dir, mut rig) = with_server(
            &[("src/lib.rs", "fn a() {\n    let m = \n}\n")],
            "src/lib.rs",
        );
        sent(&mut rig);
        let caret = rig.active().area.document.to_string().find("= ").unwrap() + 2;
        rig.editor
            .active_buffer_mut()
            .unwrap()
            .area
            .set_caret(caret);
        rig.type_text("Ha");
        let asked = sent(&mut rig);
        let requests: Vec<_> = asked
            .iter()
            .filter(|m| m["method"] == "textDocument/completion")
            .collect();
        assert!(!requests.is_empty(), "{asked:?}");
        let last = requests.last().unwrap();
        assert_eq!(
            last["params"]["position"],
            serde_json::json!({"line": 1, "character": 14})
        );
        rig.editor.apply_lsp(vec![completion_reply(&last["id"])]);
        assert_eq!(rig.editor.visible_completions().len(), 3);
        // Typing on narrows the list before the next answer arrives.
        rig.type_text("shM");
        assert_eq!(rig.editor.visible_completions(), vec![0]);
        rig.key(HostKey::Enter, Modifiers::default());
        assert_eq!(
            rig.active().area.text(),
            "use std::collections::HashMap;\nfn a() {\n    let m = HashMap\n}\n"
        );
        let text = rig.active().area.text();
        assert_eq!(
            rig.active().area.caret(),
            text.find("HashMap\n}").unwrap() + 7
        );
        assert!(rig.editor.completion.is_none());
    }

    #[test]
    fn escape_or_a_space_closes_the_list_and_a_click_accepts() {
        let (_dir, mut rig) = with_server(
            &[("src/lib.rs", "fn a() {\n    let m = \n}\n")],
            "src/lib.rs",
        );
        sent(&mut rig);
        let caret = rig.active().area.document.to_string().find("= ").unwrap() + 2;
        rig.editor
            .active_buffer_mut()
            .unwrap()
            .area
            .set_caret(caret);
        rig.type_text("Ha");
        let id = sent(&mut rig).last().unwrap()["id"].clone();
        rig.editor.apply_lsp(vec![completion_reply(&id)]);
        rig.key(HostKey::Escape, Modifiers::default());
        assert!(rig.editor.completion.is_none());
        rig.type_text("s");
        let id = sent(&mut rig).last().unwrap()["id"].clone();
        rig.editor.apply_lsp(vec![completion_reply(&id)]);
        rig.type_text(" ");
        assert!(rig.editor.completion.is_none(), "a space ends the word");

        rig.key(HostKey::Backspace, Modifiers::default());
        rig.type_text("h");
        let id = sent(&mut rig).last().unwrap()["id"].clone();
        rig.editor.apply_lsp(vec![completion_reply(&id)]);
        let mut scene = Vec::new();
        rig.editor.paint(&mut scene);
        let (_, row) = rig.editor.completion.as_ref().unwrap().rows[1];
        rig.click(Point {
            x: row.x + 10.0,
            y: row.y + 5.0,
        });
        assert!(
            rig.active().area.text().contains("let m = HashSet"),
            "{}",
            rig.active().area.text()
        );
    }

    #[test]
    fn typing_with_completions_open_still_undoes_a_word_at_a_time() {
        let (_dir, mut rig) = with_server(&[("src/lib.rs", "fn a() {}\n")], "src/lib.rs");
        sent(&mut rig);
        for ch in ["h", "e", "l"] {
            rig.type_text(ch);
            let asked = sent(&mut rig);
            if let Some(request) = asked
                .iter()
                .rev()
                .find(|m| m["method"] == "textDocument/completion")
            {
                rig.editor.apply_lsp(vec![completion_reply(&request["id"])]);
            }
            rig.frame(InputSnapshot::default());
        }
        assert_eq!(rig.active().area.text(), "helfn a() {}\n");
        rig.outcome(menu_input(Command::Undo));
        assert_eq!(rig.active().area.text(), "fn a() {}\n");
    }

    #[test]
    fn rename_edits_the_open_file_and_opens_the_others() {
        let (dir, mut rig) = with_server(
            &[
                (
                    "src/lib.rs",
                    "mod other;\nfn helper() {}\nfn a() { helper(); }\n",
                ),
                ("src/other.rs", "use crate::helper;\nfn b() { helper(); }\n"),
            ],
            "src/lib.rs",
        );
        sent(&mut rig);
        // Caret inside `helper` on line 2.
        rig.editor.active_buffer_mut().unwrap().area.set_caret(16);
        rig.outcome(menu_input(Command::RenameSymbol));
        assert_eq!(rig.editor.goto.as_ref().unwrap().text(), "helper");
        rig.key(HostKey::A, cmd());
        rig.type_text("assist");
        rig.key(HostKey::Enter, Modifiers::default());
        let asked = sent(&mut rig);
        let rename = asked
            .iter()
            .find(|m| m["method"] == "textDocument/rename")
            .expect("a rename request");
        assert_eq!(rename["params"]["newName"], "assist");
        assert_eq!(
            rename["params"]["position"],
            serde_json::json!({"line": 1, "character": 3})
        );
        let lib = crate::lsp::uri::from_path(&dir.path().join("src/lib.rs"));
        let other = crate::lsp::uri::from_path(&dir.path().join("src/other.rs"));
        let edit = |line: u64, start: u64| {
            serde_json::json!({"range": {"start": {"line": line, "character": start},
                "end": {"line": line, "character": start + 6}}, "newText": "assist"})
        };
        rig.editor.apply_lsp(vec![serde_json::json!({"jsonrpc": "2.0", "id": rename["id"].clone(),
            "result": {"changes": {lib: [edit(1, 3), edit(2, 9)], other: [edit(0, 11), edit(1, 9)]}}})]);
        rig.settle();
        assert_eq!(rig.active().path, dir.path().join("src/lib.rs"));
        assert_eq!(
            rig.active().area.text(),
            "mod other;\nfn assist() {}\nfn a() { assist(); }\n"
        );
        let other_buffer = rig
            .editor
            .buffers
            .iter()
            .find(|buffer| buffer.path == dir.path().join("src/other.rs"))
            .expect("opened for the rename");
        assert_eq!(
            other_buffer.area.text(),
            "use crate::assist;\nfn b() { assist(); }\n"
        );
        assert!(other_buffer.dirty() && rig.active().dirty());
    }

    #[test]
    fn a_real_rename_reply_edits_every_place() {
        let main_text = "mod other;\n\nfn main() {\n    let total = helper(41);\n    println!(\"{total}\");\n}\n\nfn helper(value: i32) -> i32 {\n    other::twice(value)\n}\n";
        let (dir, mut rig) = with_server(
            &[
                ("src/main.rs", main_text),
                (
                    "src/other.rs",
                    "pub fn twice(value: i32) -> i32 {\n    crate::helper(0) + value * 2\n}\n",
                ),
            ],
            "src/main.rs",
        );
        sent(&mut rig);
        let at = main_text.find("fn helper").unwrap() + 4;
        rig.editor.active_buffer_mut().unwrap().area.set_caret(at);
        rig.outcome(menu_input(Command::RenameSymbol));
        rig.key(HostKey::A, cmd());
        rig.type_text("assist");
        rig.key(HostKey::Enter, Modifiers::default());
        let asked = sent(&mut rig);
        let rename = asked
            .iter()
            .find(|m| m["method"] == "textDocument/rename")
            .unwrap();
        let main = crate::lsp::uri::from_path(&dir.path().join("src/main.rs"));
        let other = crate::lsp::uri::from_path(&dir.path().join("src/other.rs"));
        let edit = |line: u64, start: u64| {
            serde_json::json!({"newText": "assist", "range": {"start": {"line": line, "character": start},
                "end": {"line": line, "character": start + 6}}})
        };
        rig.editor.apply_lsp(vec![
            serde_json::json!({"id": rename["id"].clone(), "jsonrpc": "2.0",
            "result": {"documentChanges": [
                {"edits": [edit(3, 16), edit(7, 3)], "textDocument": {"uri": main, "version": 1}},
                {"edits": [edit(1, 11)], "textDocument": {"uri": other, "version": null}}
            ]}}),
        ]);
        rig.settle();
        let text = rig.active().area.text();
        assert!(text.contains("let total = assist(41);"), "{text}");
        assert!(text.contains("fn assist(value: i32)"), "{text}");
        // And it is what is drawn: several edits in one frame used to leave
        // earlier-edited lines showing their old text.
        let shown: Vec<String> = rig
            .active()
            .area
            .layout_cache
            .lines
            .iter()
            .map(|line| line.display_text.clone())
            .collect();
        assert!(
            shown.contains(&"fn assist(value: i32) -> i32 {".to_string()),
            "{shown:?}"
        );
    }

    #[test]
    fn new_file_and_new_folder_go_into_the_selected_folder() {
        let dir = workspace(&[("src/lib.rs", "")]);
        let mut rig = Rig::new(None, Some(dir.path().to_path_buf()));
        rig.editor
            .tree
            .as_mut()
            .unwrap()
            .reveal(&dir.path().join("src/lib.rs"));
        rig.editor.tree.as_mut().unwrap().selected = Some(dir.path().join("src"));
        rig.settle();
        rig.outcome(menu_input(Command::NewFile));
        rig.type_text("util.rs");
        rig.key(HostKey::Enter, Modifiers::default());
        rig.settle();
        let created = dir.path().join("src/util.rs");
        assert!(created.is_file());
        assert_eq!(rig.active().path, created, "opened in a tab");

        rig.editor.tree.as_mut().unwrap().selected = Some(dir.path().join("src/lib.rs"));
        rig.outcome(menu_input(Command::NewFolder));
        rig.type_text("nested");
        rig.key(HostKey::Enter, Modifiers::default());
        rig.settle();
        assert!(
            dir.path().join("src/nested").is_dir(),
            "beside the selected file"
        );

        rig.editor.tree.as_mut().unwrap().selected = Some(dir.path().join("src"));
        rig.outcome(menu_input(Command::NewFile));
        rig.type_text("util.rs");
        rig.key(HostKey::Enter, Modifiers::default());
        rig.settle();
        assert!(rig.status().contains("already exists"), "{}", rig.status());
    }

    #[test]
    fn renaming_an_open_file_keeps_its_tab_and_unsaved_text() {
        let (dir, mut rig) = with_server(&[("src/old.rs", "fn a() {}\n")], "src/old.rs");
        sent(&mut rig);
        rig.type_text("// edited\n");
        sent(&mut rig);
        rig.outcome(menu_input(Command::RenameFile));
        assert_eq!(rig.editor.goto.as_ref().unwrap().text(), "old.rs");
        // The stem is selected: typing replaces it and keeps ".rs".
        rig.type_text("new");
        rig.key(HostKey::Enter, Modifiers::default());
        rig.settle();
        let new = dir.path().join("src/new.rs");
        assert!(new.is_file() && !dir.path().join("src/old.rs").exists());
        assert_eq!(rig.active().path, new);
        assert_eq!(rig.active().title, "new.rs");
        assert!(rig.active().dirty(), "unsaved text kept");
        assert_eq!(std::fs::read_to_string(&new).unwrap(), "fn a() {}\n");
        let told = methods(&sent(&mut rig));
        assert!(
            told.contains(&"textDocument/didClose".to_string()),
            "{told:?}"
        );
        assert!(
            told.contains(&"textDocument/didOpen".to_string()),
            "{told:?}"
        );
        rig.key(HostKey::S, cmd());
        assert_eq!(
            std::fs::read_to_string(&new).unwrap(),
            "// edited\nfn a() {}\n"
        );
    }

    #[test]
    fn move_to_trash_asks_first_and_closes_the_tab() {
        let dir = workspace(&[("a.rs", "a"), ("b.rs", "b")]);
        let mut rig = Rig::new(None, Some(dir.path().to_path_buf()));
        rig.editor.open_file(dir.path().join("a.rs"));
        rig.settle();
        rig.type_text("x");
        rig.outcome(menu_input(Command::TrashFile));
        assert_eq!(
            rig.editor.trash_prompt.as_deref(),
            Some(dir.path().join("a.rs").as_path())
        );
        rig.key(HostKey::Escape, Modifiers::default());
        assert!(rig.editor.trash_prompt.is_none(), "Escape cancels");
        rig.outcome(menu_input(Command::TrashFile));
        // Confirm, but take the request rather than run it: the real Trash
        // is not for tests.
        rig.editor.run(Command::TrashConfirm, &rig.host);
        let requests = rig.editor.take_requests();
        let trash = requests
            .iter()
            .find_map(|request| match request {
                IoRequest::FileOp(op @ FileOp::Trash(_)) => Some(op.clone()),
                _ => None,
            })
            .expect("a Trash request");
        assert_eq!(trash, FileOp::Trash(dir.path().join("a.rs")));
        rig.editor.apply(
            IoResponse::FileOpDone {
                op: trash,
                result: Ok(()),
            },
            rig.now,
        );
        assert!(rig.only_a_blank_page(), "its tab closed");
        assert!(rig.status().starts_with("Moved a.rs to the Trash"));
    }

    #[test]
    fn find_in_files_lists_matches_and_a_click_selects_one() {
        let dir = workspace(&[
            ("src/lib.rs", "pub fn helper() {}\n"),
            (
                "src/other.rs",
                "use crate::helper;\nfn b() {\n    helper();\n}\n",
            ),
        ]);
        let mut rig = Rig::new(None, Some(dir.path().to_path_buf()));
        rig.key(
            HostKey::F,
            Modifiers {
                shift: true,
                ..cmd()
            },
        );
        assert!(rig.editor.search_open);
        rig.type_text("helper");
        rig.key(HostKey::Enter, Modifiers::default());
        let request = rig.editor.take_search_request().expect("a search");
        assert_eq!(request.query, "helper");
        let outcome = crate::search::search(&request.root, &request.query, &Default::default());
        assert_eq!(outcome.matches.len(), 3);
        rig.editor.apply_search(request.id, outcome);
        rig.frame(InputSnapshot::default());
        let panel = rig.editor.layout.search.expect("panel shown");
        // The third result: src/other.rs line 3.
        rig.click(Point {
            x: panel.x + 200.0,
            y: panel.y + SEARCH_HEADER_HEIGHT + PROBLEM_ROW_HEIGHT * 2.5,
        });
        assert_eq!(rig.active().path, dir.path().join("src/other.rs"));
        let area = &rig.active().area;
        let (start, end) = area.selection_range().expect("the match selected");
        assert_eq!(area.document.slice_chars(start, end), "helper");
        assert_eq!(caret_line_column(area).0, 3);
    }

    #[test]
    fn the_split_shows_two_files_and_a_click_moves_focus() {
        let dir = workspace(&[
            ("a.rs", "fn a() {}\n"),
            ("b.rs", "fn b() {}\n"),
            ("c.rs", "fn c() {}\n"),
        ]);
        let mut rig = Rig::new(None, Some(dir.path().to_path_buf()));
        rig.editor.open_file(dir.path().join("a.rs"));
        rig.editor.open_file(dir.path().join("b.rs"));
        rig.settle();
        rig.outcome(menu_input(Command::ToggleSplit));
        rig.frame(InputSnapshot::default());
        let left = rig.editor.layout.editor;
        let right = rig.editor.layout.other_pane.expect("two panes");
        assert!(left.right() <= right.x, "focused pane on the left");
        let other = rig.editor.split.unwrap().other;
        let a = rig.editor.buffer_index(other).unwrap();
        assert_eq!(
            rig.editor.buffers[a].path,
            dir.path().join("a.rs"),
            "the previous file"
        );
        assert_eq!(rig.active().path, dir.path().join("b.rs"));
        assert_eq!(rig.editor.buffers[a].area.bounds, right);

        // A click in the right pane focuses it and lands in its text.
        rig.click(Point {
            x: right.x + 120.0,
            y: right.y + 14.0,
        });
        assert_eq!(rig.active().path, dir.path().join("a.rs"));
        assert!(rig.editor.split.unwrap().focused_right);
        assert_eq!(rig.editor.layout.editor, right);
        rig.type_text("x");
        assert!(rig.active().area.text().contains('x'));

        // Closing the focused pane's file: it takes the other pane's file,
        // and with no third file open the split closes.
        let a = rig.editor.active.unwrap();
        rig.editor.close(a);
        rig.frame(InputSnapshot::default());
        assert_eq!(rig.active().path, dir.path().join("b.rs"));
        assert!(rig.editor.split.is_none(), "one file left: no split");

        // With three files, the second pane gets another one.
        rig.editor.open_file(dir.path().join("a.rs"));
        rig.editor.open_file(dir.path().join("c.rs"));
        rig.settle();
        rig.outcome(menu_input(Command::ToggleSplit));
        let focused = rig.editor.active.unwrap();
        rig.editor.close(focused);
        rig.frame(InputSnapshot::default());
        let split = rig.editor.split.expect("still split");
        let other = rig.editor.buffer_index(split.other).unwrap();
        assert_ne!(Some(other), rig.editor.active, "two different files");
    }

    // ----- drag and drop -----

    fn row_point(rig: &mut Rig, path: &Path) -> Point {
        let tree = rig.editor.tree.as_mut().unwrap();
        let bounds = tree.bounds;
        let scroll = tree.scroll_y;
        let index = tree
            .rows()
            .iter()
            .position(|row| row.path == path)
            .unwrap_or_else(|| panic!("{} is in the tree", path.display()));
        Point {
            x: bounds.x + 60.0,
            y: bounds.y + (index as f32 + 0.5) * theme::TREE_ROW_HEIGHT - scroll,
        }
    }

    fn drag_pointer(rig: &mut Rig, from: Point, to: Point, modifiers: Modifiers) {
        rig.frame(InputSnapshot {
            mouse_x: from.x,
            mouse_y: from.y,
            mouse_pressed: true,
            mouse_down: true,
            modifiers,
            ..InputSnapshot::default()
        });
        for step in 1..=3 {
            let t = step as f32 / 3.0;
            rig.frame(InputSnapshot {
                mouse_x: from.x + (to.x - from.x) * t,
                mouse_y: from.y + (to.y - from.y) * t,
                mouse_down: true,
                modifiers,
                ..InputSnapshot::default()
            });
        }
        rig.frame(InputSnapshot {
            mouse_x: to.x,
            mouse_y: to.y,
            mouse_released: true,
            modifiers,
            ..InputSnapshot::default()
        });
        rig.settle();
    }

    fn tab_titles(rig: &Rig) -> Vec<String> {
        rig.editor
            .buffers
            .iter()
            .map(|buffer| buffer.title.clone())
            .collect()
    }

    fn center(rect: Rect) -> Point {
        Point {
            x: rect.x + rect.width / 2.0,
            y: rect.y + rect.height / 2.0,
        }
    }

    #[test]
    fn a_tree_entry_dragged_onto_a_folder_moves_and_its_tab_follows() {
        let dir = workspace(&[("src/a.rs", "fn a() {}\n"), ("docs/notes.md", "")]);
        let mut rig = Rig::new(None, Some(dir.path().to_path_buf()));
        let from = dir.path().join("src/a.rs");
        rig.editor.tree.as_mut().unwrap().reveal(&from);
        rig.settle();
        // A press and release without moving is a click: the file opens.
        let point = row_point(&mut rig, &from);
        drag_pointer(&mut rig, point, point, Modifiers::default());
        assert_eq!(rig.active().path, from);

        let to_folder = row_point(&mut rig, &dir.path().join("docs"));
        drag_pointer(&mut rig, point, to_folder, Modifiers::default());
        let moved = dir.path().join("docs/a.rs");
        assert!(moved.is_file() && !from.exists());
        assert_eq!(rig.active().path, moved, "the tab follows");
        assert!(rig.status().starts_with("Moved"), "{}", rig.status());

        // A folder never goes inside itself.
        let docs = row_point(&mut rig, &dir.path().join("docs"));
        rig.editor.tree.as_mut().unwrap().reveal(&moved);
        rig.settle();
        let inside = row_point(&mut rig, &moved);
        drag_pointer(&mut rig, docs, inside, Modifiers::default());
        assert!(rig.status().contains("inside itself"), "{}", rig.status());
        assert!(dir.path().join("docs").is_dir());
    }

    #[test]
    fn tabs_drag_to_reorder_and_between_panes() {
        let dir = workspace(&[("a.rs", ""), ("b.rs", ""), ("c.rs", "")]);
        let mut rig = Rig::new(None, Some(dir.path().to_path_buf()));
        for name in ["a.rs", "b.rs", "c.rs"] {
            rig.editor.open_file(dir.path().join(name));
        }
        rig.settle();
        let c = rig.editor.layout.tab_slots[2].rect;
        let a = rig.editor.layout.tab_slots[0].rect;
        drag_pointer(
            &mut rig,
            center(c),
            Point {
                x: a.x + 4.0,
                y: a.y + 8.0,
            },
            Modifiers::default(),
        );
        assert_eq!(tab_titles(&rig), ["c.rs", "a.rs", "b.rs"]);
        assert_eq!(rig.active().title, "c.rs", "the dragged tab stays active");

        // Unsplit, a tab dropped on the editor's right half splits it.
        let b = rig.editor.layout.tab_slots[2].rect;
        let editor = rig.editor.layout.editor;
        let right = Point {
            x: editor.x + editor.width * 0.8,
            y: editor.y + 100.0,
        };
        drag_pointer(&mut rig, center(b), right, Modifiers::default());
        let split = rig.editor.split.expect("split");
        let b_id = rig.editor.buffers[2].id;
        assert_eq!(split.other, b_id, "b.rs on the right");
        assert_ne!(rig.active().title, "b.rs");

        // The focused pane's tab dropped on the other pane swaps the two.
        let active_title = rig.active().title.clone();
        let active_slot = rig
            .editor
            .buffers
            .iter()
            .position(|buffer| buffer.title == active_title)
            .unwrap();
        let slot = rig.editor.layout.tab_slots[active_slot].rect;
        let other = rig.editor.layout.other_pane.unwrap();
        drag_pointer(&mut rig, center(slot), center(other), Modifiers::default());
        let split = rig.editor.split.expect("still split");
        let other_title = rig.editor.buffers[rig.editor.buffer_index(split.other).unwrap()]
            .title
            .clone();
        assert_eq!(other_title, active_title);
        assert_eq!(rig.active().title, "b.rs");
    }

    #[test]
    fn files_dropped_from_the_file_manager_open_or_copy_into_the_tree() {
        let dir = workspace(&[("src/lib.rs", "")]);
        let outside = workspace(&[("note.txt", "hello"), ("kit/x.rs", "x")]);
        let mut rig = Rig::new(None, Some(dir.path().to_path_buf()));
        let editor = center(rig.editor.layout.editor);
        rig.frame(InputSnapshot {
            mouse_x: editor.x,
            mouse_y: editor.y,
            dropped_files: vec![outside.path().join("note.txt")],
            ..InputSnapshot::default()
        });
        rig.settle();
        assert_eq!(rig.active().path, outside.path().join("note.txt"));

        let src = row_point(&mut rig, &dir.path().join("src"));
        rig.frame(InputSnapshot {
            mouse_x: src.x,
            mouse_y: src.y,
            file_drag_over: true,
            ..InputSnapshot::default()
        });
        assert_eq!(
            rig.editor.tree.as_ref().unwrap().drop_target,
            Some(dir.path().join("src"))
        );
        rig.frame(InputSnapshot {
            mouse_x: src.x,
            mouse_y: src.y,
            dropped_files: vec![outside.path().join("kit"), outside.path().join("note.txt")],
            ..InputSnapshot::default()
        });
        rig.settle();
        assert_eq!(
            fs::read_to_string(dir.path().join("src/kit/x.rs")).unwrap(),
            "x"
        );
        assert!(dir.path().join("src/note.txt").is_file());
        assert!(
            outside.path().join("note.txt").is_file(),
            "copied, not moved"
        );
        assert_eq!(rig.editor.tree.as_ref().unwrap().drop_target, None);
    }

    #[test]
    fn selected_text_dragged_to_the_other_pane_moves_there() {
        let dir = workspace(&[("a.rs", "alpha beta\n"), ("b.rs", "gamma\n")]);
        let mut rig = Rig::new(None, Some(dir.path().to_path_buf()));
        rig.editor.open_file(dir.path().join("b.rs"));
        rig.editor.open_file(dir.path().join("a.rs"));
        rig.settle();
        rig.outcome(menu_input(Command::ToggleSplit));
        rig.settle();
        assert_eq!(rig.active().title, "a.rs");
        let index = rig.editor.active.unwrap();
        rig.editor.buffers[index].area.select_range(6, 10);
        rig.settle();
        let area = &rig.editor.buffers[index].area;
        let line = &area.layout_cache.lines[0];
        let from = Point {
            x: line.rect.x + line.char_offsets[8] - area.scroll_x,
            y: line.rect.y + line.rect.height / 2.0,
        };
        let other = rig.editor.layout.other_pane.unwrap();
        // Past the end of the other file's first line.
        let to = Point {
            x: other.x + other.width * 0.6,
            y: other.y + 10.0,
        };
        drag_pointer(&mut rig, from, to, Modifiers::default());
        let a = rig
            .editor
            .buffer_index(
                rig.editor
                    .buffers
                    .iter()
                    .find(|b| b.title == "a.rs")
                    .unwrap()
                    .id,
            )
            .unwrap();
        let b = rig
            .editor
            .buffers
            .iter()
            .position(|b| b.title == "b.rs")
            .unwrap();
        assert_eq!(rig.editor.buffers[a].area.text(), "alpha \n");
        assert_eq!(rig.editor.buffers[b].area.text(), "gammabeta\n");
        assert_eq!(rig.active().title, "b.rs", "focus follows the text");
    }

    // ----- terminal -----

    fn writes(rig: &mut Rig) -> Vec<Vec<u8>> {
        rig.editor
            .take_terminal_requests()
            .into_iter()
            .filter_map(|request| match request {
                TerminalRequest::Write { bytes, .. } => Some(bytes),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_terminal_takes_typing_and_control_keys_while_focused() {
        let dir = workspace(&[("a.rs", "fn a() {}\n")]);
        let mut rig = Rig::new(None, Some(dir.path().to_path_buf()));
        rig.editor.open_file(dir.path().join("a.rs"));
        rig.settle();
        rig.outcome(menu_input(Command::ToggleTerminal));
        let requests = rig.editor.take_terminal_requests();
        assert!(
            matches!(&requests[..], [TerminalRequest::Start { cwd, .. }] if cwd == dir.path()),
            "{requests:?}"
        );
        assert!(rig.editor.layout.terminal.is_some());
        rig.type_text("ls");
        rig.frame(InputSnapshot {
            control_text: "\u{3}".to_string(),
            key_events: vec![HostKeyEvent {
                key: HostKey::C,
                modifiers: Modifiers {
                    ctrl: true,
                    ..Modifiers::default()
                },
            }],
            ..InputSnapshot::default()
        });
        rig.key(HostKey::Escape, Modifiers::default());
        assert_eq!(
            writes(&mut rig),
            [b"ls".to_vec(), b"\x03".to_vec(), b"\x1b".to_vec()]
        );
        assert_eq!(
            rig.active().area.text(),
            "fn a() {}\n",
            "the file is untouched"
        );

        // A click in the editor gives it the keys back.
        let editor = center(rig.editor.layout.editor);
        rig.click(editor);
        rig.type_text("x");
        assert!(writes(&mut rig).is_empty());
        assert!(rig.active().area.text().contains('x'));
    }

    #[test]
    fn a_program_that_asks_gets_clicks_with_their_button() {
        let dir = workspace(&[("a.rs", "fn a() {}\n")]);
        let mut rig = Rig::new(None, Some(dir.path().to_path_buf()));
        rig.settle();
        rig.outcome(menu_input(Command::ToggleTerminal));
        rig.editor.take_terminal_requests();
        rig.editor.terminal_output(0, b"\x1b[?1000h\x1b[?1006h");
        let panel = rig.editor.layout.terminal.expect("terminal open");
        let point = Point {
            x: panel.x + 40.0,
            y: panel.y + panel.height - 40.0,
        };
        for (pressed, released) in [(true, false), (false, true)] {
            rig.frame(InputSnapshot {
                mouse_x: point.x,
                mouse_y: point.y,
                mouse_pressed: pressed,
                mouse_down: pressed,
                mouse_released: released,
                mouse_button: PointerButton::Secondary,
                ..InputSnapshot::default()
            });
        }
        let reports: Vec<String> = writes(&mut rig)
            .into_iter()
            .map(|bytes| String::from_utf8(bytes).unwrap())
            .collect();
        assert_eq!(reports.len(), 2, "{reports:?}");
        assert!(reports[0].starts_with("\x1b[<2;") && reports[0].ends_with('M'));
        assert!(reports[1].starts_with("\x1b[<2;") && reports[1].ends_with('m'));
        assert!(rig.editor.focus == Focus::Terminal);
    }
}
