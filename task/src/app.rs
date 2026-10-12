//! The Task window with no platform in it: a day plan and a project plan
//! over one plan, as the Win32 Task shows them. The `task` binary connects
//! it to `loadngo-host-desktop`: input in, paint operations out, and file
//! work (`IoRequest`) run on the host's offload workers.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use chrono::{Local, NaiveDate};
use data::types::Id;
use loadngo_host_core::{HostKey, InputSnapshot};
use ui_core::{
    HorizontalAlign, Key, MenuBar, MenuBarModel, MenuCommand, MenuItem, Modifiers, PaintOp, Point,
    PointerButton, PointerState, Rect, Shortcut, TextFieldModel, UiEvent, WidgetAction,
};

use crate::clock::{self, SLOTS_PER_DAY, TICKS_PER_SLOT};
use crate::draw::{self, rect};
use crate::plan::{parse_plan, PaneKind, Plan, MAX_PRIORITY};
use crate::store::{self, IoRequest, IoResponse};
use crate::tree_layout::{self, TreeLayout, NODE_HEIGHT};

const TOOLBAR_HEIGHT: f32 = 40.0;
const STATUS_HEIGHT: f32 = 24.0;
const BANNER_HEIGHT: f32 = 40.0;
const PANE_HEADER_HEIGHT: f32 = 24.0;
const GUTTER_WIDTH: f32 = 70.0;
const SLOT_PX: f32 = 18.0;
const SPLITTER_WIDTH: f32 = 6.0;
const MIN_PANE_WIDTH: f32 = 80.0;
const GRIP_WIDTH: f32 = 6.0;
const ROW_HEIGHT: f32 = 24.0;
const FIELD_HEIGHT: f32 = 28.0;
const MARGIN: f32 = 8.0;
const DETAILS_HEIGHT: f32 = 172.0;
const SUGGESTION_HEIGHT: f32 = 22.0;
const MAX_SUGGESTIONS: usize = 6;
/// Pointer travel before a press becomes a drag.
const DRAG_THRESHOLD: f32 = 4.0;
/// How near an entry's bottom edge a press resizes it.
const RESIZE_EDGE: f32 = 6.0;
/// Edits are written this long after the last one.
const SAVE_DELAY: Duration = Duration::from_secs(1);
/// After a failed save, the next try waits this long.
const SAVE_RETRY: Duration = Duration::from_secs(10);
/// The list beside the planner: the Win32 Task's default and least width.
const DEFAULT_LIST_WIDTH: f32 = 220.0;
const MIN_LIST_WIDTH: f32 = 150.0;
const DEFAULT_SPLIT: f32 = 0.55;
/// The planner opens at 8am, as the Win32 Task's did.
const DEFAULT_FIRST_SLOT: f32 = 32.0;

/// What the app needs from its host.
pub trait TaskHost {
    /// The width of `text` drawn at `font_size`.
    fn measure(&self, text: &str, font_size: u16) -> f32;
    fn read_clipboard(&self) -> Option<String>;
    fn write_clipboard(&self, text: &str);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Day,
    Project,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
enum Command {
    NewTask = 1,
    Save,
    Quit,
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    SelectAll,
    Delete,
    ShowDay,
    ShowProject,
    PreviousDay,
    NextDay,
    Today,
}

impl Command {
    const ALL: [Command; 15] = [
        Command::NewTask,
        Command::Save,
        Command::Quit,
        Command::Undo,
        Command::Redo,
        Command::Cut,
        Command::Copy,
        Command::Paste,
        Command::SelectAll,
        Command::Delete,
        Command::ShowDay,
        Command::ShowProject,
        Command::PreviousDay,
        Command::NextDay,
        Command::Today,
    ];

    fn menu_command(self) -> MenuCommand {
        MenuCommand(self as u32)
    }

    fn from_menu(command: MenuCommand) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.menu_command() == command)
    }
}

/// The text fields the window can show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    /// "Add a task" under the day plan's list.
    DayAdd,
    /// "Add a project" under the project plan's list.
    ProjectAdd,
    /// The selected task's name, in the project plan's details.
    Name,
    /// The task for a planner slot.
    Slot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Status {
    Info(String),
    Error(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum LoadState {
    Loading,
    Ready,
    /// The files could not be read; nothing is written over them.
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum DragKind {
    /// An entry being moved (keeping `grab` slots above the pointer) or
    /// resized from its bottom edge.
    Entry {
        id: Id,
        pane: PaneKind,
        grab: i64,
        resize: bool,
    },
    /// A task from a list, onto the planner, the hierarchy or the trash.
    Task { id: Id },
    /// A box in the hierarchy, onto another (to move it there), the
    /// project list (to make it a project) or the trash.
    Node { id: Id },
    /// The line between the planned and actual columns.
    Split,
    /// The left edge of the list beside the planner or hierarchy.
    ListWidth { grab: f32 },
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Drag {
    kind: DragKind,
    start: Point,
    active: bool,
}

#[derive(Debug, Clone, PartialEq)]
struct SlotEdit {
    pane: PaneKind,
    slot: u64,
    /// Matching tasks for what is typed, and the text they match.
    suggestions: Vec<(Id, String)>,
    matched: String,
    highlighted: Option<usize>,
}

struct DayState {
    date: NaiveDate,
    /// Pixels scrolled down from midnight.
    scroll: f32,
    split: f32,
    list_width: f32,
    list_scroll: f32,
    context: Option<Id>,
    context_open: bool,
    selected_entry: Option<Id>,
    selected_task: Option<Id>,
    slot_edit: Option<SlotEdit>,
}

struct ProjectState {
    root: Option<Id>,
    selected: Option<Id>,
    collapsed: HashSet<Id>,
    collapse_revision: u64,
    scroll_x: f32,
    scroll_y: f32,
    list_width: f32,
    list_scroll: f32,
    /// The hierarchy, kept until the plan, the project or a collapse changes.
    cache: Option<((u64, Id, u64), TreeLayout)>,
    /// The task whose name the details field shows.
    name_for: Option<Id>,
}

#[derive(Debug, Clone, Copy, Default)]
struct Layout {
    toolbar: Rect,
    tabs: [Rect; 2],
    new_task: Rect,
    save: Rect,
    trash: Rect,
    status: Rect,
    body: Rect,
    banner: Rect,
    prev_day: Rect,
    next_day: Rect,
    today: Rect,
    pane_header: Rect,
    gutter: Rect,
    /// Planned, then actual.
    panes: [Rect; 2],
    splitter: Rect,
    list: Rect,
    grip: Rect,
    context: Rect,
    rows: Rect,
    add_field: Rect,
    canvas: Rect,
    details: Rect,
    name_field: Rect,
    /// No mark, then 1 to 5.
    priorities: [Rect; 6],
    add_subtask: Rect,
    delete_task: Rect,
}

/// What the binary does after a frame.
#[derive(Debug, Clone, Default)]
pub struct FrameOutcome {
    /// Everything is saved and the app may close.
    pub quit: bool,
    /// The pointer is over text that can be edited.
    pub text_cursor: bool,
    /// The menus changed; show these.
    pub menu_bar: Option<MenuBar>,
}

pub struct TaskApp {
    plan: Plan,
    data_dir: Option<PathBuf>,
    config: BTreeMap<String, String>,
    config_dirty: bool,
    load: LoadState,
    saved_revision: u64,
    saving: Option<u64>,
    seen_revision: u64,
    save_due: Option<Instant>,
    save_now: bool,
    requests: Vec<IoRequest>,
    status: Status,
    tab: Tab,
    day: DayState,
    project: ProjectState,
    focus: Option<Field>,
    day_add: TextFieldModel,
    project_add: TextFieldModel,
    name_field: TextFieldModel,
    slot_field: TextFieldModel,
    drag: Option<Drag>,
    pointer: Point,
    size: (f32, f32),
    layout: Layout,
    native_menu: bool,
    drawn_menu: Option<MenuBarModel>,
    shown_menu: Option<MenuBar>,
    quitting: bool,
}

impl TaskApp {
    /// A window over the plan in `data_dir` (which it starts loading), or
    /// over an empty plan that is not kept when there is no folder.
    pub fn new(data_dir: Option<PathBuf>, owner: &str, today: NaiveDate) -> Self {
        let mut requests = Vec::new();
        let (load, status) = match &data_dir {
            Some(dir) => {
                requests.push(IoRequest::Load { dir: dir.clone() });
                (LoadState::Loading, Status::Info("Loading the plan…".into()))
            }
            None => (
                LoadState::Ready,
                Status::Error("No folder to keep the plan in; changes are not saved.".into()),
            ),
        };
        let field = |text: &str| {
            let mut field = TextFieldModel::new(text, Rect::default());
            field.area.style.font_size = draw::FONT;
            field.area.style.color = draw::TEXT;
            field.area.caret_color = draw::TEXT;
            field.area.selection_fill = draw::SELECTION;
            field.area.background = Some(draw::PANEL);
            field.area.border = Some(draw::BORDER);
            field.area.padding.left = 6.0;
            field.area.padding.right = 6.0;
            field
        };
        Self {
            plan: Plan::new(owner),
            data_dir,
            config: BTreeMap::new(),
            config_dirty: false,
            load,
            saved_revision: 0,
            saving: None,
            seen_revision: 0,
            save_due: None,
            save_now: false,
            requests,
            status,
            tab: Tab::Day,
            day: DayState {
                date: today,
                scroll: DEFAULT_FIRST_SLOT * SLOT_PX,
                split: DEFAULT_SPLIT,
                list_width: DEFAULT_LIST_WIDTH,
                list_scroll: 0.0,
                context: None,
                context_open: false,
                selected_entry: None,
                selected_task: None,
                slot_edit: None,
            },
            project: ProjectState {
                root: None,
                selected: None,
                collapsed: HashSet::new(),
                collapse_revision: 0,
                scroll_x: 0.0,
                scroll_y: 0.0,
                list_width: DEFAULT_LIST_WIDTH,
                list_scroll: 0.0,
                cache: None,
                name_for: None,
            },
            focus: None,
            day_add: field(""),
            project_add: field(""),
            name_field: field(""),
            slot_field: field(""),
            drag: None,
            pointer: Point::default(),
            size: (0.0, 0.0),
            layout: Layout::default(),
            native_menu: false,
            drawn_menu: None,
            shown_menu: None,
            quitting: false,
        }
    }

    /// Whether the host shows the menus itself (the macOS menu bar); when
    /// not, the window draws them.
    pub fn set_native_menu(&mut self, native: bool) {
        self.native_menu = native;
        self.drawn_menu = (!native).then(|| MenuBarModel::new(self.menu_bar()));
    }

    pub fn tab(&self) -> Tab {
        self.tab
    }

    pub fn plan(&self) -> &Plan {
        &self.plan
    }

    /// File work to start on the offload workers.
    pub fn take_requests(&mut self) -> Vec<IoRequest> {
        std::mem::take(&mut self.requests)
    }

    /// How long until the app needs a frame with no input: a save that is
    /// due. `None` when it can sleep until input comes.
    pub fn next_wake(&self, now: Instant) -> Option<Duration> {
        if self.saving.is_some() || !self.unsaved() {
            return None;
        }
        self.save_due.map(|due| {
            due.saturating_duration_since(now)
                .max(Duration::from_millis(1))
        })
    }

    /// A finished file job.
    pub fn apply(&mut self, response: IoResponse, now: Instant) {
        match response {
            IoResponse::Loaded {
                tasks,
                entries,
                config,
            } => self.loaded(tasks, entries, config),
            IoResponse::Saved { revision, result } => {
                self.saving = None;
                match result {
                    Ok(()) => {
                        self.saved_revision = self.saved_revision.max(revision);
                        let time = Local::now().format("%-I:%M %p");
                        self.status = Status::Info(format!("Saved at {time}."));
                    }
                    Err(error) => {
                        self.status = Status::Error(format!("Could not save: {error}"));
                        self.config_dirty = true;
                        self.save_due = Some(now + SAVE_RETRY);
                        // Do not close over an unsaved plan.
                        self.quitting = false;
                    }
                }
            }
        }
    }

    fn loaded(
        &mut self,
        tasks: Result<store::FileText, String>,
        entries: Result<store::FileText, String>,
        config: Result<store::FileText, String>,
    ) {
        let read = (|| -> Result<_, String> {
            let tasks = tasks?;
            let entries = entries?;
            let parsed = parse_plan(tasks.as_deref(), entries.as_deref())
                .map_err(|error| format!("{error:#}"))?;
            let config: BTreeMap<String, String> = match config? {
                Some(text) => serde_json::from_str(&text)
                    .map_err(|error| format!("the settings file: {error}"))?,
                None => BTreeMap::new(),
            };
            Ok((parsed, config))
        })();
        match read {
            Ok(((tasks, entries), config)) => {
                let count = tasks.len();
                self.plan.replace(tasks, entries);
                self.config.extend(config);
                self.apply_config();
                self.saved_revision = self.plan.revision();
                self.seen_revision = self.plan.revision();
                self.load = LoadState::Ready;
                self.status = Status::Info(match count {
                    0 => "A new plan. Add a task on the right, or click a time to plan one.".into(),
                    1 => "Opened the plan: 1 task.".into(),
                    n => format!("Opened the plan: {n} tasks."),
                });
            }
            Err(error) => {
                self.status = Status::Error(format!(
                    "Could not read the plan ({error}); it is left as it is and nothing will be saved."
                ));
                self.load = LoadState::Failed(error);
            }
        }
    }

    fn apply_config(&mut self) {
        let number = |key: &str| self.config.get(key).and_then(|v| v.parse::<f32>().ok());
        if let Some(width) = number("dayplanTaskWidth") {
            self.day.list_width = width.max(MIN_LIST_WIDTH);
        }
        if let Some(width) = number("projectTaskWidth") {
            self.project.list_width = width.max(MIN_LIST_WIDTH);
        }
        if let Some(percent) = number("dayplanSplitPercent") {
            self.day.split = (percent / 100.0).clamp(0.1, 0.9);
        }
    }

    fn store_config(&mut self) {
        let mut set = |key: &str, value: String| {
            if self.config.get(key) != Some(&value) {
                self.config.insert(key.to_string(), value);
                self.config_dirty = true;
            }
        };
        set(
            "dayplanTaskWidth",
            format!("{}", self.day.list_width.round()),
        );
        set(
            "projectTaskWidth",
            format!("{}", self.project.list_width.round()),
        );
        set(
            "dayplanSplitPercent",
            format!("{}", (self.day.split * 100.0).round()),
        );
    }

    fn editable(&self) -> bool {
        self.load == LoadState::Ready
    }

    fn unsaved(&self) -> bool {
        self.data_dir.is_some()
            && self.editable()
            && (self.plan.revision() != self.saved_revision || self.config_dirty)
    }

    /// One frame: input in, state updated. Paint with [`TaskApp::paint`].
    pub fn frame(
        &mut self,
        input: &InputSnapshot,
        size: (f32, f32),
        now: Instant,
        host: &dyn TaskHost,
    ) -> FrameOutcome {
        self.size = size;
        self.do_layout(host);

        for command in &input.menu_commands {
            if let Some(command) = Command::from_menu(*command) {
                self.run(command, host);
            }
        }
        self.route_input(input, host);
        self.do_layout(host);
        self.relayout_fields(host);

        if self.plan.revision() != self.seen_revision {
            self.seen_revision = self.plan.revision();
            self.save_due = Some(now + SAVE_DELAY);
        }
        self.maybe_save(now);

        let menu = self.menu_bar();
        let mut outcome = FrameOutcome::default();
        if self.shown_menu.as_ref() != Some(&menu) {
            if let Some(drawn) = &mut self.drawn_menu {
                drawn.set_menu_bar(menu.clone());
            }
            self.shown_menu = Some(menu.clone());
            outcome.menu_bar = Some(menu);
        }
        outcome.quit = self.quitting && self.saving.is_none() && !self.unsaved();
        outcome.text_cursor = self.text_cursor_at(self.pointer);
        outcome
    }

    fn maybe_save(&mut self, now: Instant) {
        if self.saving.is_some() || !self.unsaved() {
            self.save_now = false;
            return;
        }
        let due = self.save_now || self.save_due.is_none_or(|due| now >= due);
        if !due {
            return;
        }
        let Some(dir) = self.data_dir.clone() else {
            return;
        };
        self.save_now = false;
        let tasks = self.plan.tasks_json();
        let entries = self.plan.entries_json();
        let config = serde_json::to_string_pretty(&self.config);
        match (tasks, entries, config) {
            (Ok(tasks), Ok(entries), Ok(config)) => {
                let revision = self.plan.revision();
                self.requests.push(IoRequest::Save {
                    revision,
                    files: vec![
                        (store::tasks_path(&dir), tasks),
                        (store::entries_path(&dir), entries),
                        (store::config_path(&dir), config),
                    ],
                });
                self.saving = Some(revision);
                self.config_dirty = false;
            }
            (tasks, entries, config) => {
                let error = tasks
                    .err()
                    .or(entries.err())
                    .map(|e| e.to_string())
                    .or(config.err().map(|e| e.to_string()))
                    .unwrap_or_default();
                self.status = Status::Error(format!("Could not save: {error}"));
                self.save_due = Some(now + SAVE_RETRY);
                self.quitting = false;
            }
        }
    }

    // ----- menus and commands -------------------------------------------

    fn menu_bar(&self) -> MenuBar {
        let item = |command: Command, title: &str| MenuItem::command(command.menu_command(), title);
        let editing = self.focus.is_some();
        let mut file = vec![
            item(Command::NewTask, "New Task")
                .with_shortcut(Shortcut::primary('n'))
                .enabled(self.editable()),
            MenuItem::Separator,
            item(Command::Save, "Save Plan")
                .with_shortcut(Shortcut::primary('s'))
                .enabled(self.unsaved()),
        ];
        if !self.native_menu {
            file.push(MenuItem::Separator);
            file.push(item(Command::Quit, "Quit").with_shortcut(Shortcut::primary('q')));
        }
        let delete_enabled = !editing
            && self.editable()
            && match self.tab {
                Tab::Day => self.day.selected_entry.is_some(),
                Tab::Project => self.project.selected.is_some(),
            };
        let delete_title = match self.tab {
            Tab::Day => "Delete Entry",
            Tab::Project => "Delete Task",
        };
        let edit = vec![
            item(Command::Undo, "Undo")
                .with_shortcut(Shortcut::primary('z'))
                .enabled(!editing && self.editable() && self.plan.can_undo()),
            item(Command::Redo, "Redo")
                .with_shortcut(Shortcut::primary_shift('z'))
                .enabled(!editing && self.editable() && self.plan.can_redo()),
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
            item(Command::Delete, delete_title).enabled(delete_enabled),
        ];
        let on_day = self.tab == Tab::Day;
        let view = vec![
            item(Command::ShowDay, "Day Plan").enabled(!on_day),
            item(Command::ShowProject, "Project Plan").enabled(on_day),
            MenuItem::Separator,
            item(Command::PreviousDay, "Previous Day").enabled(on_day),
            item(Command::NextDay, "Next Day").enabled(on_day),
            item(Command::Today, "Today").enabled(on_day && self.day.date != clock::today()),
        ];
        MenuBar {
            menus: vec![
                ui_core::Menu::new("File", file),
                ui_core::Menu::new("Edit", edit),
                ui_core::Menu::new("View", view),
            ],
            quit: Some(Command::Quit.menu_command()),
        }
    }

    fn run(&mut self, command: Command, host: &dyn TaskHost) {
        match command {
            Command::NewTask => {
                if self.editable() {
                    let field = match self.tab {
                        Tab::Day => Field::DayAdd,
                        Tab::Project => Field::ProjectAdd,
                    };
                    self.set_focus(Some(field));
                }
            }
            Command::Save => self.save_now = true,
            Command::Quit => {
                self.set_focus(None);
                self.quitting = true;
                self.save_now = true;
            }
            Command::Undo => {
                if self.focus.is_none() && self.editable() && self.plan.undo() {
                    self.after_undo();
                }
            }
            Command::Redo => {
                if self.focus.is_none() && self.editable() && self.plan.redo() {
                    self.after_undo();
                }
            }
            Command::Cut | Command::Copy => {
                let cut = command == Command::Cut;
                if let Some(field) = self.focused_field() {
                    if let Some((start, end)) = field.area.selection_range() {
                        host.write_clipboard(&field.area.document.slice_chars(start, end));
                        if cut {
                            let _ = field.handle_event(UiEvent::KeyPressed {
                                key: Key::Backspace,
                                modifiers: Modifiers::default(),
                            });
                        }
                    }
                }
                self.after_field_edit();
            }
            Command::Paste => {
                if let Some(text) = host.read_clipboard() {
                    if let Some(field) = self.focused_field() {
                        let _ = field.handle_event(UiEvent::TextInput { text });
                    }
                }
                self.after_field_edit();
            }
            Command::SelectAll => {
                if let Some(field) = self.focused_field() {
                    field.select_all();
                }
            }
            Command::Delete => self.delete_selection(),
            Command::ShowDay => self.show_tab(Tab::Day),
            Command::ShowProject => self.show_tab(Tab::Project),
            Command::PreviousDay => self.change_day(-1),
            Command::NextDay => self.change_day(1),
            Command::Today => self.set_day(clock::today()),
        }
    }

    /// Selections that no longer exist after an undo or redo are dropped.
    fn after_undo(&mut self) {
        if self
            .day
            .selected_entry
            .is_some_and(|id| self.plan.entry(id).is_none())
        {
            self.day.selected_entry = None;
        }
        self.drop_missing_tasks();
    }

    fn drop_missing_tasks(&mut self) {
        let exists = |plan: &Plan, id: Option<Id>| id.filter(|id| plan.task(*id).is_some());
        self.day.selected_task = exists(&self.plan, self.day.selected_task);
        self.day.context = exists(&self.plan, self.day.context);
        self.project.selected = exists(&self.plan, self.project.selected);
        self.project.root = exists(&self.plan, self.project.root);
        if self.project.name_for.is_some() && self.project.selected != self.project.name_for {
            self.project.name_for = None;
        }
    }

    fn delete_selection(&mut self) {
        if !self.editable() || self.focus.is_some() {
            return;
        }
        match self.tab {
            Tab::Day => {
                if let Some(id) = self.day.selected_entry.take() {
                    self.plan.delete_entry(id);
                }
            }
            Tab::Project => {
                if let Some(id) = self.project.selected {
                    self.delete_task(id);
                }
            }
        }
    }

    fn delete_task(&mut self, id: Id) {
        let name = self
            .plan
            .task(id)
            .map(|t| t.name.clone())
            .unwrap_or_default();
        if self.plan.delete_task(id) {
            self.status = Status::Info(format!(
                "Deleted \u{201c}{name}\u{201d}. Undo brings it back."
            ));
            self.drop_missing_tasks();
        }
    }

    fn show_tab(&mut self, tab: Tab) {
        if self.tab != tab {
            self.set_focus(None);
            self.day.context_open = false;
            self.tab = tab;
        }
    }

    fn change_day(&mut self, days: i64) {
        if let Some(date) = self
            .day
            .date
            .checked_add_signed(chrono::TimeDelta::days(days))
        {
            self.set_day(date);
        }
    }

    fn set_day(&mut self, date: NaiveDate) {
        if self.tab == Tab::Day && self.day.date != date {
            self.close_slot_edit();
            self.day.selected_entry = None;
            self.day.date = date;
        }
    }

    // ----- focus and fields -----------------------------------------------

    fn field_mut(&mut self, field: Field) -> &mut TextFieldModel {
        match field {
            Field::DayAdd => &mut self.day_add,
            Field::ProjectAdd => &mut self.project_add,
            Field::Name => &mut self.name_field,
            Field::Slot => &mut self.slot_field,
        }
    }

    fn focused_field(&mut self) -> Option<&mut TextFieldModel> {
        let field = self.focus?;
        Some(self.field_mut(field))
    }

    /// Moves focus, committing a name being edited and closing a slot
    /// editor that loses it.
    fn set_focus(&mut self, next: Option<Field>) {
        if self.focus == next {
            return;
        }
        if let Some(previous) = self.focus.take() {
            let _ = self
                .field_mut(previous)
                .handle_event(UiEvent::FocusChanged(false));
            match previous {
                Field::Name => self.commit_name(),
                Field::Slot => self.day.slot_edit = None,
                Field::DayAdd | Field::ProjectAdd => {}
            }
        }
        if let Some(field) = next {
            let model = self.field_mut(field);
            let _ = model.handle_event(UiEvent::FocusChanged(true));
            model.area.show_caret = true;
            self.focus = Some(field);
        }
    }

    fn close_slot_edit(&mut self) {
        if self.focus == Some(Field::Slot) {
            self.set_focus(None);
        }
        self.day.slot_edit = None;
    }

    fn field_rect(&self, field: Field) -> Option<Rect> {
        match (field, self.tab) {
            (Field::DayAdd, Tab::Day) => Some(self.layout.add_field),
            (Field::ProjectAdd, Tab::Project) => Some(self.layout.add_field),
            (Field::Name, Tab::Project) => self.project.selected.map(|_| self.layout.name_field),
            (Field::Slot, Tab::Day) => self.slot_edit_rect(),
            _ => None,
        }
    }

    fn slot_edit_rect(&self) -> Option<Rect> {
        let edit = self.day.slot_edit.as_ref()?;
        let pane = self.pane_rect(edit.pane);
        let y = pane.y + edit.slot as f32 * SLOT_PX - self.day.scroll;
        Some(rect(pane.x + 2.0, y, pane.width - 4.0, SLOT_PX.max(24.0)))
    }

    fn relayout_fields(&mut self, host: &dyn TaskHost) {
        for field in [Field::DayAdd, Field::ProjectAdd, Field::Name, Field::Slot] {
            if let Some(bounds) = self.field_rect(field) {
                let model = self.field_mut(field);
                model.set_bounds(bounds);
                model.relayout(|text, size| host.measure(text, size));
            }
        }
    }

    fn text_cursor_at(&self, point: Point) -> bool {
        [Field::DayAdd, Field::ProjectAdd, Field::Name, Field::Slot]
            .into_iter()
            .any(|field| self.field_rect(field).is_some_and(|r| r.contains(point)))
    }

    /// After typing or pasting into a field: the slot editor's suggestions
    /// follow the text.
    fn after_field_edit(&mut self) {
        if self.focus != Some(Field::Slot) {
            return;
        }
        let text = self.slot_field.text();
        if let Some(edit) = &mut self.day.slot_edit {
            if edit.matched != text {
                edit.suggestions = if text.trim().is_empty() {
                    Vec::new()
                } else {
                    self.plan.names_matching(&text, MAX_SUGGESTIONS)
                };
                edit.highlighted = None;
                edit.matched = text;
            }
        }
    }

    /// Enter in a field.
    fn activate(&mut self, field: Field, modifiers: Modifiers) {
        let _ = modifiers;
        match field {
            Field::DayAdd | Field::ProjectAdd => {
                let name = self.field_mut(field).text();
                if name.trim().is_empty() || !self.editable() {
                    return;
                }
                self.field_mut(field).set_text("");
                if field == Field::DayAdd {
                    let id = self.plan.add_task(&name, self.day.context);
                    self.day.selected_task = Some(id);
                } else {
                    let id = self.plan.add_task(&name, None);
                    self.project.root = Some(id);
                    self.select_project_task(Some(id));
                }
            }
            Field::Name => {
                self.commit_name();
                self.set_focus(None);
            }
            Field::Slot => {
                let chosen = self
                    .day
                    .slot_edit
                    .as_ref()
                    .and_then(|edit| edit.highlighted.and_then(|i| edit.suggestions.get(i)))
                    .map(|(id, _)| *id);
                match chosen {
                    Some(id) => self.commit_slot(Some(id)),
                    None => self.commit_slot(None),
                }
            }
        }
    }

    fn commit_name(&mut self) {
        let Some(id) = self.project.name_for else {
            return;
        };
        let name = self.name_field.text();
        if name.trim().is_empty() {
            // An empty name is not kept; show the old one again.
            let old = self
                .plan
                .task(id)
                .map(|t| t.name.clone())
                .unwrap_or_default();
            self.name_field.set_text(&old);
        } else if self.editable() {
            self.plan.rename_task(id, &name);
        }
    }

    /// Plans the slot editor's task (the chosen one, or the one its text
    /// names, made if there is none) in its slot.
    fn commit_slot(&mut self, chosen: Option<Id>) {
        let Some(edit) = self.day.slot_edit.clone() else {
            return;
        };
        let text = self.slot_field.text();
        let task = match chosen {
            Some(id) => Some(id),
            None if text.trim().is_empty() => None,
            None => Some(
                self.plan
                    .find_task(&text)
                    .unwrap_or_else(|| self.plan.add_task(&text, None)),
            ),
        };
        if let (Some(task), true) = (task, self.editable()) {
            let start = clock::day_start(self.day.date) + edit.slot * TICKS_PER_SLOT;
            let id =
                self.plan
                    .add_entry(task, edit.pane.entry_kind(), start, start + TICKS_PER_SLOT);
            self.day.selected_entry = Some(id);
        }
        self.close_slot_edit();
    }

    fn open_slot_edit(&mut self, pane: PaneKind, slot: u64) {
        self.slot_field.set_text("");
        self.day.slot_edit = Some(SlotEdit {
            pane,
            slot,
            suggestions: Vec::new(),
            matched: String::new(),
            highlighted: None,
        });
        self.day.selected_entry = None;
        self.focus = None;
        self.set_focus(Some(Field::Slot));
    }

    fn select_project_task(&mut self, id: Option<Id>) {
        if self.project.selected == id && self.project.name_for == id {
            return;
        }
        if self.focus == Some(Field::Name) {
            self.set_focus(None);
        }
        self.project.selected = id;
        self.project.name_for = id;
        let name = id
            .and_then(|id| self.plan.task(id))
            .map(|t| t.name.clone())
            .unwrap_or_default();
        self.name_field.set_text(&name);
    }

    // ----- layout -------------------------------------------------------

    fn do_layout(&mut self, host: &dyn TaskHost) {
        let (width, height) = self.size;
        let menu_height = if let Some(menu) = &mut self.drawn_menu {
            menu.relayout(rect(0.0, 0.0, width, height), |text, size| {
                host.measure(text, size)
            });
            MenuBarModel::height()
        } else {
            0.0
        };
        let mut l = Layout {
            toolbar: rect(0.0, menu_height, width, TOOLBAR_HEIGHT),
            status: rect(0.0, height - STATUS_HEIGHT, width, STATUS_HEIGHT),
            ..Layout::default()
        };
        let bar = l.toolbar;
        let tab_width = |label: &str| host.measure(label, draw::FONT) + 32.0;
        let day_width = tab_width("Day Plan");
        l.tabs[0] = rect(MARGIN, bar.y + 6.0, day_width, bar.height - 6.0);
        l.tabs[1] = rect(
            l.tabs[0].right() + 4.0,
            bar.y + 6.0,
            tab_width("Project Plan"),
            bar.height - 6.0,
        );
        l.trash = rect(width - MARGIN - 40.0, bar.y + 4.0, 40.0, bar.height - 8.0);
        let save_width = host.measure("Save", draw::FONT) + 24.0;
        l.save = rect(
            l.trash.x - MARGIN - save_width,
            bar.y + 6.0,
            save_width,
            28.0,
        );
        let new_width = host.measure("New Task", draw::FONT) + 24.0;
        l.new_task = rect(l.save.x - 6.0 - new_width, bar.y + 6.0, new_width, 28.0);
        l.body = rect(0.0, bar.bottom(), width, l.status.y - bar.bottom());

        let body = l.body;
        let list_width = match self.tab {
            Tab::Day => &mut self.day.list_width,
            Tab::Project => &mut self.project.list_width,
        };
        let max_list = (width / 3.0).max(MIN_LIST_WIDTH);
        *list_width = list_width.clamp(MIN_LIST_WIDTH, max_list);
        let list_width = *list_width;
        l.list = rect(width - list_width, body.y, list_width, body.height);
        l.grip = rect(l.list.x, body.y, GRIP_WIDTH, body.height);
        let inner_x = l.list.x + GRIP_WIDTH + 4.0;
        let inner_width = l.list.right() - MARGIN - inner_x;
        let left_width = l.list.x;

        match self.tab {
            Tab::Day => {
                l.context = rect(inner_x, body.y + MARGIN, inner_width, FIELD_HEIGHT);
                l.add_field = rect(
                    inner_x,
                    body.bottom() - MARGIN - FIELD_HEIGHT,
                    inner_width,
                    FIELD_HEIGHT,
                );
                l.rows = rect(
                    inner_x,
                    l.context.bottom() + MARGIN,
                    inner_width,
                    l.add_field.y - MARGIN - (l.context.bottom() + MARGIN),
                );
                l.banner = rect(0.0, body.y, left_width, BANNER_HEIGHT);
                l.prev_day = rect(MARGIN, body.y + 6.0, 32.0, BANNER_HEIGHT - 12.0);
                l.next_day = rect(
                    left_width - MARGIN - 32.0,
                    body.y + 6.0,
                    32.0,
                    BANNER_HEIGHT - 12.0,
                );
                if self.day.date != clock::today() {
                    let w = host.measure("Today", draw::FONT) + 24.0;
                    l.today = rect(
                        l.next_day.x - 6.0 - w,
                        body.y + 6.0,
                        w,
                        BANNER_HEIGHT - 12.0,
                    );
                }
                l.pane_header = rect(0.0, l.banner.bottom(), left_width, PANE_HEADER_HEIGHT);
                let grid_top = l.pane_header.bottom();
                let grid_height = body.bottom() - grid_top;
                l.gutter = rect(0.0, grid_top, GUTTER_WIDTH, grid_height);
                let panes_width = (left_width - GUTTER_WIDTH - SPLITTER_WIDTH).max(0.0);
                let planned = (panes_width * self.day.split).clamp(
                    MIN_PANE_WIDTH.min(panes_width),
                    (panes_width - MIN_PANE_WIDTH).max(0.0),
                );
                l.panes[0] = rect(GUTTER_WIDTH, grid_top, planned, grid_height);
                l.splitter = rect(l.panes[0].right(), grid_top, SPLITTER_WIDTH, grid_height);
                l.panes[1] = rect(
                    l.splitter.right(),
                    grid_top,
                    left_width - l.splitter.right(),
                    grid_height,
                );
                let max_scroll = (SLOTS_PER_DAY as f32 * SLOT_PX - grid_height).max(0.0);
                self.day.scroll = self.day.scroll.clamp(0.0, max_scroll);
            }
            Tab::Project => {
                l.details = rect(
                    inner_x,
                    body.bottom() - MARGIN - DETAILS_HEIGHT,
                    inner_width,
                    DETAILS_HEIGHT,
                );
                l.add_field = rect(
                    inner_x,
                    l.details.y - MARGIN - FIELD_HEIGHT,
                    inner_width,
                    FIELD_HEIGHT,
                );
                l.rows = rect(
                    inner_x,
                    body.y + MARGIN + 20.0,
                    inner_width,
                    l.add_field.y - MARGIN - (body.y + MARGIN + 20.0),
                );
                let d = l.details;
                l.name_field = rect(d.x + 8.0, d.y + 26.0, d.width - 16.0, FIELD_HEIGHT);
                let marks_y = l.name_field.bottom() + 26.0;
                l.priorities[0] = rect(d.x + 8.0, marks_y, 48.0, 22.0);
                for i in 1..=MAX_PRIORITY as usize {
                    l.priorities[i] = rect(
                        l.priorities[0].right() + 4.0 + (i - 1) as f32 * 26.0,
                        marks_y,
                        22.0,
                        22.0,
                    );
                }
                let buttons_y = d.bottom() - 8.0 - 28.0;
                let half = (d.width - 24.0) / 2.0;
                l.add_subtask = rect(d.x + 8.0, buttons_y, half, 28.0);
                l.delete_task = rect(l.add_subtask.right() + 8.0, buttons_y, half, 28.0);
                l.canvas = rect(0.0, body.y, left_width, body.height);
            }
        }
        self.layout = l;
    }

    fn pane_rect(&self, pane: PaneKind) -> Rect {
        match pane {
            PaneKind::Planned => self.layout.panes[0],
            PaneKind::Actual => self.layout.panes[1],
        }
    }

    fn pane_at(&self, point: Point) -> Option<PaneKind> {
        [PaneKind::Planned, PaneKind::Actual]
            .into_iter()
            .find(|pane| self.pane_rect(*pane).contains(point))
    }

    /// The slot of the day at `y`, clamped to the day.
    fn slot_at(&self, y: f32) -> u64 {
        let top = self.layout.panes[0].y;
        let slot = ((y - top + self.day.scroll) / SLOT_PX).floor();
        slot.clamp(0.0, (SLOTS_PER_DAY - 1) as f32) as u64
    }

    /// An entry's rectangle in its pane, from its first to its last slot of
    /// the day shown.
    fn entry_rect(&self, pane: Rect, start: u64, stop: u64) -> Rect {
        let day = clock::day_start(self.day.date);
        let first = start.saturating_sub(day) / TICKS_PER_SLOT;
        let last = stop
            .saturating_sub(day)
            .div_ceil(TICKS_PER_SLOT)
            .clamp(first + 1, SLOTS_PER_DAY);
        rect(
            pane.x + 3.0,
            pane.y + first as f32 * SLOT_PX - self.day.scroll + 1.0,
            pane.width - 6.0,
            (last - first) as f32 * SLOT_PX - 2.0,
        )
    }

    /// The entry under `point`, and whether the point is on its bottom edge.
    fn entry_at(&self, point: Point) -> Option<(Id, PaneKind, bool)> {
        let pane = self.pane_at(point)?;
        let pane_rect = self.pane_rect(pane);
        let day = clock::day_start(self.day.date);
        self.plan
            .entries_on(day, pane)
            .into_iter()
            .rev()
            .find_map(|entry| {
                let r = self.entry_rect(pane_rect, entry.start, entry.stop);
                r.contains(point)
                    .then(|| (entry.entity.id, pane, point.y >= r.bottom() - RESIZE_EDGE))
            })
    }

    fn day_rows(&self) -> Vec<(Id, String, i32)> {
        self.plan
            .leaves(self.day.context)
            .into_iter()
            .map(|t| (t.entity.id, t.name.clone(), t.priority))
            .collect()
    }

    fn project_rows(&self) -> Vec<(Id, String, i32)> {
        self.plan
            .roots()
            .into_iter()
            .map(|t| (t.entity.id, t.name.clone(), t.priority))
            .collect()
    }

    fn list_rows(&self) -> Vec<(Id, String, i32)> {
        match self.tab {
            Tab::Day => self.day_rows(),
            Tab::Project => self.project_rows(),
        }
    }

    fn list_scroll(&self) -> f32 {
        match self.tab {
            Tab::Day => self.day.list_scroll,
            Tab::Project => self.project.list_scroll,
        }
    }

    fn row_at(&self, point: Point) -> Option<Id> {
        let rows = self.layout.rows;
        if !rows.contains(point) {
            return None;
        }
        let index = ((point.y - rows.y + self.list_scroll()) / ROW_HEIGHT).floor() as usize;
        self.list_rows().get(index).map(|(id, _, _)| *id)
    }

    /// The context menu's rows (all tasks, then each project) below the
    /// context button.
    fn context_options(&self) -> Vec<(Option<Id>, String, Rect)> {
        let c = self.layout.context;
        let mut options = vec![(None, "All Tasks".to_string())];
        options.extend(
            self.plan
                .roots()
                .into_iter()
                .map(|t| (Some(t.entity.id), t.name.clone())),
        );
        let room = ((self.layout.body.bottom() - c.bottom()) / ROW_HEIGHT)
            .floor()
            .max(1.0) as usize;
        options
            .into_iter()
            .take(room)
            .enumerate()
            .map(|(i, (id, name))| {
                (
                    id,
                    name,
                    rect(c.x, c.bottom() + i as f32 * ROW_HEIGHT, c.width, ROW_HEIGHT),
                )
            })
            .collect()
    }

    fn suggestion_rects(&self) -> Vec<(Id, Rect)> {
        let (Some(edit), Some(field)) = (&self.day.slot_edit, self.slot_edit_rect()) else {
            return Vec::new();
        };
        edit.suggestions
            .iter()
            .enumerate()
            .map(|(i, (id, _))| {
                (
                    *id,
                    rect(
                        field.x,
                        field.bottom() + i as f32 * SUGGESTION_HEIGHT,
                        field.width,
                        SUGGESTION_HEIGHT,
                    ),
                )
            })
            .collect()
    }

    fn tree(&mut self, host: &dyn TaskHost) -> Option<(Id, TreeLayout)> {
        let root = self
            .project
            .root
            .filter(|id| self.plan.task(*id).is_some())
            .or_else(|| self.plan.roots().first().map(|t| t.entity.id))?;
        let key = (self.plan.revision(), root, self.project.collapse_revision);
        if self
            .project
            .cache
            .as_ref()
            .is_none_or(|(cached, _)| *cached != key)
        {
            let mut measure = |text: &str| host.measure(text, draw::FONT);
            let layout =
                tree_layout::layout(&self.plan, root, &self.project.collapsed, &mut measure);
            self.project.cache = Some((key, layout));
        }
        self.project
            .cache
            .as_ref()
            .map(|(_, layout)| (root, layout.clone()))
    }

    /// Where the hierarchy's top left is drawn: centred when it fits.
    fn tree_origin(&self, layout: &TreeLayout) -> Point {
        let canvas = self.layout.canvas;
        let x = if layout.width + 32.0 <= canvas.width {
            canvas.x + (canvas.width - layout.width) / 2.0
        } else {
            canvas.x + 16.0 - self.project.scroll_x
        };
        Point {
            x,
            y: canvas.y + 20.0 - self.project.scroll_y,
        }
    }

    fn node_rect(origin: Point, node: &tree_layout::NodeBox) -> Rect {
        Rect {
            x: origin.x + node.rect.x,
            y: origin.y + node.rect.y,
            ..node.rect
        }
    }

    /// The collapse button under a box with subtasks.
    fn toggle_rect(node: Rect) -> Rect {
        rect(
            node.x + node.width / 2.0 - 7.0,
            node.bottom() - 7.0,
            14.0,
            14.0,
        )
    }

    /// The box under `point`, and whether the point is on its collapse
    /// button.
    fn node_at(&mut self, point: Point, host: &dyn TaskHost) -> Option<(Id, bool)> {
        if !self.layout.canvas.contains(point) {
            return None;
        }
        let (_, layout) = self.tree(host)?;
        let origin = self.tree_origin(&layout);
        layout.nodes.iter().rev().find_map(|node| {
            let r = Self::node_rect(origin, node);
            if node.has_children && Self::toggle_rect(r).contains(point) {
                Some((node.id, true))
            } else {
                r.contains(point).then_some((node.id, false))
            }
        })
    }

    // ----- input --------------------------------------------------------

    fn route_input(&mut self, input: &InputSnapshot, host: &dyn TaskHost) {
        let point = Point {
            x: input.mouse_x,
            y: input.mouse_y,
        };
        let moved = point != self.pointer;
        self.pointer = point;
        let state = PointerState::mouse(point, input.modifiers);
        let primary = input.mouse_button == PointerButton::Primary;

        if let Some(menu) = &mut self.drawn_menu {
            let mut events = vec![UiEvent::PointerMoved(state)];
            if input.mouse_pressed {
                events.push(UiEvent::PointerPressed {
                    button: input.mouse_button,
                    state,
                });
            }
            if input.mouse_released {
                events.push(UiEvent::PointerReleased {
                    button: input.mouse_button,
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
            let mut consumed = false;
            let mut commands = Vec::new();
            for event in &events {
                let response = menu.handle_event(event);
                consumed |= response.consumed && !matches!(event, UiEvent::PointerMoved(_));
                commands.extend(response.command.and_then(Command::from_menu));
            }
            for command in commands {
                self.run(command, host);
            }
            if consumed || menu_is_open(&self.drawn_menu) {
                return;
            }
        }

        if moved || input.mouse_down {
            self.pointer_moved(state, input.mouse_down);
        }
        if input.mouse_pressed && primary {
            self.pointer_pressed(state, host);
        }
        if input.mouse_released && primary {
            self.pointer_released(state, host);
        }
        if input.mouse_wheel_x != 0.0 || input.mouse_wheel_y != 0.0 {
            self.wheel(point, input);
        }
        for event in &input.key_events {
            self.key(event.key, event.modifiers, host);
        }
        if !input.typed_text.is_empty() {
            if let Some(field) = self.focused_field() {
                let _ = field.handle_event(UiEvent::TextInput {
                    text: input.typed_text.clone(),
                });
            }
            self.after_field_edit();
        }
    }

    fn pointer_moved(&mut self, state: PointerState, down: bool) {
        if let Some(field) = self.focused_field() {
            let _ = field.handle_event(UiEvent::PointerMoved(state));
        }
        let point = state.position;
        let Some(mut drag) = self.drag else {
            let hovered = self
                .suggestion_rects()
                .iter()
                .position(|(_, r)| r.contains(point));
            if let (Some(edit), Some(_)) = (&mut self.day.slot_edit, hovered) {
                edit.highlighted = hovered;
            }
            return;
        };
        if !down {
            return;
        }
        if !drag.active {
            let dx = point.x - drag.start.x;
            let dy = point.y - drag.start.y;
            if dx * dx + dy * dy < DRAG_THRESHOLD * DRAG_THRESHOLD {
                return;
            }
            drag.active = true;
            if let DragKind::Entry { .. } = drag.kind {
                // One undo step for the whole move.
                self.plan.checkpoint();
            }
        }
        self.drag = Some(drag);
        match drag.kind {
            DragKind::Entry {
                id,
                pane,
                grab,
                resize,
            } => self.drag_entry(id, pane, grab, resize, point),
            DragKind::Split => {
                let panes = self.layout.panes[0].x;
                let width = self.layout.panes[1].right() - panes - SPLITTER_WIDTH;
                if width > 0.0 {
                    self.day.split = ((point.x - panes) / width).clamp(0.1, 0.9);
                }
            }
            DragKind::ListWidth { grab } => {
                let width = self.size.0 - (point.x - grab);
                match self.tab {
                    Tab::Day => self.day.list_width = width,
                    Tab::Project => self.project.list_width = width,
                }
            }
            DragKind::Task { .. } | DragKind::Node { .. } => {}
        }
    }

    fn drag_entry(&mut self, id: Id, pane: PaneKind, grab: i64, resize: bool, point: Point) {
        let Some(entry) = self.plan.entry(id) else {
            return;
        };
        let _ = pane;
        let day = clock::day_start(self.day.date);
        let (start, stop) = (entry.start, entry.stop);
        let slot = self.slot_at(point.y);
        if resize {
            let first = start.saturating_sub(day) / TICKS_PER_SLOT;
            let last = (slot + 1).max(first + 1).min(SLOTS_PER_DAY);
            let new_stop = day + last * TICKS_PER_SLOT;
            self.plan
                .set_entry_time_live(id, start, new_stop.max(start + TICKS_PER_SLOT));
        } else {
            let length = stop.saturating_sub(start).div_ceil(TICKS_PER_SLOT).max(1);
            let first = (slot as i64 - grab)
                .clamp(0, (SLOTS_PER_DAY - length.min(SLOTS_PER_DAY)) as i64)
                as u64;
            let new_start = day + first * TICKS_PER_SLOT;
            self.plan
                .set_entry_time_live(id, new_start, new_start + length * TICKS_PER_SLOT);
        }
    }

    fn pointer_pressed(&mut self, state: PointerState, host: &dyn TaskHost) {
        let point = state.position;
        let l = self.layout;

        // An open context menu takes the press.
        if self.day.context_open {
            self.day.context_open = false;
            if let Some((id, _, _)) = self
                .context_options()
                .into_iter()
                .find(|(_, _, r)| r.contains(point))
            {
                self.day.context = id;
                self.day.list_scroll = 0.0;
            }
            return;
        }
        // A slot editor's suggestion.
        if let Some((id, _)) = self
            .suggestion_rects()
            .into_iter()
            .find(|(_, r)| r.contains(point))
        {
            self.commit_slot(Some(id));
            return;
        }
        // A field.
        if let Some(field) = [Field::DayAdd, Field::ProjectAdd, Field::Name, Field::Slot]
            .into_iter()
            .find(|f| self.field_rect(*f).is_some_and(|r| r.contains(point)))
        {
            if self.editable() || field == Field::Slot {
                self.set_focus(Some(field));
                let _ = self.field_mut(field).handle_event(UiEvent::PointerPressed {
                    button: PointerButton::Primary,
                    state,
                });
            }
            return;
        }
        self.set_focus(None);

        // The toolbar.
        if l.tabs[0].contains(point) {
            return self.show_tab(Tab::Day);
        }
        if l.tabs[1].contains(point) {
            return self.show_tab(Tab::Project);
        }
        if l.new_task.contains(point) {
            return self.run(Command::NewTask, host);
        }
        if l.save.contains(point) {
            return self.run(Command::Save, host);
        }
        if !l.body.contains(point) {
            return;
        }
        if l.grip.contains(point) {
            self.drag = Some(Drag {
                kind: DragKind::ListWidth {
                    grab: point.x - l.list.x,
                },
                start: point,
                active: true,
            });
            return;
        }
        if l.list.contains(point) {
            if self.tab == Tab::Day && l.context.contains(point) {
                self.day.context_open = true;
                return;
            }
            if let Some(id) = self.row_at(point) {
                match self.tab {
                    Tab::Day => self.day.selected_task = Some(id),
                    Tab::Project => {
                        self.project.root = Some(id);
                        self.project.scroll_x = 0.0;
                        self.project.scroll_y = 0.0;
                        self.select_project_task(Some(id));
                    }
                }
                if self.editable() {
                    self.drag = Some(Drag {
                        kind: DragKind::Task { id },
                        start: point,
                        active: false,
                    });
                }
                return;
            }
            if self.tab == Tab::Project && self.editable() {
                self.press_details(point);
            }
            return;
        }
        match self.tab {
            Tab::Day => self.press_day(point),
            Tab::Project => self.press_canvas(point, host),
        }
    }

    fn press_day(&mut self, point: Point) {
        let l = self.layout;
        if l.prev_day.contains(point) {
            return self.change_day(-1);
        }
        if l.next_day.contains(point) {
            return self.change_day(1);
        }
        if l.today.contains(point) {
            return self.set_day(clock::today());
        }
        if l.splitter.contains(point) {
            self.drag = Some(Drag {
                kind: DragKind::Split,
                start: point,
                active: true,
            });
            return;
        }
        let Some(pane) = self.pane_at(point) else {
            return;
        };
        if let Some((id, pane, resize)) = self.entry_at(point) {
            self.day.selected_entry = Some(id);
            if !self.editable() {
                return;
            }
            let day = clock::day_start(self.day.date);
            let first = self
                .plan
                .entry(id)
                .map(|e| e.start.saturating_sub(day) / TICKS_PER_SLOT)
                .unwrap_or(0);
            let grab = self.slot_at(point.y) as i64 - first as i64;
            self.drag = Some(Drag {
                kind: DragKind::Entry {
                    id,
                    pane,
                    grab,
                    resize,
                },
                start: point,
                active: false,
            });
            return;
        }
        self.day.selected_entry = None;
        if self.editable() {
            let slot = self.slot_at(point.y);
            self.open_slot_edit(pane, slot);
        }
    }

    fn press_canvas(&mut self, point: Point, host: &dyn TaskHost) {
        match self.node_at(point, host) {
            Some((id, true)) => {
                if !self.project.collapsed.remove(&id) {
                    self.project.collapsed.insert(id);
                }
                self.project.collapse_revision += 1;
            }
            Some((id, false)) => {
                self.select_project_task(Some(id));
                if self.editable() {
                    self.drag = Some(Drag {
                        kind: DragKind::Node { id },
                        start: point,
                        active: false,
                    });
                }
            }
            None => self.select_project_task(None),
        }
    }

    fn press_details(&mut self, point: Point) {
        let Some(id) = self.project.selected else {
            return;
        };
        let l = self.layout;
        if let Some(priority) = l.priorities.iter().position(|r| r.contains(point)) {
            self.plan.set_priority(id, priority as i32);
        } else if l.add_subtask.contains(point) {
            let child = self.plan.add_task("New Task", Some(id));
            self.project.collapsed.remove(&id);
            self.project.collapse_revision += 1;
            self.select_project_task(Some(child));
            self.set_focus(Some(Field::Name));
            self.name_field.select_all();
        } else if l.delete_task.contains(point) {
            self.delete_task(id);
        }
    }

    fn pointer_released(&mut self, state: PointerState, host: &dyn TaskHost) {
        let point = state.position;
        if let Some(field) = self.focused_field() {
            let _ = field.handle_event(UiEvent::PointerReleased {
                button: PointerButton::Primary,
                state,
            });
        }
        let Some(drag) = self.drag.take() else {
            return;
        };
        let on_trash = self.layout.trash.contains(point);
        match drag.kind {
            DragKind::Split | DragKind::ListWidth { .. } => self.store_config(),
            DragKind::Entry { id, .. } if drag.active && on_trash => {
                // Put the entry back where the drag found it, then delete it:
                // one undo brings it back.
                self.plan.undo();
                self.plan.delete_entry(id);
                self.day.selected_entry = None;
            }
            DragKind::Entry { .. } => {}
            DragKind::Task { id } | DragKind::Node { id } if drag.active => {
                self.drop_task(id, point, on_trash, host)
            }
            DragKind::Task { .. } | DragKind::Node { .. } => {}
        }
    }

    /// A task let go of at `point`.
    fn drop_task(&mut self, id: Id, point: Point, on_trash: bool, host: &dyn TaskHost) {
        if on_trash {
            return self.delete_task(id);
        }
        match self.tab {
            Tab::Day => {
                if let Some(pane) = self.pane_at(point) {
                    let start =
                        clock::day_start(self.day.date) + self.slot_at(point.y) * TICKS_PER_SLOT;
                    let entry =
                        self.plan
                            .add_entry(id, pane.entry_kind(), start, start + TICKS_PER_SLOT);
                    self.day.selected_entry = Some(entry);
                }
            }
            Tab::Project => {
                if let Some((target, _)) = self.node_at(point, host) {
                    if target != id {
                        if self.plan.set_parent(id, Some(target)) {
                            self.project.collapsed.remove(&target);
                            self.project.collapse_revision += 1;
                        } else {
                            self.status = Status::Error(
                                "A task cannot go under itself or one of its own subtasks.".into(),
                            );
                        }
                    }
                } else if self.layout.list.contains(point) && self.plan.set_parent(id, None) {
                    self.project.root = Some(id);
                }
            }
        }
    }

    fn wheel(&mut self, point: Point, input: &InputSnapshot) {
        let step = |delta: f32| {
            if input.mouse_wheel_precise {
                -delta
            } else {
                -delta * ROW_HEIGHT * 2.0
            }
        };
        let dy = step(input.mouse_wheel_y);
        let dx = step(input.mouse_wheel_x);
        let l = self.layout;
        if l.rows.contains(point) {
            let rows = self.list_rows().len() as f32 * ROW_HEIGHT;
            let max = (rows - l.rows.height).max(0.0);
            let scroll = match self.tab {
                Tab::Day => &mut self.day.list_scroll,
                Tab::Project => &mut self.project.list_scroll,
            };
            *scroll = (*scroll + dy).clamp(0.0, max);
            return;
        }
        match self.tab {
            Tab::Day => {
                if l.gutter.contains(point) || self.pane_at(point).is_some() {
                    // Clamped to the day in layout.
                    self.day.scroll = (self.day.scroll + dy).max(0.0);
                }
            }
            Tab::Project => {
                if l.canvas.contains(point) {
                    let (width, height) = self
                        .project
                        .cache
                        .as_ref()
                        .map(|(_, t)| (t.width, t.height))
                        .unwrap_or((0.0, 0.0));
                    let max_x = (width + 32.0 - l.canvas.width).max(0.0);
                    let max_y = (height + 40.0 - l.canvas.height).max(0.0);
                    let (dx, dy) = if input.modifiers.shift && dx == 0.0 {
                        (dy, 0.0)
                    } else {
                        (dx, dy)
                    };
                    self.project.scroll_x = (self.project.scroll_x + dx).clamp(0.0, max_x);
                    self.project.scroll_y = (self.project.scroll_y + dy).clamp(0.0, max_y);
                }
            }
        }
    }

    fn key(&mut self, key: HostKey, modifiers: Modifiers, host: &dyn TaskHost) {
        let Some(ui_key) = key.ui_key() else {
            return;
        };
        if ui_key == Key::Escape {
            if self.drag.is_some_and(|d| d.active) {
                if let Some(Drag {
                    kind: DragKind::Entry { .. },
                    ..
                }) = self.drag
                {
                    self.plan.undo();
                }
                self.drag = None;
            } else if self.day.context_open {
                self.day.context_open = false;
            } else if self.focus == Some(Field::Name) {
                // Leave the name as it was.
                self.project.name_for = None;
                self.set_focus(None);
                let selected = self.project.selected.take();
                self.select_project_task(selected);
            } else {
                self.close_slot_edit();
                self.set_focus(None);
            }
            return;
        }
        if let Some(field) = self.focus {
            if field == Field::Slot && matches!(ui_key, Key::Up | Key::Down) {
                if let Some(edit) = &mut self.day.slot_edit {
                    let count = edit.suggestions.len();
                    if count > 0 {
                        edit.highlighted = Some(match (ui_key, edit.highlighted) {
                            (Key::Down, None) => 0,
                            (Key::Down, Some(i)) => (i + 1).min(count - 1),
                            (_, None) => count - 1,
                            (_, Some(i)) => i.saturating_sub(1),
                        });
                    }
                }
                return;
            }
            let response = self.field_mut(field).handle_event(UiEvent::KeyPressed {
                key: ui_key,
                modifiers,
            });
            if let Some(WidgetAction::Activate(_)) = response.action {
                self.activate(field, modifiers);
            }
            self.after_field_edit();
            return;
        }
        let primary = modifiers.meta || modifiers.ctrl;
        match ui_key {
            Key::Backspace | Key::Delete if !primary => self.run(Command::Delete, host),
            Key::Left if self.tab == Tab::Day && !primary => self.change_day(-1),
            Key::Right if self.tab == Tab::Day && !primary => self.change_day(1),
            _ => {}
        }
    }

    // ----- painting -----------------------------------------------------

    pub fn paint(&mut self, scene: &mut Vec<PaintOp>, host: &dyn TaskHost) {
        let l = self.layout;
        draw::fill(
            scene,
            rect(0.0, 0.0, self.size.0, self.size.1),
            draw::BACKGROUND,
        );
        match self.tab {
            Tab::Day => self.paint_day(scene),
            Tab::Project => self.paint_project(scene, host),
        }
        self.paint_list(scene);
        self.paint_toolbar(scene);
        self.paint_status(scene);
        if self.tab == Tab::Day {
            self.paint_overlays(scene);
        }
        self.paint_drag(scene);
        if let Some(menu) = &self.drawn_menu {
            menu.paint(scene);
        }
        let _ = l;
    }

    fn paint_toolbar(&self, scene: &mut Vec<PaintOp>) {
        let l = self.layout;
        draw::fill(scene, l.toolbar, draw::TOOLBAR);
        draw::line(
            scene,
            Point {
                x: 0.0,
                y: l.toolbar.bottom() - 0.5,
            },
            Point {
                x: l.toolbar.right(),
                y: l.toolbar.bottom() - 0.5,
            },
            draw::BORDER,
        );
        for (tab, label, r) in [
            (Tab::Day, "Day Plan", l.tabs[0]),
            (Tab::Project, "Project Plan", l.tabs[1]),
        ] {
            let current = self.tab == tab;
            if current {
                draw::fill(scene, r, draw::BACKGROUND);
                draw::stroke(scene, rect(r.x, r.y, r.width, r.height + 1.0), draw::BORDER);
                draw::fill(
                    scene,
                    rect(r.x + 1.0, r.bottom() - 1.0, r.width - 2.0, 2.0),
                    draw::BACKGROUND,
                );
            }
            let color = if current { draw::TEXT } else { draw::DIM_TEXT };
            draw::text(
                scene,
                r,
                None,
                label,
                draw::FONT,
                color,
                HorizontalAlign::Center,
            );
        }
        let hot = |r: Rect| r.contains(self.pointer) && self.drag.is_none();
        draw::button(
            scene,
            l.new_task,
            "New Task",
            self.editable(),
            hot(l.new_task),
        );
        draw::button(scene, l.save, "Save", self.unsaved(), hot(l.save));
        let dragging = self.drag.is_some_and(|d| {
            d.active
                && matches!(
                    d.kind,
                    DragKind::Task { .. } | DragKind::Node { .. } | DragKind::Entry { .. }
                )
        });
        let over = dragging && l.trash.contains(self.pointer);
        draw::fill(
            scene,
            l.trash,
            if over {
                draw::TRASH_HOT
            } else if dragging {
                draw::DROP_TARGET
            } else {
                draw::TOOLBAR
            },
        );
        if dragging {
            draw::stroke(scene, l.trash, draw::BORDER);
        }
        draw::trash_icon(
            scene,
            l.trash,
            if dragging { draw::TEXT } else { draw::DIM_TEXT },
        );
    }

    fn paint_status(&self, scene: &mut Vec<PaintOp>) {
        let l = self.layout.status;
        draw::fill(scene, l, draw::TOOLBAR);
        let (message, color) = match &self.status {
            Status::Info(text) => (text.as_str(), draw::DIM_TEXT),
            Status::Error(text) => (text.as_str(), draw::ERROR_TEXT),
        };
        let folder = self
            .data_dir
            .as_ref()
            .map(|dir| abbreviate_home(dir))
            .unwrap_or_default();
        let half = l.width * 0.6;
        draw::text(
            scene,
            rect(l.x + MARGIN, l.y, half - MARGIN, l.height),
            None,
            message,
            draw::SMALL_FONT,
            color,
            HorizontalAlign::Left,
        );
        draw::text(
            scene,
            rect(l.x + half, l.y, l.width - half - MARGIN, l.height),
            None,
            folder,
            draw::SMALL_FONT,
            draw::DIM_TEXT,
            HorizontalAlign::Right,
        );
    }

    fn paint_day(&self, scene: &mut Vec<PaintOp>) {
        let l = self.layout;
        let today = clock::today();
        let banner_color = match self.day.date.cmp(&today) {
            std::cmp::Ordering::Less => draw::BANNER_PAST,
            std::cmp::Ordering::Equal => draw::BANNER_TODAY,
            std::cmp::Ordering::Greater => draw::BANNER_FUTURE,
        };
        draw::fill(scene, l.banner, banner_color);
        let hot = |r: Rect| r.contains(self.pointer) && self.drag.is_none();
        draw::button(scene, l.prev_day, "<", true, hot(l.prev_day));
        draw::button(scene, l.next_day, ">", true, hot(l.next_day));
        if l.today.width > 0.0 {
            draw::button(scene, l.today, "Today", true, hot(l.today));
        }
        let text_left = l.prev_day.right() + 12.0;
        let text_right = if l.today.width > 0.0 {
            l.today.x
        } else {
            l.next_day.x
        } - 12.0;
        draw::text(
            scene,
            rect(
                text_left,
                l.banner.y,
                text_right - text_left,
                l.banner.height,
            ),
            None,
            clock::banner_text(self.day.date),
            draw::BANNER_FONT,
            draw::TEXT,
            HorizontalAlign::Left,
        );

        // Column titles.
        draw::fill(scene, l.pane_header, draw::TOOLBAR);
        for (pane, r) in [
            (PaneKind::Planned, l.panes[0]),
            (PaneKind::Actual, l.panes[1]),
        ] {
            draw::text(
                scene,
                rect(r.x, l.pane_header.y, r.width, l.pane_header.height),
                None,
                pane.title(),
                draw::SMALL_FONT,
                draw::DIM_TEXT,
                HorizontalAlign::Center,
            );
        }

        // Gutter and grid.
        draw::fill(scene, l.gutter, draw::GUTTER);
        let grid = rect(
            l.panes[0].x,
            l.gutter.y,
            l.panes[1].right() - l.panes[0].x,
            l.gutter.height,
        );
        draw::fill(scene, grid, draw::PANEL);
        let first = (self.day.scroll / SLOT_PX).floor() as u64;
        let last = (((self.day.scroll + grid.height) / SLOT_PX).ceil() as u64).min(SLOTS_PER_DAY);
        for slot in first..=last {
            let y = grid.y + slot as f32 * SLOT_PX - self.day.scroll;
            if y < grid.y || y > grid.bottom() {
                continue;
            }
            let hour = slot % 4 == 0;
            let color = if hour { draw::HOUR_LINE } else { draw::GRID };
            let from = if hour { 0.0 } else { grid.x };
            draw::line(
                scene,
                Point { x: from, y },
                Point { x: grid.right(), y },
                color,
            );
            if hour && slot < SLOTS_PER_DAY {
                draw::text(
                    scene,
                    rect(6.0, y + 2.0, GUTTER_WIDTH - 12.0, SLOT_PX),
                    Some(l.gutter),
                    clock::hour_label((slot / 4) as u32),
                    draw::SMALL_FONT,
                    draw::TEXT,
                    HorizontalAlign::Left,
                );
            }
        }
        draw::line(
            scene,
            Point {
                x: GUTTER_WIDTH,
                y: grid.y,
            },
            Point {
                x: GUTTER_WIDTH,
                y: grid.bottom(),
            },
            draw::BORDER,
        );
        let splitter_color = if self.drag.is_some_and(|d| d.kind == DragKind::Split)
            || (self.drag.is_none() && l.splitter.contains(self.pointer))
        {
            draw::ACCENT
        } else {
            draw::TOOLBAR
        };
        draw::fill(scene, l.splitter, splitter_color);

        // Entries.
        let day = clock::day_start(self.day.date);
        let dropping_task = self
            .drag
            .is_some_and(|d| d.active && matches!(d.kind, DragKind::Task { .. }));
        for (pane, r) in [
            (PaneKind::Planned, l.panes[0]),
            (PaneKind::Actual, l.panes[1]),
        ] {
            if dropping_task && r.contains(self.pointer) {
                let slot = self.slot_at(self.pointer.y);
                let target = rect(
                    r.x + 3.0,
                    r.y + slot as f32 * SLOT_PX - self.day.scroll,
                    r.width - 6.0,
                    SLOT_PX,
                );
                draw::fill_clipped(scene, target, r, draw::DROP_TARGET);
            }
            let (fill, edge) = match pane {
                PaneKind::Planned => (draw::PLANNED_FILL, draw::PLANNED_EDGE),
                PaneKind::Actual => (draw::ACTUAL_FILL, draw::ACTUAL_EDGE),
            };
            for entry in self.plan.entries_on(day, pane) {
                let er = self.entry_rect(r, entry.start, entry.stop);
                let Some(visible) = draw::intersect(er, r) else {
                    continue;
                };
                let selected = self.day.selected_entry == Some(entry.entity.id);
                draw::fill(scene, visible, fill);
                if selected {
                    draw::stroke(scene, visible, draw::ACCENT);
                    let inner = rect(
                        visible.x + 1.0,
                        visible.y + 1.0,
                        visible.width - 2.0,
                        visible.height - 2.0,
                    );
                    draw::stroke(scene, inner, draw::ACCENT);
                } else if visible == er {
                    draw::stroke(scene, er, edge);
                }
                draw::fill(scene, rect(visible.x, visible.y, 3.0, visible.height), edge);
                let title = self
                    .plan
                    .task(entry.task_id)
                    .map(|t| t.name.as_str())
                    .unwrap_or(entry.title.as_str());
                let label = format!(
                    "{}  {}",
                    clock::slot_label(entry.start.saturating_sub(day) / TICKS_PER_SLOT),
                    title
                );
                draw::text(
                    scene,
                    rect(er.x + 8.0, er.y, er.width - 12.0, SLOT_PX.min(er.height)),
                    Some(visible),
                    label,
                    draw::SMALL_FONT,
                    draw::TEXT,
                    HorizontalAlign::Left,
                );
            }
        }
    }

    /// The slot editor and the context menu, drawn over everything else in
    /// the day plan.
    fn paint_overlays(&self, scene: &mut Vec<PaintOp>) {
        if let (Some(edit), Some(_)) = (&self.day.slot_edit, self.slot_edit_rect()) {
            self.slot_field.paint(scene);
            if self.slot_field.text().is_empty() {
                let r = self.slot_field.bounds();
                draw::text(
                    scene,
                    rect(r.x + 7.0, r.y, r.width - 12.0, r.height),
                    None,
                    format!(
                        "{}: type a task, Enter to plan it",
                        clock::slot_label(edit.slot)
                    ),
                    draw::SMALL_FONT,
                    draw::DIM_TEXT,
                    HorizontalAlign::Left,
                );
            }
            for (i, (id, r)) in self.suggestion_rects().into_iter().enumerate() {
                let hot = edit.highlighted == Some(i);
                draw::fill(scene, r, if hot { draw::SELECTION } else { draw::PANEL });
                draw::stroke(scene, r, draw::BORDER);
                let name = self
                    .plan
                    .task(id)
                    .map(|t| t.name.clone())
                    .unwrap_or_default();
                draw::text(
                    scene,
                    rect(r.x + 7.0, r.y, r.width - 12.0, r.height),
                    None,
                    name,
                    draw::SMALL_FONT,
                    draw::TEXT,
                    HorizontalAlign::Left,
                );
            }
        }
        if self.day.context_open {
            for (id, name, r) in self.context_options() {
                let current = self.day.context == id;
                let hot = r.contains(self.pointer);
                draw::fill(
                    scene,
                    r,
                    if hot {
                        draw::HOVER
                    } else if current {
                        draw::SELECTION
                    } else {
                        draw::PANEL
                    },
                );
                draw::stroke(scene, r, draw::BORDER);
                draw::text(
                    scene,
                    rect(r.x + 8.0, r.y, r.width - 16.0, r.height),
                    None,
                    name,
                    draw::FONT,
                    draw::TEXT,
                    HorizontalAlign::Left,
                );
            }
        }
    }

    fn paint_list(&self, scene: &mut Vec<PaintOp>) {
        let l = self.layout;
        draw::fill(scene, l.list, draw::BACKGROUND);
        let grip_hot = self
            .drag
            .is_some_and(|d| matches!(d.kind, DragKind::ListWidth { .. }))
            || (self.drag.is_none() && l.grip.contains(self.pointer));
        draw::fill(
            scene,
            l.grip,
            if grip_hot {
                draw::ACCENT
            } else {
                draw::TOOLBAR
            },
        );

        match self.tab {
            Tab::Day => {
                let label = self
                    .day
                    .context
                    .and_then(|id| self.plan.task(id))
                    .map_or("All Tasks".to_string(), |t| t.name.clone());
                draw::fill(scene, l.context, draw::PANEL);
                draw::stroke(scene, l.context, draw::BORDER);
                draw::text(
                    scene,
                    rect(
                        l.context.x + 8.0,
                        l.context.y,
                        l.context.width - 30.0,
                        l.context.height,
                    ),
                    None,
                    label,
                    draw::FONT,
                    draw::TEXT,
                    HorizontalAlign::Left,
                );
                let arrow = Point {
                    x: l.context.right() - 14.0,
                    y: l.context.y + l.context.height / 2.0,
                };
                draw::line(
                    scene,
                    Point {
                        x: arrow.x - 5.0,
                        y: arrow.y - 2.0,
                    },
                    Point {
                        x: arrow.x,
                        y: arrow.y + 3.0,
                    },
                    draw::DIM_TEXT,
                );
                draw::line(
                    scene,
                    Point {
                        x: arrow.x,
                        y: arrow.y + 3.0,
                    },
                    Point {
                        x: arrow.x + 5.0,
                        y: arrow.y - 2.0,
                    },
                    draw::DIM_TEXT,
                );
            }
            Tab::Project => {
                draw::text(
                    scene,
                    rect(l.rows.x, l.body.y + 4.0, l.rows.width, 18.0),
                    None,
                    "Projects",
                    draw::SMALL_FONT,
                    draw::DIM_TEXT,
                    HorizontalAlign::Left,
                );
            }
        }

        draw::fill(scene, l.rows, draw::PANEL);
        draw::stroke(scene, l.rows, draw::BORDER);
        let selected = match self.tab {
            Tab::Day => self.day.selected_task,
            Tab::Project => self
                .project
                .root
                .or(self.plan.roots().first().map(|t| t.entity.id)),
        };
        let rows = self.list_rows();
        if rows.is_empty() {
            let hint = match (self.tab, &self.load) {
                (_, LoadState::Loading) => "Loading…",
                (Tab::Day, _) if self.day.context.is_some() => "No tasks in this project yet.",
                (Tab::Day, _) => "No tasks yet. Add one below.",
                (Tab::Project, _) => "No projects yet. Add one below.",
            };
            draw::text(
                scene,
                rect(
                    l.rows.x + 8.0,
                    l.rows.y + 4.0,
                    l.rows.width - 16.0,
                    ROW_HEIGHT,
                ),
                None,
                hint,
                draw::SMALL_FONT,
                draw::DIM_TEXT,
                HorizontalAlign::Left,
            );
        }
        let scroll = self.list_scroll();
        for (i, (id, name, priority)) in rows.iter().enumerate() {
            let y = l.rows.y + i as f32 * ROW_HEIGHT - scroll;
            if y + ROW_HEIGHT < l.rows.y || y > l.rows.bottom() {
                continue;
            }
            let row = rect(l.rows.x, y, l.rows.width, ROW_HEIGHT);
            if selected == Some(*id) {
                draw::fill_clipped(scene, row, l.rows, draw::SELECTION);
            }
            if y >= l.rows.y && y + ROW_HEIGHT <= l.rows.bottom() {
                draw::priority_mark(scene, rect(row.x + 6.0, row.y + 5.0, 14.0, 14.0), *priority);
            }
            draw::text(
                scene,
                rect(row.x + 26.0, row.y, row.width - 32.0, row.height),
                Some(l.rows),
                name.clone(),
                draw::FONT,
                draw::TEXT,
                HorizontalAlign::Left,
            );
        }

        let (field, placeholder) = match self.tab {
            Tab::Day => (
                &self.day_add,
                if self.day.context.is_some() {
                    "Add a task to this project"
                } else {
                    "Add a task"
                },
            ),
            Tab::Project => (&self.project_add, "Add a project"),
        };
        field.paint(scene);
        if field.text().is_empty() && !field.focused() {
            let r = field.bounds();
            draw::text(
                scene,
                rect(r.x + 7.0, r.y, r.width - 12.0, r.height),
                None,
                placeholder,
                draw::FONT,
                draw::DIM_TEXT,
                HorizontalAlign::Left,
            );
        }
        if self.tab == Tab::Project {
            self.paint_details(scene);
        }
    }

    fn paint_details(&self, scene: &mut Vec<PaintOp>) {
        let l = self.layout;
        let d = l.details;
        draw::fill(scene, d, draw::PANEL);
        draw::stroke(scene, d, draw::BORDER);
        let Some(task) = self.project.selected.and_then(|id| self.plan.task(id)) else {
            draw::text(
                scene,
                rect(d.x + 8.0, d.y + 8.0, d.width - 16.0, 20.0),
                None,
                "Select a task to change it here.",
                draw::SMALL_FONT,
                draw::DIM_TEXT,
                HorizontalAlign::Left,
            );
            return;
        };
        let label = |scene: &mut Vec<PaintOp>, y: f32, text: &str| {
            draw::text(
                scene,
                rect(d.x + 8.0, y, d.width - 16.0, 20.0),
                None,
                text.to_string(),
                draw::SMALL_FONT,
                draw::DIM_TEXT,
                HorizontalAlign::Left,
            );
        };
        label(scene, d.y + 4.0, "Name");
        self.name_field.paint(scene);
        label(scene, l.name_field.bottom() + 4.0, "Priority");
        let place = task
            .parent
            .and_then(|id| self.plan.task(id))
            .map_or("A project".to_string(), |p| {
                format!("In \u{201c}{}\u{201d}", p.name)
            });
        label(scene, l.priorities[0].bottom() + 6.0, &place);
        for (i, r) in l.priorities.iter().enumerate() {
            let current = task.priority.clamp(0, MAX_PRIORITY) as usize == i;
            if current {
                draw::fill(scene, *r, draw::SELECTION);
                draw::stroke(scene, *r, draw::ACCENT);
            }
            if i == 0 {
                draw::text(
                    scene,
                    *r,
                    None,
                    "None",
                    draw::SMALL_FONT,
                    draw::TEXT,
                    HorizontalAlign::Center,
                );
            } else {
                draw::priority_mark(scene, rect(r.x + 4.0, r.y + 4.0, 14.0, 14.0), i as i32);
            }
        }
        let hot = |r: Rect| r.contains(self.pointer) && self.drag.is_none();
        draw::button(
            scene,
            l.add_subtask,
            "Add Subtask",
            self.editable(),
            hot(l.add_subtask),
        );
        draw::button(
            scene,
            l.delete_task,
            "Delete",
            self.editable(),
            hot(l.delete_task),
        );
    }

    fn paint_project(&mut self, scene: &mut Vec<PaintOp>, host: &dyn TaskHost) {
        let canvas = self.layout.canvas;
        draw::fill(scene, canvas, draw::PANEL);
        let Some((root, tree)) = self.tree(host) else {
            let hint = if self.load == LoadState::Loading {
                "Loading…"
            } else {
                "Add a project on the right to see its tasks here."
            };
            draw::text(
                scene,
                rect(canvas.x + 20.0, canvas.y + 20.0, canvas.width - 40.0, 24.0),
                None,
                hint,
                draw::FONT,
                draw::DIM_TEXT,
                HorizontalAlign::Left,
            );
            return;
        };
        let origin = self.tree_origin(&tree);
        let rects: Vec<Rect> = tree
            .nodes
            .iter()
            .map(|n| Self::node_rect(origin, n))
            .collect();
        for (node, r) in tree.nodes.iter().zip(&rects) {
            if let Some(parent) = node.parent {
                let p = rects[parent];
                let from = Point {
                    x: p.x + p.width / 2.0,
                    y: p.bottom(),
                };
                let to = Point {
                    x: r.x + r.width / 2.0,
                    y: r.y,
                };
                draw::line_clipped(scene, from, to, canvas, draw::NODE_EDGE);
            }
        }
        let drop_target = self.drag.and_then(|d| match d.kind {
            DragKind::Task { id } | DragKind::Node { id } if d.active => Some(id),
            _ => None,
        });
        for (node, r) in tree.nodes.iter().zip(&rects) {
            let Some(visible) = draw::intersect(*r, canvas) else {
                continue;
            };
            let Some(task) = self.plan.task(node.id) else {
                continue;
            };
            let fill = if drop_target.is_some_and(|dragged| dragged != node.id)
                && r.contains(self.pointer)
            {
                draw::DROP_TARGET
            } else if self.project.selected == Some(node.id) {
                draw::SELECTION
            } else if node.id == root {
                draw::NODE_ROOT
            } else if node.depth == 1 {
                draw::NODE_MILESTONE
            } else {
                draw::NODE_TASK
            };
            draw::fill(scene, visible, fill);
            if visible == *r {
                let edge = if self.project.selected == Some(node.id) {
                    draw::ACCENT
                } else {
                    draw::NODE_EDGE
                };
                draw::stroke(scene, *r, edge);
            }
            let mark = if task.priority > 0 {
                tree_layout::PRIORITY_MARK
            } else {
                0.0
            };
            if mark > 0.0 && visible == *r {
                draw::priority_mark(
                    scene,
                    rect(r.x + 6.0, r.y + (NODE_HEIGHT - 12.0) / 2.0, 12.0, 12.0),
                    task.priority,
                );
            }
            draw::text(
                scene,
                rect(r.x + 8.0 + mark, r.y, r.width - 16.0 - mark, r.height),
                Some(canvas),
                task.name.clone(),
                draw::FONT,
                draw::TEXT,
                if mark > 0.0 {
                    HorizontalAlign::Left
                } else {
                    HorizontalAlign::Center
                },
            );
            if node.has_children {
                let t = Self::toggle_rect(*r);
                if draw::intersect(t, canvas) == Some(t) {
                    draw::fill(scene, t, draw::PANEL);
                    draw::stroke(scene, t, draw::NODE_EDGE);
                    let c = draw::center(t);
                    draw::line(
                        scene,
                        Point {
                            x: c.x - 4.0,
                            y: c.y,
                        },
                        Point {
                            x: c.x + 4.0,
                            y: c.y,
                        },
                        draw::NODE_EDGE,
                    );
                    if node.collapsed {
                        draw::line(
                            scene,
                            Point {
                                x: c.x,
                                y: c.y - 4.0,
                            },
                            Point {
                                x: c.x,
                                y: c.y + 4.0,
                            },
                            draw::NODE_EDGE,
                        );
                    }
                }
            }
        }
    }

    /// What is being dragged, under the pointer.
    fn paint_drag(&self, scene: &mut Vec<PaintOp>) {
        let Some(drag) = self.drag.filter(|d| d.active) else {
            return;
        };
        let (DragKind::Task { id } | DragKind::Node { id }) = drag.kind else {
            return;
        };
        let Some(task) = self.plan.task(id) else {
            return;
        };
        let r = rect(self.pointer.x + 12.0, self.pointer.y + 8.0, 180.0, 24.0);
        draw::fill(scene, r, draw::PANEL);
        draw::stroke(scene, r, draw::ACCENT);
        draw::text(
            scene,
            rect(r.x + 8.0, r.y, r.width - 16.0, r.height),
            None,
            task.name.clone(),
            draw::FONT,
            draw::TEXT,
            HorizontalAlign::Left,
        );
    }
}

fn menu_is_open(menu: &Option<MenuBarModel>) -> bool {
    menu.as_ref().is_some_and(MenuBarModel::is_open)
}

/// `~/…` for a path in the home folder.
fn abbreviate_home(path: &std::path::Path) -> String {
    if let Some(home) = std::env::var_os("HOME") {
        if let Ok(rest) = path.strip_prefix(&home) {
            return format!("~/{}", rest.display());
        }
    }
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use loadngo_host_core::HostKeyEvent;

    struct FakeHost;

    impl TaskHost for FakeHost {
        fn measure(&self, text: &str, font_size: u16) -> f32 {
            text.chars().count() as f32 * f32::from(font_size) * 0.5
        }
        fn read_clipboard(&self) -> Option<String> {
            Some("Pasted".into())
        }
        fn write_clipboard(&self, _text: &str) {}
    }

    const SIZE: (f32, f32) = (1200.0, 800.0);

    struct Rig {
        app: TaskApp,
        now: Instant,
        dir: tempfile::TempDir,
    }

    fn date() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, 13).unwrap()
    }

    impl Rig {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let mut app = TaskApp::new(Some(dir.path().to_path_buf()), "tester", date());
            app.set_native_menu(true);
            let mut rig = Self {
                app,
                now: Instant::now(),
                dir,
            };
            rig.run_io();
            rig.frame(InputSnapshot::default());
            rig
        }

        fn run_io(&mut self) {
            for request in self.app.take_requests() {
                let response = store::perform(request);
                self.app.apply(response, self.now);
            }
        }

        fn frame(&mut self, mut input: InputSnapshot) -> FrameOutcome {
            input.mouse_x = self.app.pointer.x;
            input.mouse_y = self.app.pointer.y;
            self.frame_at(input)
        }

        fn frame_at(&mut self, input: InputSnapshot) -> FrameOutcome {
            let outcome = self.app.frame(&input, SIZE, self.now, &FakeHost);
            let mut scene = Vec::new();
            self.app.paint(&mut scene, &FakeHost);
            outcome
        }

        fn move_to(&mut self, point: Point, down: bool) {
            let input = InputSnapshot {
                mouse_x: point.x,
                mouse_y: point.y,
                mouse_down: down,
                ..InputSnapshot::default()
            };
            self.frame_at(input);
        }

        fn click(&mut self, point: Point) {
            self.press(point);
            self.release(point);
        }

        fn press(&mut self, point: Point) {
            self.frame_at(InputSnapshot {
                mouse_x: point.x,
                mouse_y: point.y,
                mouse_pressed: true,
                mouse_down: true,
                ..InputSnapshot::default()
            });
        }

        fn release(&mut self, point: Point) {
            self.frame_at(InputSnapshot {
                mouse_x: point.x,
                mouse_y: point.y,
                mouse_released: true,
                ..InputSnapshot::default()
            });
        }

        fn drag(&mut self, from: Point, to: Point) {
            self.press(from);
            let steps = 5;
            for i in 1..=steps {
                let t = i as f32 / steps as f32;
                self.move_to(
                    Point {
                        x: from.x + (to.x - from.x) * t,
                        y: from.y + (to.y - from.y) * t,
                    },
                    true,
                );
            }
            self.release(to);
        }

        fn type_text(&mut self, text: &str) {
            self.frame(InputSnapshot {
                typed_text: text.into(),
                ..InputSnapshot::default()
            });
        }

        fn key(&mut self, key: HostKey) {
            self.frame(InputSnapshot {
                key_events: vec![HostKeyEvent {
                    key,
                    modifiers: Modifiers::default(),
                }],
                ..InputSnapshot::default()
            });
        }

        fn command(&mut self, command: Command) -> FrameOutcome {
            self.frame(InputSnapshot {
                menu_commands: vec![command.menu_command()],
                ..InputSnapshot::default()
            })
        }

        fn add_task_from_list(&mut self, name: &str) {
            let field = center(self.app.layout.add_field);
            self.click(field);
            self.type_text(name);
            self.key(HostKey::Enter);
        }

        fn slot_point(&self, pane: PaneKind, slot: u64) -> Point {
            let r = self.app.pane_rect(pane);
            Point {
                x: r.x + r.width / 2.0,
                y: r.y + slot as f32 * SLOT_PX - self.app.day.scroll + SLOT_PX / 2.0,
            }
        }

        /// Lets the save delay pass and writes the plan.
        fn settle(&mut self) {
            self.now += SAVE_DELAY + Duration::from_millis(10);
            self.frame(InputSnapshot::default());
            self.run_io();
        }

        fn reopen(&self) -> TaskApp {
            let mut app = TaskApp::new(Some(self.dir.path().to_path_buf()), "tester", date());
            for request in app.take_requests() {
                app.apply(store::perform(request), self.now);
            }
            app
        }
    }

    fn center(r: Rect) -> Point {
        draw::center(r)
    }

    fn entry_starts(app: &TaskApp, pane: PaneKind) -> Vec<u64> {
        let day = clock::day_start(app.day.date);
        app.plan
            .entries_on(day, pane)
            .into_iter()
            .map(|e| (e.start - day) / TICKS_PER_SLOT)
            .collect()
    }

    #[test]
    fn a_new_plan_opens_empty_and_ready() {
        let rig = Rig::new();
        assert!(rig.app.editable());
        assert_eq!(rig.app.plan.task_count(), 0);
    }

    #[test]
    fn tasks_added_from_the_list_are_saved_and_reopen() {
        let mut rig = Rig::new();
        rig.add_task_from_list("Write report");
        rig.add_task_from_list("Call bank");
        assert_eq!(rig.app.plan.task_count(), 2);
        assert!(rig.app.unsaved());
        // Nothing is written before the delay.
        rig.frame(InputSnapshot::default());
        assert!(rig.app.take_requests().is_empty());
        rig.settle();
        assert!(!rig.app.unsaved());
        let reopened = rig.reopen();
        let names: Vec<String> = reopened
            .plan
            .leaves(None)
            .iter()
            .map(|t| t.name.clone())
            .collect();
        assert_eq!(names, ["Call bank", "Write report"]);
    }

    #[test]
    fn typing_in_a_slot_plans_a_task_there() {
        let mut rig = Rig::new();
        rig.add_task_from_list("Write report");
        let at = rig.slot_point(PaneKind::Planned, 36);
        rig.click(at);
        assert_eq!(rig.app.focus, Some(Field::Slot));
        rig.type_text("write");
        assert_eq!(rig.app.day.slot_edit.as_ref().unwrap().suggestions.len(), 1);
        rig.key(HostKey::Down);
        rig.key(HostKey::Enter);
        assert_eq!(entry_starts(&rig.app, PaneKind::Planned), [36]);
        assert_eq!(rig.app.plan.task_count(), 1, "the existing task was used");
        // A name nobody has yet makes a task.
        let at = rig.slot_point(PaneKind::Actual, 40);
        rig.click(at);
        rig.type_text("Lunch");
        rig.key(HostKey::Enter);
        assert_eq!(entry_starts(&rig.app, PaneKind::Actual), [40]);
        assert_eq!(rig.app.plan.task_count(), 2);
    }

    #[test]
    fn entries_move_resize_and_undo_in_one_step() {
        let mut rig = Rig::new();
        let at = rig.slot_point(PaneKind::Planned, 36);
        rig.click(at);
        rig.type_text("Focus");
        rig.key(HostKey::Enter);
        // Move it down four slots.
        let from = rig.slot_point(PaneKind::Planned, 36);
        let to = rig.slot_point(PaneKind::Planned, 40);
        rig.drag(
            Point {
                y: from.y - 4.0,
                ..from
            },
            Point {
                y: to.y - 4.0,
                ..to
            },
        );
        assert_eq!(entry_starts(&rig.app, PaneKind::Planned), [40]);
        // Stretch it to an hour from its bottom edge.
        let id = rig.app.day.selected_entry.unwrap();
        let entry_rect = {
            let e = rig.app.plan.entry(id).unwrap();
            rig.app.entry_rect(rig.app.layout.panes[0], e.start, e.stop)
        };
        let edge = Point {
            x: entry_rect.x + 20.0,
            y: entry_rect.bottom() - 2.0,
        };
        let lower = rig.slot_point(PaneKind::Planned, 43);
        rig.drag(
            edge,
            Point {
                x: edge.x,
                y: lower.y,
            },
        );
        let entry = rig.app.plan.entry(id).unwrap();
        assert_eq!((entry.stop - entry.start) / TICKS_PER_SLOT, 4);
        rig.command(Command::Undo);
        let entry = rig.app.plan.entry(id).unwrap();
        assert_eq!((entry.stop - entry.start) / TICKS_PER_SLOT, 1);
        rig.command(Command::Undo);
        assert_eq!(entry_starts(&rig.app, PaneKind::Planned), [36]);
    }

    #[test]
    fn a_task_dragged_from_the_list_lands_in_a_slot() {
        let mut rig = Rig::new();
        rig.add_task_from_list("Exercise");
        let row = Point {
            x: rig.app.layout.rows.x + 40.0,
            y: rig.app.layout.rows.y + ROW_HEIGHT / 2.0,
        };
        let slot = rig.slot_point(PaneKind::Actual, 50);
        rig.drag(row, slot);
        assert_eq!(entry_starts(&rig.app, PaneKind::Actual), [50]);
    }

    #[test]
    fn the_trash_deletes_what_is_dropped_on_it() {
        let mut rig = Rig::new();
        rig.add_task_from_list("Old");
        let at = rig.slot_point(PaneKind::Planned, 36);
        rig.click(at);
        rig.type_text("Old");
        rig.key(HostKey::Enter);
        let trash = center(rig.app.layout.trash);
        rig.drag(rig.slot_point(PaneKind::Planned, 36), trash);
        assert!(entry_starts(&rig.app, PaneKind::Planned).is_empty());
        rig.command(Command::Undo);
        assert_eq!(entry_starts(&rig.app, PaneKind::Planned), [36]);
        let row = Point {
            x: rig.app.layout.rows.x + 40.0,
            y: rig.app.layout.rows.y + ROW_HEIGHT / 2.0,
        };
        rig.drag(row, trash);
        assert_eq!(rig.app.plan.task_count(), 0);
    }

    #[test]
    fn days_change_with_the_banner_buttons() {
        let mut rig = Rig::new();
        let next = center(rig.app.layout.next_day);
        rig.click(next);
        assert_eq!(rig.app.day.date, date().succ_opt().unwrap());
        rig.key(HostKey::Left);
        rig.key(HostKey::Left);
        assert_eq!(rig.app.day.date, date().pred_opt().unwrap());
    }

    #[test]
    fn projects_take_subtasks_renames_and_priorities() {
        let mut rig = Rig::new();
        rig.command(Command::ShowProject);
        rig.add_task_from_list("Garden");
        let garden = rig
            .app
            .project
            .selected
            .expect("the new project is selected");
        // Add a subtask and name it.
        let add = center(rig.app.layout.add_subtask);
        rig.click(add);
        assert_eq!(rig.app.focus, Some(Field::Name));
        rig.type_text("Plant beans");
        rig.key(HostKey::Enter);
        let bean = rig.app.project.selected.unwrap();
        assert_eq!(rig.app.plan.task(bean).unwrap().name, "Plant beans");
        assert_eq!(rig.app.plan.task(bean).unwrap().parent, Some(garden));
        // Priority 2.
        let mark = center(rig.app.layout.priorities[2]);
        rig.click(mark);
        assert_eq!(rig.app.plan.task(bean).unwrap().priority, 2);
        // The hierarchy shows both.
        let (_, tree) = rig.app.tree(&FakeHost).unwrap();
        assert_eq!(tree.nodes.len(), 2);
        // The day plan's list offers the subtask, not the project.
        rig.command(Command::ShowDay);
        let rows: Vec<String> = rig.app.day_rows().into_iter().map(|r| r.1).collect();
        assert_eq!(rows, ["Plant beans"]);
    }

    #[test]
    fn hierarchy_boxes_drop_onto_each_other() {
        let mut rig = Rig::new();
        rig.command(Command::ShowProject);
        rig.add_task_from_list("Home");
        let home = rig.app.project.selected.unwrap();
        let add = center(rig.app.layout.add_subtask);
        rig.click(add);
        rig.type_text("Kitchen");
        rig.key(HostKey::Enter);
        let kitchen = rig.app.project.selected.unwrap();
        rig.click(center(rig.app.layout.canvas));
        rig.click(Point {
            x: rig.app.layout.canvas.x + 5.0,
            y: rig.app.layout.canvas.bottom() - 5.0,
        });
        // Select Home again and add a second subtask.
        rig.app.select_project_task(Some(home));
        let add = center(rig.app.layout.add_subtask);
        rig.click(add);
        rig.type_text("Paint");
        rig.key(HostKey::Enter);
        let paint = rig.app.project.selected.unwrap();
        let (_, tree) = rig.app.tree(&FakeHost).unwrap();
        let origin = rig.app.tree_origin(&tree);
        let rect_of = |id: Id| {
            let node = tree.nodes.iter().find(|n| n.id == id).unwrap();
            TaskApp::node_rect(origin, node)
        };
        let from = Point {
            x: rect_of(paint).x + 10.0,
            y: rect_of(paint).y + 8.0,
        };
        let to = Point {
            x: rect_of(kitchen).x + 10.0,
            y: rect_of(kitchen).y + 8.0,
        };
        rig.drag(from, to);
        assert_eq!(rig.app.plan.task(paint).unwrap().parent, Some(kitchen));
        // Home cannot go under its own subtask.
        let (_, tree) = rig.app.tree(&FakeHost).unwrap();
        let origin = rig.app.tree_origin(&tree);
        let node =
            |id: Id| TaskApp::node_rect(origin, tree.nodes.iter().find(|n| n.id == id).unwrap());
        rig.drag(
            Point {
                x: node(home).x + 10.0,
                y: node(home).y + 8.0,
            },
            Point {
                x: node(paint).x + 10.0,
                y: node(paint).y + 8.0,
            },
        );
        assert_eq!(rig.app.plan.task(home).unwrap().parent, None);
    }

    #[test]
    fn quitting_waits_for_the_save() {
        let mut rig = Rig::new();
        rig.add_task_from_list("Unsaved");
        let outcome = rig.command(Command::Quit);
        assert!(!outcome.quit);
        rig.run_io();
        let outcome = rig.frame(InputSnapshot::default());
        assert!(outcome.quit);
        assert_eq!(rig.reopen().plan.task_count(), 1);
    }

    #[test]
    fn an_unreadable_plan_is_never_written_over() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(store::tasks_path(dir.path()), "not json").unwrap();
        let mut app = TaskApp::new(Some(dir.path().to_path_buf()), "tester", date());
        let now = Instant::now();
        for request in app.take_requests() {
            app.apply(store::perform(request), now);
        }
        assert!(matches!(app.load, LoadState::Failed(_)));
        app.plan.add_task("x", None);
        let input = InputSnapshot {
            menu_commands: vec![Command::Save.menu_command()],
            ..InputSnapshot::default()
        };
        app.frame(&input, SIZE, now + Duration::from_secs(5), &FakeHost);
        assert!(app.take_requests().is_empty());
        assert_eq!(
            std::fs::read_to_string(store::tasks_path(dir.path())).unwrap(),
            "not json"
        );
    }

    #[test]
    fn idle_frames_ask_for_no_wake() {
        let mut rig = Rig::new();
        assert_eq!(rig.app.next_wake(rig.now), None);
        rig.add_task_from_list("Soon saved");
        assert!(rig.app.next_wake(rig.now).is_some());
        rig.settle();
        assert_eq!(rig.app.next_wake(rig.now), None);
    }

    #[test]
    fn list_and_split_widths_are_kept() {
        let mut rig = Rig::new();
        let grip = center(rig.app.layout.grip);
        rig.drag(
            grip,
            Point {
                x: grip.x - 60.0,
                y: grip.y,
            },
        );
        let width = rig.app.day.list_width;
        assert!((width - (DEFAULT_LIST_WIDTH + 60.0)).abs() < 1.0, "{width}");
        rig.settle();
        let reopened = rig.reopen();
        assert!((reopened.day.list_width - width).abs() < 1.0);
    }

    #[test]
    fn entries_saved_on_windows_open_on_their_day() {
        // A Win32 Task entries file: local FILETIME ticks, "Spec" kind.
        let start = clock::day_start(date()) + 9 * 4 * TICKS_PER_SLOT;
        let entries = format!(
            r#"{{"version":1,"generated_at":0,"entries":[{{"entity":{{"id":5,"origin_id":5,
            "owner":"jay","created":0,"machine_id":null,"user_id":null,"properties":{{}},
            "children":[]}},"task_id":0,"duration":{d},"start":{start},"stop":{stop},
            "title":"Standup","kind":"Spec","notes":null}}]}}"#,
            d = 2 * TICKS_PER_SLOT,
            stop = start + 2 * TICKS_PER_SLOT
        );
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(store::entries_path(dir.path()), entries).unwrap();
        let mut app = TaskApp::new(Some(dir.path().to_path_buf()), "tester", date());
        for request in app.take_requests() {
            app.apply(store::perform(request), Instant::now());
        }
        assert!(app.editable());
        assert_eq!(entry_starts(&app, PaneKind::Planned), [36]);
    }
}
