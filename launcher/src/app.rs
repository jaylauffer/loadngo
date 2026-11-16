//! The launcher's window: the catalog as a list, what each entry is doing,
//! and what the pointer and the keys do to it.
//!
//! The app owns no threads and does no blocking work. What it wants done
//! leaves as [`Effect`]s for the host loop (`main.rs`) to carry out, and what
//! happened comes back through [`LauncherApp::apply_catalog`],
//! [`LauncherApp::apply_process`] and [`LauncherApp::opened`].

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use loadngo_host_core::{HostKey, InputSnapshot};
use ui_core::{HorizontalAlign, PaintOp, Point, PointerButton, Rect};

use crate::catalog::{Catalog, Entry, Kind, Loaded};
use crate::draw::{self, Tone};
use crate::process::{self, Ending, Event, What};

const PAD: f32 = 16.0;
const HEADER_HEIGHT: f32 = 56.0;
const BANNER_HEIGHT: f32 = 28.0;
const HEADING_HEIGHT: f32 = 34.0;
const ROW_HEIGHT: f32 = 58.0;
const ROW_GAP: f32 = 6.0;
const BUTTON_WIDTH: f32 = 76.0;
const BUTTON_HEIGHT: f32 = 30.0;
const BUTTON_GAP: f32 = 8.0;
/// The pane at the bottom with nothing printed to show: what the entry does.
const DETAIL_BASE: f32 = 58.0;
/// The most printed lines the pane grows to show.
const DETAIL_LINES: usize = 7;
const LINE_HEIGHT: f32 = 17.0;
/// Output lines kept per entry.
const KEPT_LINES: usize = 200;

/// What an entry is doing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Idle,
    Building,
    /// It has been asked to start; a service stays here until its page is
    /// ready.
    Starting,
    Running,
    Stopping,
    /// It could not start or ended with a failure; the text says which.
    Failed(String),
}

impl State {
    fn busy(&self) -> bool {
        matches!(
            self,
            State::Building | State::Starting | State::Running | State::Stopping
        )
    }
}

/// What the host loop is asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Read the catalog file again.
    LoadCatalog,
    /// Open the catalog file for editing, making an empty one first if
    /// there is none.
    EditCatalog,
    /// Start `entry` as start number `run`.
    Start {
        run: u64,
        entry: Box<Entry>,
    },
    Stop {
        run: u64,
    },
    /// Open a page, file or folder with the system's opener.
    Open {
        target: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Start,
    Open,
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    Row(usize),
    Act(usize, Action),
    Reload,
    Edit,
}

struct Row {
    entry: Entry,
    state: State,
    /// The start it is on, while it has one.
    run: Option<u64>,
    /// The last lines a build or a service printed, or the end of a failed
    /// program's log.
    lines: VecDeque<String>,
}

impl Row {
    fn new(entry: Entry) -> Self {
        Self {
            entry,
            state: State::Idle,
            run: None,
            lines: VecDeque::new(),
        }
    }

    /// Its buttons, left to right.
    fn actions(&self) -> &'static [Action] {
        match (self.entry.kind(), &self.state) {
            (Kind::Link, _) => &[Action::Open],
            (_, State::Idle | State::Failed(_)) => &[Action::Start],
            (_, State::Building | State::Starting) => &[Action::Stop],
            (Kind::Service, State::Running) => &[Action::Open, Action::Stop],
            (Kind::Program, State::Running) => &[Action::Stop],
            (_, State::Stopping) => &[],
        }
    }
}

/// A heading or an entry, at its place in the scrolled list.
enum Placed {
    Heading { text: String, y: f32 },
    Entry { row: usize, y: f32 },
}

#[derive(Debug, Clone, Copy)]
struct Layout {
    header: Rect,
    banner: Rect,
    list: Rect,
    detail: Rect,
    reload: Rect,
    edit: Rect,
}

pub struct LauncherApp {
    catalog_path: PathBuf,
    logs: PathBuf,
    rows: Vec<Row>,
    placed: Vec<Placed>,
    content_height: f32,
    /// The catalog has been read at least once.
    loaded: bool,
    /// There is no catalog file.
    missing: bool,
    /// What to tell the user above the list: a catalog that could not be
    /// read, something that would not open.
    banner: Option<String>,
    selected: usize,
    scroll: f32,
    size: (f32, f32),
    pointer: Point,
    pressed: Option<Target>,
    /// The running program whose Stop has been pressed once and waits for a
    /// second press.
    armed: Option<usize>,
    next_run: u64,
    effects: Vec<Effect>,
    /// The entries to start as soon as the list has been read.
    start_when_loaded: Vec<String>,
}

impl LauncherApp {
    /// `catalog_path` is the file listing what to launch; `logs` is where
    /// the log files of programs left running are (for showing).
    pub fn new(catalog_path: PathBuf, logs: PathBuf) -> Self {
        Self {
            catalog_path,
            logs,
            rows: Vec::new(),
            placed: Vec::new(),
            content_height: 0.0,
            loaded: false,
            missing: false,
            banner: None,
            selected: 0,
            scroll: 0.0,
            size: (0.0, 0.0),
            pointer: Point::default(),
            pressed: None,
            armed: None,
            next_run: 1,
            effects: vec![Effect::LoadCatalog],
            start_when_loaded: Vec::new(),
        }
    }

    /// Starts (or opens) the entry called `name` once the list has been
    /// read, as if its button had been pressed.
    pub fn start_when_loaded(&mut self, name: String) {
        self.start_when_loaded.push(name);
    }

    /// What the host loop should do now; each effect is handed over once.
    pub fn take_effects(&mut self) -> Vec<Effect> {
        std::mem::take(&mut self.effects)
    }

    /// How many entries are building, starting, running or stopping.
    pub fn busy(&self) -> usize {
        self.rows.iter().filter(|row| row.state.busy()).count()
    }

    /// The state of the entry called `name`.
    pub fn state_of(&self, name: &str) -> Option<&State> {
        self.rows
            .iter()
            .find(|row| row.entry.name == name)
            .map(|row| &row.state)
    }

    /// The catalog file has been read.
    pub fn apply_catalog(&mut self, loaded: Loaded) {
        self.loaded = true;
        match loaded {
            Loaded::Catalog(catalog) => {
                self.missing = false;
                self.banner = None;
                self.set_rows(catalog);
            }
            Loaded::Missing => {
                self.missing = true;
                self.banner = None;
                self.set_rows(Catalog::default());
            }
            // Whatever was listed stays, so a slip in the file takes nothing
            // away that is running.
            Loaded::Wrong(error) => {
                self.missing = false;
                self.banner = Some(format!("The list could not be read: {error}"));
                return;
            }
        }
        for name in std::mem::take(&mut self.start_when_loaded) {
            let found = self.rows.iter().position(|row| row.entry.name == name);
            let action = found.and_then(|index| {
                self.rows[index]
                    .actions()
                    .iter()
                    .copied()
                    .find(|action| *action != Action::Stop)
            });
            match (found, action) {
                (Some(index), Some(action)) => {
                    self.select(index);
                    self.perform(index, action);
                }
                _ => {
                    self.banner = Some(format!("There is no entry called \"{name}\" to start."));
                }
            }
        }
    }

    /// Replaces the list, keeping what each entry that is still there is
    /// doing. A service or a build whose entry has gone is stopped; a
    /// program in its own window is let be.
    fn set_rows(&mut self, catalog: Catalog) {
        let chosen = self
            .rows
            .get(self.selected)
            .map(|row| row.entry.name.clone());
        let mut old = std::mem::take(&mut self.rows);
        for entry in catalog.entries {
            let kept = old
                .iter()
                .position(|row| row.entry.name == entry.name)
                .map(|at| old.swap_remove(at));
            self.rows.push(match kept {
                Some(mut row) => {
                    row.entry = entry;
                    row
                }
                None => Row::new(entry),
            });
        }
        for gone in old {
            let leave = gone.entry.kind() == Kind::Program && gone.state == State::Running;
            if let (Some(run), false) = (gone.run, leave) {
                self.effects.push(Effect::Stop { run });
            }
        }
        self.selected = chosen
            .and_then(|name| self.rows.iter().position(|row| row.entry.name == name))
            .unwrap_or(0);
        self.armed = None;
        self.pressed = None;
        self.place();
    }

    /// Lays the headings and entries out down the list.
    fn place(&mut self) {
        self.placed.clear();
        let mut y = PAD - ROW_GAP;
        let mut group: Option<&str> = None;
        for (index, row) in self.rows.iter().enumerate() {
            let this = row.entry.group.as_str();
            let changed = group != Some(this);
            // Entries with no group before any heading need none.
            if changed && !(group.is_none() && this.is_empty()) {
                let text = if this.is_empty() { "Other" } else { this };
                self.placed.push(Placed::Heading {
                    text: text.to_string(),
                    y,
                });
                y += HEADING_HEIGHT;
            } else {
                y += ROW_GAP;
            }
            group = Some(this);
            self.placed.push(Placed::Entry { row: index, y });
            y += ROW_HEIGHT;
        }
        self.content_height = y + PAD;
    }

    /// Something happened to a start.
    pub fn apply_process(&mut self, event: Event) {
        let Some(index) = self.rows.iter().position(|row| row.run == Some(event.run)) else {
            return;
        };
        let row = &mut self.rows[index];
        match event.what {
            What::Building => row.state = State::Building,
            What::Started => {
                // Stop may have been pressed while it was starting.
                if row.state != State::Stopping {
                    row.state = match row.entry.kind() {
                        Kind::Service => State::Starting,
                        Kind::Program | Kind::Link => State::Running,
                    };
                }
            }
            What::Line(line) => {
                if row.lines.len() == KEPT_LINES {
                    row.lines.pop_front();
                }
                row.lines.push_back(line);
            }
            What::Ready => {
                if row.state != State::Stopping {
                    row.state = State::Running;
                    if let Some(target) = row.entry.open.clone() {
                        self.effects.push(Effect::Open { target });
                    }
                }
            }
            What::Ended(ending) => {
                row.run = None;
                match ending {
                    Ending::Finished | Ending::Stopped => row.state = State::Idle,
                    Ending::Failed { why, tail } => {
                        // A build or a service has shown its lines as they
                        // came; a program's are in its log.
                        if row.lines.is_empty() {
                            row.lines.extend(tail);
                        }
                        row.state = State::Failed(why);
                    }
                }
                if self.armed == Some(index) {
                    self.armed = None;
                }
            }
        }
    }

    /// The system's opener has answered for `target`.
    pub fn opened(&mut self, target: &str, result: Result<(), String>) {
        if let Err(error) = result {
            self.banner = Some(format!("Could not open {target}: {error}"));
        }
    }

    fn layout(&self) -> Layout {
        let (width, height) = self.size;
        let header = Rect {
            x: 0.0,
            y: 0.0,
            width,
            height: HEADER_HEIGHT,
        };
        let banner = Rect {
            x: 0.0,
            y: HEADER_HEIGHT,
            width,
            height: if self.banner.is_some() {
                BANNER_HEIGHT
            } else {
                0.0
            },
        };
        let detail_height = self
            .detail_wanted()
            .min((height - banner.bottom()) * 0.5)
            .max(0.0);
        let detail = Rect {
            x: 0.0,
            y: height - detail_height,
            width,
            height: detail_height,
        };
        let list = Rect {
            x: 0.0,
            y: banner.bottom(),
            width,
            height: (detail.y - banner.bottom()).max(0.0),
        };
        let button_y = (HEADER_HEIGHT - BUTTON_HEIGHT) / 2.0;
        let edit = Rect {
            x: width - PAD - BUTTON_WIDTH,
            y: button_y,
            width: BUTTON_WIDTH,
            height: BUTTON_HEIGHT,
        };
        let reload = Rect {
            x: edit.x - BUTTON_GAP - BUTTON_WIDTH,
            ..edit
        };
        Layout {
            header,
            banner,
            list,
            detail,
            reload,
            edit,
        }
    }

    /// Where a running program's output goes, to say so in the pane.
    fn output_note(&self, row: &Row) -> Option<String> {
        (row.entry.kind() == Kind::Program && row.state == State::Running).then(|| {
            format!(
                "What it prints goes to {}",
                process::log_path(&self.logs, &row.entry.name).display()
            )
        })
    }

    /// How tall the pane at the bottom wants to be: what the selected entry
    /// does, and room for the last lines it printed.
    fn detail_wanted(&self) -> f32 {
        let Some(row) = self.rows.get(self.selected) else {
            return 0.0;
        };
        let lines = if row.lines.is_empty() {
            usize::from(self.output_note(row).is_some())
        } else {
            row.lines.len().min(DETAIL_LINES)
        };
        if lines == 0 {
            DETAIL_BASE
        } else {
            DETAIL_BASE + 6.0 + lines as f32 * LINE_HEIGHT
        }
    }

    fn max_scroll(&self, layout: &Layout) -> f32 {
        (self.content_height - layout.list.height).max(0.0)
    }

    /// The row of an entry placed at `y` in the list, in window space.
    fn row_rect(&self, layout: &Layout, y: f32) -> Rect {
        Rect {
            x: layout.list.x + PAD,
            y: layout.list.y + y - self.scroll,
            width: (layout.list.width - 2.0 * PAD).max(0.0),
            height: ROW_HEIGHT,
        }
    }

    /// The buttons of the entry in `row`, each with its place.
    fn buttons(&self, index: usize, row: Rect) -> impl Iterator<Item = (Action, Rect)> + '_ {
        let actions = self.rows[index].actions();
        let count = actions.len();
        actions.iter().enumerate().map(move |(at, action)| {
            let from_right = (count - 1 - at) as f32;
            let rect = Rect {
                x: row.right() - 12.0 - BUTTON_WIDTH - from_right * (BUTTON_WIDTH + BUTTON_GAP),
                y: row.y + (ROW_HEIGHT - BUTTON_HEIGHT) / 2.0,
                width: BUTTON_WIDTH,
                height: BUTTON_HEIGHT,
            };
            (*action, rect)
        })
    }

    fn hit(&self, layout: &Layout, point: Point) -> Option<Target> {
        if layout.reload.contains(point) {
            return Some(Target::Reload);
        }
        if layout.edit.contains(point) {
            return Some(Target::Edit);
        }
        if !layout.list.contains(point) {
            return None;
        }
        self.placed.iter().find_map(|placed| {
            let Placed::Entry { row: index, y } = placed else {
                return None;
            };
            let row = self.row_rect(layout, *y);
            if !row.contains(point) {
                return None;
            }
            Some(
                self.buttons(*index, row)
                    .find(|(_, rect)| rect.contains(point))
                    .map_or(Target::Row(*index), |(action, _)| {
                        Target::Act(*index, action)
                    }),
            )
        })
    }

    /// One frame of input for a window of `size`.
    pub fn frame(&mut self, input: &InputSnapshot, size: (f32, f32)) {
        self.size = size;
        let layout = self.layout();
        self.pointer = Point {
            x: input.mouse_x,
            y: input.mouse_y,
        };
        let primary = input.mouse_button == PointerButton::Primary;
        if input.mouse_pressed && primary {
            self.pressed = self.hit(&layout, self.pointer);
            // A press anywhere but the armed Stop takes the question back.
            if let Some(armed) = self.armed {
                if self.pressed != Some(Target::Act(armed, Action::Stop)) {
                    self.armed = None;
                }
            }
        }
        if input.mouse_released && primary {
            let released_on = self.hit(&layout, self.pointer);
            if let Some(target) = self.pressed.take() {
                if released_on == Some(target) {
                    self.activate(target);
                }
            }
        }
        if input.mouse_wheel_y != 0.0 && layout.list.contains(self.pointer) {
            self.scroll += if input.mouse_wheel_precise {
                -input.mouse_wheel_y
            } else {
                -input.mouse_wheel_y * ROW_HEIGHT
            };
        }
        for event in &input.key_events {
            match event.key {
                HostKey::Up => self.select(self.selected.saturating_sub(1)),
                HostKey::Down => self.select(self.selected + 1),
                HostKey::Enter => {
                    // Enter starts and opens; it never stops anything.
                    let action = self.rows.get(self.selected).and_then(|row| {
                        row.actions()
                            .iter()
                            .copied()
                            .find(|action| *action != Action::Stop)
                    });
                    if let Some(action) = action {
                        self.perform(self.selected, action);
                    }
                }
                HostKey::Escape => self.armed = None,
                HostKey::R if event.modifiers.meta || event.modifiers.ctrl => {
                    self.effects.push(Effect::LoadCatalog);
                }
                _ => {}
            }
        }
        let layout = self.layout();
        self.scroll = self.scroll.clamp(0.0, self.max_scroll(&layout));
    }

    /// Selects an entry and scrolls it into view.
    fn select(&mut self, index: usize) {
        if self.rows.is_empty() {
            return;
        }
        self.selected = index.min(self.rows.len() - 1);
        self.armed = None;
        let layout = self.layout();
        let y = self.placed.iter().find_map(|placed| match placed {
            Placed::Entry { row, y } if *row == self.selected => Some(*y),
            _ => None,
        });
        if let Some(y) = y {
            let top = y - HEADING_HEIGHT;
            let bottom = y + ROW_HEIGHT + PAD;
            if top < self.scroll {
                self.scroll = top.max(0.0);
            } else if bottom > self.scroll + layout.list.height {
                self.scroll = bottom - layout.list.height;
            }
        }
    }

    fn activate(&mut self, target: Target) {
        match target {
            Target::Row(index) => {
                self.selected = index;
                self.armed = None;
            }
            Target::Act(index, action) => {
                self.selected = index;
                self.perform(index, action);
            }
            Target::Reload => self.effects.push(Effect::LoadCatalog),
            Target::Edit => self.effects.push(Effect::EditCatalog),
        }
    }

    fn perform(&mut self, index: usize, action: Action) {
        let Some(row) = self.rows.get_mut(index) else {
            return;
        };
        match action {
            Action::Start => {
                if row.state.busy() || row.entry.run.is_none() {
                    return;
                }
                let run = self.next_run;
                self.next_run += 1;
                row.run = Some(run);
                row.lines.clear();
                row.state = if row.entry.build.is_some() {
                    State::Building
                } else {
                    State::Starting
                };
                self.armed = None;
                self.banner = None;
                self.effects.push(Effect::Start {
                    run,
                    entry: Box::new(row.entry.clone()),
                });
            }
            Action::Open => {
                self.armed = None;
                self.banner = None;
                if let Some(target) = row.entry.open.clone() {
                    self.effects.push(Effect::Open { target });
                }
            }
            Action::Stop => {
                // A program in its own window may hold unsaved work, so its
                // Stop asks once more.
                let asks = row.entry.kind() == Kind::Program && row.state == State::Running;
                if asks && self.armed != Some(index) {
                    self.armed = Some(index);
                    return;
                }
                self.armed = None;
                if let Some(run) = row.run {
                    row.state = State::Stopping;
                    self.effects.push(Effect::Stop { run });
                }
            }
        }
    }

    /// The line under an entry's name: what it is, or what it is doing.
    fn status(row: &Row) -> (String, ui_core::Color) {
        let last = row.lines.back().map(String::as_str);
        match &row.state {
            State::Idle => (row.entry.about.clone(), draw::DIM_TEXT),
            State::Building => (
                match last {
                    Some(line) => format!("Building: {}", line.trim_start()),
                    None => "Building…".to_string(),
                },
                draw::DIM_TEXT,
            ),
            State::Starting => (
                match (row.entry.kind(), last) {
                    (Kind::Service, Some(line)) => format!("Starting: {}", line.trim_start()),
                    _ => "Starting…".to_string(),
                },
                draw::DIM_TEXT,
            ),
            State::Running => (
                match (row.entry.kind(), &row.entry.open) {
                    (Kind::Service, Some(open)) => format!("Running at {open}"),
                    _ => "Running in its own window".to_string(),
                },
                draw::DIM_TEXT,
            ),
            State::Stopping => ("Stopping…".to_string(), draw::DIM_TEXT),
            State::Failed(why) => (why.clone(), draw::FAILED),
        }
    }

    pub fn paint(&self, scene: &mut Vec<PaintOp>) {
        let layout = self.layout();
        let hot = self.hit(&layout, self.pointer);
        self.paint_list(scene, &layout, hot);
        // The header and the detail pane are painted over the list's edges,
        // which is what clips a row scrolled half out of view.
        self.paint_header(scene, &layout, hot);
        self.paint_detail(scene, &layout);
    }

    fn paint_header(&self, scene: &mut Vec<PaintOp>, layout: &Layout, hot: Option<Target>) {
        draw::fill(scene, layout.header, draw::HEADER);
        draw::text(
            scene,
            Rect {
                x: PAD,
                y: 0.0,
                width: 110.0,
                height: HEADER_HEIGHT,
            },
            "loadngo",
            draw::TITLE_FONT,
            draw::TEXT,
            HorizontalAlign::Left,
        );
        let busy = self.busy();
        if busy > 0 {
            draw::text(
                scene,
                Rect {
                    x: PAD + 110.0,
                    y: 2.0,
                    width: (layout.reload.x - PAD * 2.0 - 110.0).max(0.0),
                    height: HEADER_HEIGHT,
                },
                format!("{busy} running"),
                draw::FONT,
                draw::DIM_TEXT,
                HorizontalAlign::Left,
            );
        }
        draw::button(
            scene,
            layout.reload,
            "Reload",
            Tone::Plain,
            hot == Some(Target::Reload),
        );
        draw::button(
            scene,
            layout.edit,
            "Edit list",
            Tone::Plain,
            hot == Some(Target::Edit),
        );
        draw::line(
            scene,
            Point {
                x: 0.0,
                y: HEADER_HEIGHT,
            },
            Point {
                x: layout.header.width,
                y: HEADER_HEIGHT,
            },
            draw::EDGE,
        );
        if let Some(banner) = &self.banner {
            draw::fill(scene, layout.banner, draw::PANEL);
            draw::text(
                scene,
                Rect {
                    x: PAD,
                    width: (layout.banner.width - 2.0 * PAD).max(0.0),
                    ..layout.banner
                },
                banner.as_str(),
                draw::FONT,
                draw::FAILED,
                HorizontalAlign::Left,
            );
        }
    }

    fn paint_list(&self, scene: &mut Vec<PaintOp>, layout: &Layout, hot: Option<Target>) {
        draw::fill(
            scene,
            Rect {
                x: 0.0,
                y: 0.0,
                width: self.size.0,
                height: self.size.1,
            },
            draw::BACKGROUND,
        );
        if self.rows.is_empty() {
            self.paint_empty(scene, layout);
            return;
        }
        for placed in &self.placed {
            match placed {
                Placed::Heading { text, y } => {
                    let rect = Rect {
                        x: layout.list.x + PAD + 2.0,
                        y: layout.list.y + y - self.scroll,
                        width: (layout.list.width - 2.0 * PAD).max(0.0),
                        height: HEADING_HEIGHT,
                    };
                    if rect.bottom() > layout.list.y && rect.y < layout.list.bottom() {
                        draw::text(
                            scene,
                            Rect {
                                y: rect.y + 6.0,
                                height: HEADING_HEIGHT - 6.0,
                                ..rect
                            },
                            text.to_uppercase(),
                            draw::SMALL_FONT,
                            draw::HEADING_TEXT,
                            HorizontalAlign::Left,
                        );
                    }
                }
                Placed::Entry { row: index, y } => {
                    let rect = self.row_rect(layout, *y);
                    if rect.bottom() > layout.list.y && rect.y < layout.list.bottom() {
                        self.paint_row(scene, *index, rect, hot);
                    }
                }
            }
        }
    }

    fn paint_row(&self, scene: &mut Vec<PaintOp>, index: usize, rect: Rect, hot: Option<Target>) {
        let row = &self.rows[index];
        let selected = index == self.selected;
        let over = matches!(hot, Some(Target::Row(at) | Target::Act(at, _)) if at == index);
        draw::fill(
            scene,
            rect,
            if selected {
                draw::PANEL_SELECTED
            } else if over {
                draw::PANEL_HOT
            } else {
                draw::PANEL
            },
        );
        if selected {
            draw::fill(scene, Rect { width: 3.0, ..rect }, draw::ACCENT);
        }
        draw::dot(
            scene,
            Point {
                x: rect.x + 20.0,
                y: rect.y + ROW_HEIGHT / 2.0,
            },
            5.0,
            match &row.state {
                State::Idle => draw::IDLE_DOT,
                State::Building | State::Starting | State::Stopping => draw::BUSY_DOT,
                State::Running => draw::UP_DOT,
                State::Failed(_) => draw::FAILED,
            },
        );
        let buttons_width = row.actions().len() as f32 * (BUTTON_WIDTH + BUTTON_GAP) + 12.0;
        let text_x = rect.x + 38.0;
        let text_width = (rect.right() - buttons_width - text_x).max(0.0);
        let (status, status_color) = Self::status(row);
        let name = Rect {
            x: text_x,
            y: rect.y + if status.is_empty() { 0.0 } else { 8.0 },
            width: text_width,
            height: if status.is_empty() { ROW_HEIGHT } else { 24.0 },
        };
        draw::text(
            scene,
            name,
            row.entry.name.as_str(),
            draw::NAME_FONT,
            draw::TEXT,
            HorizontalAlign::Left,
        );
        if !status.is_empty() {
            draw::text(
                scene,
                Rect {
                    x: text_x,
                    y: rect.y + 31.0,
                    width: text_width,
                    height: 19.0,
                },
                status,
                draw::SMALL_FONT,
                status_color,
                HorizontalAlign::Left,
            );
        }
        for (action, place) in self.buttons(index, rect) {
            let armed = action == Action::Stop && self.armed == Some(index);
            let (label, tone) = match action {
                Action::Start => ("Start", Tone::Main),
                Action::Open => ("Open", Tone::Main),
                Action::Stop if armed => ("Stop it?", Tone::Warn),
                Action::Stop => ("Stop", Tone::Plain),
            };
            draw::button(
                scene,
                place,
                label,
                tone,
                hot == Some(Target::Act(index, action)),
            );
        }
    }

    /// The list's place when there is nothing in it.
    fn paint_empty(&self, scene: &mut Vec<PaintOp>, layout: &Layout) {
        let lines: Vec<(String, ui_core::Color, u16)> = if !self.loaded {
            vec![("Reading the list…".to_string(), draw::DIM_TEXT, draw::FONT)]
        } else if self.missing || self.banner.is_none() {
            vec![
                (
                    "Nothing to launch yet".to_string(),
                    draw::TEXT,
                    draw::NAME_FONT,
                ),
                (
                    "What appears here is listed in".to_string(),
                    draw::DIM_TEXT,
                    draw::FONT,
                ),
                (
                    self.catalog_path.display().to_string(),
                    draw::DIM_TEXT,
                    draw::FONT,
                ),
                (
                    "Edit list opens it. Its format is in loadngo's docs/LAUNCHER.md.".to_string(),
                    draw::DIM_TEXT,
                    draw::FONT,
                ),
            ]
        } else {
            vec![(
                self.catalog_path.display().to_string(),
                draw::DIM_TEXT,
                draw::FONT,
            )]
        };
        let line_height = 26.0;
        let mut y = layout.list.y + (layout.list.height - lines.len() as f32 * line_height) / 2.0;
        for (line, color, font) in lines {
            draw::text(
                scene,
                Rect {
                    x: PAD,
                    y,
                    width: (layout.list.width - 2.0 * PAD).max(0.0),
                    height: line_height,
                },
                line,
                font,
                color,
                HorizontalAlign::Center,
            );
            y += line_height;
        }
    }

    /// What the selected entry does, and the last of what it printed.
    fn paint_detail(&self, scene: &mut Vec<PaintOp>, layout: &Layout) {
        let Some(row) = self.rows.get(self.selected) else {
            return;
        };
        if layout.detail.height <= 0.0 {
            return;
        }
        draw::fill(scene, layout.detail, draw::HEADER);
        draw::line(
            scene,
            Point {
                x: 0.0,
                y: layout.detail.y,
            },
            Point {
                x: layout.detail.width,
                y: layout.detail.y,
            },
            draw::EDGE,
        );
        let entry = &row.entry;
        let does = match (entry.kind(), &entry.run, &entry.open) {
            (Kind::Service, Some(run), Some(open)) => {
                format!("Runs {} and opens {open}", run.display())
            }
            (_, Some(run), _) => format!("Starts {}", run.display()),
            (_, None, Some(open)) => format!("Opens {open}"),
            (_, None, None) => String::new(),
        };
        let more = match (
            &entry.build,
            entry.run.as_ref().and_then(|run| run.dir.as_ref()),
        ) {
            (Some(build), _) => format!("Builds first: {}", build.display()),
            (None, Some(dir)) => format!("In {}", dir.display()),
            (None, None) => String::new(),
        };
        let x = PAD;
        let width = (layout.detail.width - 2.0 * PAD).max(0.0);
        let mut y = layout.detail.y + 10.0;
        for (line, color) in [(does, draw::TEXT), (more, draw::DIM_TEXT)] {
            if line.is_empty() {
                continue;
            }
            draw::text(
                scene,
                Rect {
                    x,
                    y,
                    width,
                    height: 20.0,
                },
                line,
                draw::FONT,
                color,
                HorizontalAlign::Left,
            );
            y += 20.0;
        }
        y += 6.0;
        let room = ((layout.detail.bottom() - 8.0 - y) / LINE_HEIGHT)
            .floor()
            .max(0.0) as usize;
        if row.lines.is_empty() {
            if let (Some(note), true) = (self.output_note(row), room > 0) {
                draw::text(
                    scene,
                    Rect {
                        x,
                        y,
                        width,
                        height: LINE_HEIGHT,
                    },
                    note,
                    draw::SMALL_FONT,
                    draw::DIM_TEXT,
                    HorizontalAlign::Left,
                );
            }
            return;
        }
        let failed = matches!(row.state, State::Failed(_));
        let skip = row.lines.len().saturating_sub(room);
        for line in row.lines.iter().skip(skip) {
            draw::text(
                scene,
                Rect {
                    x,
                    y,
                    width,
                    height: LINE_HEIGHT,
                },
                line.as_str(),
                draw::SMALL_FONT,
                if failed { draw::TEXT } else { draw::DIM_TEXT },
                HorizontalAlign::Left,
            );
            y += LINE_HEIGHT;
        }
    }

    /// The catalog file's path, for the host loop.
    pub fn catalog_path(&self) -> &Path {
        &self.catalog_path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog;
    use loadngo_host_core::HostKeyEvent;

    const SIZE: (f32, f32) = (760.0, 640.0);

    const CATALOG: &str = r#"{ "entries": [
        { "name": "Editor", "group": "Tools", "about": "Edit source",
          "build": { "program": "cargo", "args": ["build"] },
          "run": { "program": "/tools/editor" } },
        { "name": "Site", "group": "Pages",
          "run": { "program": "npm", "args": ["run", "dev"] },
          "open": "http://localhost:5173/", "ready": "Local:" },
        { "name": "Demo", "group": "Pages", "open": "https://example.org/" }
    ] }"#;

    struct Rig {
        app: LauncherApp,
    }

    impl Rig {
        fn new() -> Self {
            let mut app = LauncherApp::new("/data/catalog.json".into(), "/data/logs".into());
            assert_eq!(app.take_effects(), [Effect::LoadCatalog]);
            app.apply_catalog(Loaded::Catalog(catalog::parse(CATALOG, None).unwrap()));
            let mut rig = Self { app };
            rig.frame(InputSnapshot::default());
            rig
        }

        fn frame(&mut self, input: InputSnapshot) -> Vec<Effect> {
            self.app.frame(&input, SIZE);
            let mut scene = Vec::new();
            self.app.paint(&mut scene);
            self.app.take_effects()
        }

        fn row_of(&self, name: &str) -> usize {
            self.app
                .rows
                .iter()
                .position(|row| row.entry.name == name)
                .unwrap()
        }

        fn point(&self, name: &str, action: Option<Action>) -> Point {
            let index = self.row_of(name);
            let layout = self.app.layout();
            let y = self
                .app
                .placed
                .iter()
                .find_map(|placed| match placed {
                    Placed::Entry { row, y } if *row == index => Some(*y),
                    _ => None,
                })
                .unwrap();
            let row = self.app.row_rect(&layout, y);
            let rect = match action {
                Some(action) => {
                    self.app
                        .buttons(index, row)
                        .find(|(found, _)| *found == action)
                        .unwrap_or_else(|| panic!("{name} has no {action:?} button"))
                        .1
                }
                None => Rect {
                    width: 100.0,
                    ..row
                },
            };
            Point {
                x: rect.x + rect.width / 2.0,
                y: rect.y + rect.height / 2.0,
            }
        }

        fn click_at(&mut self, point: Point) -> Vec<Effect> {
            let mut effects = self.frame(InputSnapshot {
                mouse_x: point.x,
                mouse_y: point.y,
                mouse_pressed: true,
                mouse_down: true,
                ..Default::default()
            });
            effects.extend(self.frame(InputSnapshot {
                mouse_x: point.x,
                mouse_y: point.y,
                mouse_released: true,
                ..Default::default()
            }));
            effects
        }

        fn click(&mut self, name: &str, action: Action) -> Vec<Effect> {
            let point = self.point(name, Some(action));
            self.click_at(point)
        }

        fn key(&mut self, key: HostKey) -> Vec<Effect> {
            self.frame(InputSnapshot {
                mouse_x: self.app.pointer.x,
                mouse_y: self.app.pointer.y,
                key_events: vec![HostKeyEvent {
                    key,
                    modifiers: Default::default(),
                }],
                ..Default::default()
            })
        }

        fn happen(&mut self, run: u64, what: What) -> Vec<Effect> {
            self.app.apply_process(Event { run, what });
            self.frame(InputSnapshot {
                mouse_x: self.app.pointer.x,
                mouse_y: self.app.pointer.y,
                ..Default::default()
            })
        }

        fn state(&self, name: &str) -> State {
            self.app.state_of(name).unwrap().clone()
        }

        /// Every line of text the window shows.
        fn shown(&self) -> Vec<String> {
            let mut scene = Vec::new();
            self.app.paint(&mut scene);
            scene
                .into_iter()
                .filter_map(|op| match op {
                    PaintOp::Text { text, .. } => Some(text),
                    _ => None,
                })
                .collect()
        }

        fn started(effects: &[Effect]) -> u64 {
            match effects {
                [Effect::Start { run, .. }] => *run,
                other => panic!("one start, not {other:?}"),
            }
        }
    }

    #[test]
    fn a_program_builds_then_runs_and_its_stop_asks_twice() {
        let mut rig = Rig::new();
        let run = Rig::started(&rig.click("Editor", Action::Start));
        assert_eq!(rig.state("Editor"), State::Building);
        rig.happen(run, What::Building);
        rig.happen(run, What::Line("   Compiling ui-core v0.1.0".to_string()));
        assert!(rig
            .shown()
            .contains(&"Building: Compiling ui-core v0.1.0".to_string()));
        assert_eq!(rig.app.busy(), 1);
        rig.happen(run, What::Started);
        assert_eq!(rig.state("Editor"), State::Running);
        assert!(rig
            .shown()
            .contains(&"Running in its own window".to_string()));

        // The first press on Stop only asks.
        assert_eq!(rig.click("Editor", Action::Stop), []);
        assert!(rig.shown().contains(&"Stop it?".to_string()));
        // A press elsewhere takes the question back.
        let elsewhere = rig.point("Demo", None);
        rig.click_at(elsewhere);
        assert!(!rig.shown().contains(&"Stop it?".to_string()));
        assert_eq!(rig.state("Editor"), State::Running);
        // Two presses stop it.
        rig.click("Editor", Action::Stop);
        assert_eq!(rig.click("Editor", Action::Stop), [Effect::Stop { run }]);
        assert_eq!(rig.state("Editor"), State::Stopping);
        rig.happen(run, What::Ended(Ending::Stopped));
        assert_eq!(rig.state("Editor"), State::Idle);
        assert_eq!(rig.app.busy(), 0);
    }

    #[test]
    fn a_service_opens_its_page_when_ready_and_can_reopen_it() {
        let mut rig = Rig::new();
        let run = Rig::started(&rig.click("Site", Action::Start));
        assert_eq!(rig.state("Site"), State::Starting);
        assert_eq!(rig.happen(run, What::Started), []);
        rig.happen(run, What::Line("VITE ready in 300 ms".to_string()));
        assert!(rig
            .shown()
            .contains(&"Starting: VITE ready in 300 ms".to_string()));
        let open = Effect::Open {
            target: "http://localhost:5173/".to_string(),
        };
        assert_eq!(rig.happen(run, What::Ready), vec![open.clone()]);
        assert_eq!(rig.state("Site"), State::Running);
        assert_eq!(rig.click("Site", Action::Open), [open]);
        // A service's Stop needs one press: nothing of the user's is in it.
        assert_eq!(rig.click("Site", Action::Stop), [Effect::Stop { run }]);
        // Its page does not open if it comes ready while stopping.
        assert_eq!(rig.happen(run, What::Ready), []);
        rig.happen(run, What::Ended(Ending::Stopped));
        assert_eq!(rig.state("Site"), State::Idle);
    }

    #[test]
    fn a_failure_shows_why_and_the_last_output() {
        let mut rig = Rig::new();
        let run = Rig::started(&rig.click("Editor", Action::Start));
        rig.happen(run, What::Building);
        rig.happen(
            run,
            What::Line("error[E0425]: cannot find value".to_string()),
        );
        rig.happen(
            run,
            What::Ended(Ending::Failed {
                why: "The build failed (exit status: 101).".to_string(),
                tail: vec!["error[E0425]: cannot find value".to_string()],
            }),
        );
        assert_eq!(
            rig.state("Editor"),
            State::Failed("The build failed (exit status: 101).".to_string())
        );
        let shown = rig.shown();
        assert!(shown.contains(&"The build failed (exit status: 101).".to_string()));
        // Once, not twice: the lines came as they were printed.
        assert_eq!(
            shown
                .iter()
                .filter(|line| *line == "error[E0425]: cannot find value")
                .count(),
            1
        );
        // It can be started again, and that clears the old output.
        let again = Rig::started(&rig.click("Editor", Action::Start));
        assert_ne!(again, run);
        assert!(!rig
            .shown()
            .contains(&"error[E0425]: cannot find value".to_string()));

        // A program's failure brings the end of its log.
        rig.happen(again, What::Started);
        rig.happen(
            again,
            What::Ended(Ending::Failed {
                why: "It ended with a failure (exit status: 1).".to_string(),
                tail: vec!["panicked at main.rs:3".to_string()],
            }),
        );
        assert!(rig.shown().contains(&"panicked at main.rs:3".to_string()));
    }

    #[test]
    fn a_link_only_opens() {
        let mut rig = Rig::new();
        assert_eq!(
            rig.click("Demo", Action::Open),
            [Effect::Open {
                target: "https://example.org/".to_string()
            }]
        );
        assert_eq!(rig.state("Demo"), State::Idle);
        rig.app
            .opened("https://example.org/", Err("no browser".to_string()));
        assert!(rig
            .shown()
            .contains(&"Could not open https://example.org/: no browser".to_string()));
    }

    #[test]
    fn keys_move_the_selection_and_enter_starts_but_never_stops() {
        let mut rig = Rig::new();
        assert_eq!(rig.app.selected, 0);
        rig.key(HostKey::Down);
        rig.key(HostKey::Down);
        rig.key(HostKey::Down);
        assert_eq!(rig.app.selected, 2);
        assert_eq!(
            rig.key(HostKey::Enter),
            [Effect::Open {
                target: "https://example.org/".to_string()
            }]
        );
        rig.key(HostKey::Up);
        rig.key(HostKey::Up);
        rig.key(HostKey::Up);
        assert_eq!(rig.app.selected, 0);
        let run = Rig::started(&rig.key(HostKey::Enter));
        rig.happen(run, What::Started);
        assert_eq!(rig.state("Editor"), State::Running);
        assert_eq!(rig.key(HostKey::Enter), []);
        assert_eq!(rig.state("Editor"), State::Running);
        assert_eq!(rig.app.armed, None);
    }

    #[test]
    fn a_press_released_elsewhere_does_nothing() {
        let mut rig = Rig::new();
        let on = rig.point("Editor", Some(Action::Start));
        let off = rig.point("Demo", None);
        rig.frame(InputSnapshot {
            mouse_x: on.x,
            mouse_y: on.y,
            mouse_pressed: true,
            mouse_down: true,
            ..Default::default()
        });
        let effects = rig.frame(InputSnapshot {
            mouse_x: off.x,
            mouse_y: off.y,
            mouse_released: true,
            ..Default::default()
        });
        assert_eq!(effects, []);
        assert_eq!(rig.state("Editor"), State::Idle);
    }

    #[test]
    fn reloading_keeps_what_is_running_and_stops_a_service_that_has_gone() {
        let mut rig = Rig::new();
        let editor = Rig::started(&rig.click("Editor", Action::Start));
        rig.happen(editor, What::Started);
        let site = Rig::started(&rig.click("Site", Action::Start));
        rig.happen(site, What::Started);
        rig.happen(site, What::Ready);
        rig.app.take_effects();

        // The header's Reload asks for the file again.
        let layout = rig.app.layout();
        let reload = Point {
            x: layout.reload.x + 5.0,
            y: layout.reload.y + 5.0,
        };
        assert_eq!(rig.click_at(reload), [Effect::LoadCatalog]);

        // A file that cannot be read changes nothing but the banner.
        rig.app
            .apply_catalog(Loaded::Wrong("expected `,` at line 3".to_string()));
        assert_eq!(rig.state("Editor"), State::Running);
        assert!(rig
            .shown()
            .contains(&"The list could not be read: expected `,` at line 3".to_string()));

        // The new list has the editor (with a new description) and no site.
        let next = catalog::parse(
            r#"{ "entries": [
                { "name": "New", "open": "https://example.org/new" },
                { "name": "Editor", "about": "Edit anything", "run": { "program": "/tools/editor" } }
            ] }"#,
            None,
        )
        .unwrap();
        rig.app.apply_catalog(Loaded::Catalog(next));
        assert_eq!(rig.app.take_effects(), [Effect::Stop { run: site }]);
        assert_eq!(rig.state("Editor"), State::Running);
        assert!(rig.app.state_of("Site").is_none());
        // Its end still finds it by its start number.
        rig.happen(editor, What::Ended(Ending::Finished));
        assert_eq!(rig.state("Editor"), State::Idle);
        assert!(rig.shown().contains(&"Edit anything".to_string()));
        // An event for a start that is no longer listed is ignored.
        rig.happen(site, What::Ended(Ending::Stopped));
    }

    #[test]
    fn an_empty_or_missing_list_says_where_the_file_goes() {
        let mut app = LauncherApp::new("/data/catalog.json".into(), "/data/logs".into());
        app.frame(&InputSnapshot::default(), SIZE);
        let shown = |app: &LauncherApp| {
            let mut scene = Vec::new();
            app.paint(&mut scene);
            scene
                .into_iter()
                .filter_map(|op| match op {
                    PaintOp::Text { text, .. } => Some(text),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        assert!(shown(&app).contains(&"Reading the list…".to_string()));
        app.apply_catalog(Loaded::Missing);
        assert!(shown(&app).contains(&"/data/catalog.json".to_string()));
        assert!(shown(&app).contains(&"Nothing to launch yet".to_string()));
        // Edit list asks for the file to be made and opened.
        let layout = app.layout();
        let edit = Point {
            x: layout.edit.x + 5.0,
            y: layout.edit.y + 5.0,
        };
        app.take_effects();
        app.frame(
            &InputSnapshot {
                mouse_x: edit.x,
                mouse_y: edit.y,
                mouse_pressed: true,
                mouse_released: true,
                ..Default::default()
            },
            SIZE,
        );
        assert_eq!(app.take_effects(), [Effect::EditCatalog]);
    }

    #[test]
    fn an_entry_named_at_the_start_is_started_once_the_list_is_read() {
        let mut app = LauncherApp::new("/data/catalog.json".into(), "/data/logs".into());
        app.start_when_loaded("Site".to_string());
        app.start_when_loaded("Demo".to_string());
        app.take_effects();
        app.frame(&InputSnapshot::default(), SIZE);
        app.apply_catalog(Loaded::Catalog(catalog::parse(CATALOG, None).unwrap()));
        let effects = app.take_effects();
        assert!(
            matches!(
                &effects[..],
                [Effect::Start { entry, .. }, Effect::Open { target }]
                    if entry.name == "Site" && target == "https://example.org/"
            ),
            "{effects:?}"
        );
        assert_eq!(app.rows[app.selected].entry.name, "Demo");
        // Once: a reload does not start it again.
        app.apply_catalog(Loaded::Catalog(catalog::parse(CATALOG, None).unwrap()));
        assert_eq!(app.take_effects(), []);

        let mut app = LauncherApp::new("/data/catalog.json".into(), "/data/logs".into());
        app.start_when_loaded("Nothing".to_string());
        app.take_effects();
        app.apply_catalog(Loaded::Catalog(catalog::parse(CATALOG, None).unwrap()));
        assert_eq!(app.take_effects(), []);
        assert_eq!(
            app.banner.as_deref(),
            Some("There is no entry called \"Nothing\" to start.")
        );
    }

    #[test]
    fn a_long_list_scrolls_and_the_keys_keep_the_selection_in_view() {
        let entries: Vec<String> = (0..40)
            .map(|n| format!(r#"{{ "name": "Entry {n}", "open": "https://example.org/{n}" }}"#))
            .collect();
        let text = format!(r#"{{ "entries": [{}] }}"#, entries.join(","));
        let mut app = LauncherApp::new("/data/catalog.json".into(), "/data/logs".into());
        app.apply_catalog(Loaded::Catalog(catalog::parse(&text, None).unwrap()));
        app.frame(&InputSnapshot::default(), SIZE);
        let layout = app.layout();
        let over_list = Point {
            x: 100.0,
            y: layout.list.y + 50.0,
        };
        app.frame(
            &InputSnapshot {
                mouse_x: over_list.x,
                mouse_y: over_list.y,
                mouse_wheel_y: -3.0,
                ..Default::default()
            },
            SIZE,
        );
        assert_eq!(app.scroll, 3.0 * ROW_HEIGHT);
        // Far past the end stops at the end.
        app.frame(
            &InputSnapshot {
                mouse_x: over_list.x,
                mouse_y: over_list.y,
                mouse_wheel_y: -100_000.0,
                mouse_wheel_precise: true,
                ..Default::default()
            },
            SIZE,
        );
        assert_eq!(app.scroll, app.content_height - layout.list.height);
        // Up to the first entry brings the top back.
        for _ in 0..40 {
            app.frame(
                &InputSnapshot {
                    key_events: vec![HostKeyEvent {
                        key: HostKey::Up,
                        modifiers: Default::default(),
                    }],
                    ..Default::default()
                },
                SIZE,
            );
        }
        assert_eq!(app.selected, 0);
        assert_eq!(app.scroll, 0.0);
        app.frame(
            &InputSnapshot {
                key_events: vec![HostKeyEvent {
                    key: HostKey::Down,
                    modifiers: Default::default(),
                }],
                ..Default::default()
            },
            SIZE,
        );
        assert_eq!(app.selected, 1);
    }
}
