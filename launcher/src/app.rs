//! The launcher's window: the catalog as a list, what each entry is doing,
//! and what the pointer and the keys do to it.
//!
//! The app owns no threads and does no blocking work. What it wants done
//! leaves as [`Effect`]s for the host loop (`host.rs`) to carry out, and what
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
/// On a phone a line holds half as much, so the banner has two: what went
/// wrong, and why.
const TALL_BANNER_HEIGHT: f32 = 48.0;
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
/// How far a finger travels before its press on the list becomes a drag.
const DRAG_SLOP: f32 = 8.0;

/// What the machine the launcher is on lets it do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Abilities {
    /// It can build and start programs. A phone cannot: an app there may
    /// only ask the system to open an address.
    pub starts_programs: bool,
    /// The catalog file can be opened in an editor on this machine.
    pub edits_catalog: bool,
    /// It is worked with fingers: the list is dragged, a button answers a
    /// touch anywhere in the height of its row, and nothing is under a
    /// pointer between touches.
    pub touch: bool,
}

impl Abilities {
    /// A computer with a pointer and a keyboard.
    pub const DESKTOP: Self = Self {
        starts_programs: true,
        edits_catalog: true,
        touch: false,
    };
    /// A phone or a tablet.
    pub const PHONE: Self = Self {
        starts_programs: false,
        edits_catalog: false,
        touch: true,
    };

    /// Whether `entry` can be started or opened here. What is built into
    /// the launcher can be anywhere the launcher runs.
    fn offers(self, entry: &Entry) -> bool {
        self.starts_programs || matches!(entry.kind(), Kind::Link | Kind::BuiltIn)
    }
}

/// The edges of the window the system keeps for itself (a phone's status
/// bar, camera housing and home indicator), in the window's own units.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Edges {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

/// A finger down on the list: where it landed and where the list was then.
#[derive(Debug, Clone, Copy)]
struct Drag {
    from: f32,
    scroll: f32,
    moved: bool,
}

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
    /// Give the window to the built-in entry called `name` until it is
    /// left.
    Play {
        name: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Start,
    Open,
    Stop,
    Play,
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
    /// This machine can start or open it.
    here: bool,
    state: State,
    /// The start it is on, while it has one.
    run: Option<u64>,
    /// The last lines a build or a service printed, or the end of a failed
    /// program's log.
    lines: VecDeque<String>,
}

impl Row {
    fn new(entry: Entry, here: bool) -> Self {
        Self {
            entry,
            here,
            state: State::Idle,
            run: None,
            lines: VecDeque::new(),
        }
    }

    /// Its buttons, left to right.
    fn actions(&self) -> &'static [Action] {
        if !self.here {
            return &[];
        }
        match (self.entry.kind(), &self.state) {
            (Kind::BuiltIn, _) => &[Action::Play],
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
    /// What this launcher has built into it, listed ahead of the catalog.
    built_ins: Vec<Entry>,
    abilities: Abilities,
    edges: Edges,
    pointer: Point,
    /// The pointer's button, or a finger, is down.
    down: bool,
    pressed: Option<Target>,
    drag: Option<Drag>,
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
            built_ins: Vec::new(),
            abilities: Abilities::DESKTOP,
            edges: Edges::default(),
            pointer: Point::default(),
            down: false,
            pressed: None,
            drag: None,
            armed: None,
            next_run: 1,
            effects: vec![Effect::LoadCatalog],
            start_when_loaded: Vec::new(),
        }
    }

    /// The same launcher on another kind of machine (a computer unless
    /// told). Say so before the catalog is read.
    pub fn with_abilities(mut self, abilities: Abilities) -> Self {
        self.abilities = abilities;
        self
    }

    /// What this launcher has built into it ([`Entry::built_in`]). They
    /// head the list whatever the catalog holds, and a catalog entry with
    /// the name of one is left out. Say so before the catalog is read.
    pub fn with_built_ins(mut self, built_ins: Vec<Entry>) -> Self {
        self.built_ins = built_ins;
        self
    }

    /// The edges of the window the system keeps for itself; nothing to read
    /// or press is put under them.
    pub fn set_edges(&mut self, edges: Edges) {
        self.edges = edges;
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
                (Some(index), None) if !self.rows[index].here => {
                    self.banner = Some(format!("\"{name}\" cannot be started on this device."));
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
        // What is built in comes first, and stands for its name: a list
        // made for a launcher without it may still link to the app.
        let listed: Vec<Entry> = catalog
            .entries
            .into_iter()
            .filter(|entry| self.built_ins.iter().all(|own| own.name != entry.name))
            .collect();
        for entry in self.built_ins.iter().cloned().chain(listed) {
            let here = self.abilities.offers(&entry);
            let kept = old
                .iter()
                .position(|row| row.entry.name == entry.name)
                .map(|at| old.swap_remove(at));
            self.rows.push(match kept {
                Some(mut row) => {
                    row.entry = entry;
                    row.here = here;
                    row
                }
                None => Row::new(entry, here),
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
        self.drag = None;
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
                        Kind::Program | Kind::Link | Kind::BuiltIn => State::Running,
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
        let edges = self.edges;
        // The header's colour runs up under the status bar; what it holds
        // starts below it.
        let header = Rect {
            x: 0.0,
            y: 0.0,
            width,
            height: edges.top + HEADER_HEIGHT,
        };
        let banner = Rect {
            x: 0.0,
            y: header.bottom(),
            width,
            height: match (&self.banner, self.abilities.touch) {
                (None, _) => 0.0,
                (Some(_), false) => BANNER_HEIGHT,
                (Some(_), true) => TALL_BANNER_HEIGHT,
            },
        };
        let wanted = self.detail_wanted();
        // Likewise the pane's colour runs down under the home indicator.
        let detail_height = if wanted > 0.0 {
            (wanted + edges.bottom)
                .min((height - banner.bottom()) * 0.5)
                .max(0.0)
        } else {
            0.0
        };
        let detail = Rect {
            x: 0.0,
            y: height - detail_height,
            width,
            height: detail_height,
        };
        let list = Rect {
            x: edges.left,
            y: banner.bottom(),
            width: (width - edges.left - edges.right).max(0.0),
            height: (detail.y - banner.bottom()).max(0.0),
        };
        let button_y = edges.top + (HEADER_HEIGHT - BUTTON_HEIGHT) / 2.0;
        // Where the list cannot be edited there is no Edit button, and
        // Reload sits where it would have been.
        let edit_width = if self.abilities.edits_catalog {
            BUTTON_WIDTH
        } else {
            -BUTTON_GAP
        };
        let edit = Rect {
            x: width - edges.right - PAD - edit_width,
            y: button_y,
            width: edit_width.max(0.0),
            height: BUTTON_HEIGHT,
        };
        let reload = Rect {
            x: edit.x - BUTTON_GAP - BUTTON_WIDTH,
            width: BUTTON_WIDTH,
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

    /// Where a button answers: its own rectangle, or under a finger the
    /// whole height of the `band` it sits in.
    fn reach(&self, button: Rect, band: Rect) -> Rect {
        if self.abilities.touch && button.width > 0.0 {
            Rect {
                y: band.y,
                height: band.height,
                ..button
            }
        } else {
            button
        }
    }

    fn hit(&self, layout: &Layout, point: Point) -> Option<Target> {
        let bar = Rect {
            y: self.edges.top,
            height: HEADER_HEIGHT,
            ..layout.header
        };
        if self.reach(layout.reload, bar).contains(point) {
            return Some(Target::Reload);
        }
        if self.reach(layout.edit, bar).contains(point) {
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
                    .find(|(_, rect)| self.reach(*rect, row).contains(point))
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
        self.down = input.mouse_down;
        let primary = input.mouse_button == PointerButton::Primary;
        if input.mouse_pressed && primary {
            self.pressed = self.hit(&layout, self.pointer);
            self.drag =
                (self.abilities.touch && layout.list.contains(self.pointer)).then_some(Drag {
                    from: self.pointer.y,
                    scroll: self.scroll,
                    moved: false,
                });
            // A press anywhere but the armed Stop takes the question back.
            if let Some(armed) = self.armed {
                if self.pressed != Some(Target::Act(armed, Action::Stop)) {
                    self.armed = None;
                }
            }
        }
        if let Some(drag) = self.drag.as_mut() {
            let travelled = self.pointer.y - drag.from;
            if travelled.abs() > DRAG_SLOP {
                drag.moved = true;
            }
            if drag.moved {
                // The finger is moving the list, not pressing what it
                // landed on.
                self.pressed = None;
                self.scroll = drag.scroll - travelled;
            }
        }
        if input.mouse_released && primary {
            self.drag = None;
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
        if !input.mouse_down {
            self.drag = None;
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
                if row.state.busy() || row.entry.run.is_none() || !row.here {
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
            Action::Play => {
                self.armed = None;
                self.banner = None;
                self.effects.push(Effect::Play {
                    name: row.entry.name.clone(),
                });
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
            State::Idle if !row.here => (
                "Starts a program, which this device cannot do".to_string(),
                draw::DIM_TEXT,
            ),
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
        // A finger is over something only while it is down and not dragging
        // the list.
        let dragging = self.drag.is_some_and(|drag| drag.moved);
        let hot = if dragging || (self.abilities.touch && !self.down) {
            None
        } else {
            self.hit(&layout, self.pointer)
        };
        self.paint_list(scene, &layout, hot);
        // The header and the detail pane are painted over the list's edges,
        // which is what clips a row scrolled half out of view.
        self.paint_header(scene, &layout, hot);
        self.paint_detail(scene, &layout);
    }

    fn paint_header(&self, scene: &mut Vec<PaintOp>, layout: &Layout, hot: Option<Target>) {
        let edges = self.edges;
        draw::fill(scene, layout.header, draw::HEADER);
        draw::text(
            scene,
            Rect {
                x: edges.left + PAD,
                y: edges.top,
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
                    x: edges.left + PAD + 110.0,
                    y: edges.top + 2.0,
                    width: (layout.reload.x - edges.left - PAD * 2.0 - 110.0).max(0.0),
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
        if self.abilities.edits_catalog {
            draw::button(
                scene,
                layout.edit,
                "Edit list",
                Tone::Plain,
                hot == Some(Target::Edit),
            );
        }
        draw::line(
            scene,
            Point {
                x: 0.0,
                y: layout.header.bottom(),
            },
            Point {
                x: layout.header.width,
                y: layout.header.bottom(),
            },
            draw::EDGE,
        );
        if let Some(banner) = &self.banner {
            draw::fill(scene, layout.banner, draw::PANEL);
            let place = Rect {
                x: edges.left + PAD,
                width: (layout.banner.width - edges.left - edges.right - 2.0 * PAD).max(0.0),
                ..layout.banner
            };
            let halves = banner.split_once(": ").filter(|_| self.abilities.touch);
            if let Some((what, why)) = halves {
                for (line, text) in [what, why].into_iter().enumerate() {
                    draw::text(
                        scene,
                        Rect {
                            y: place.y + 5.0 + line as f32 * 19.0,
                            height: 19.0,
                            ..place
                        },
                        text,
                        draw::FONT,
                        draw::FAILED,
                        HorizontalAlign::Left,
                    );
                }
            } else {
                draw::text(
                    scene,
                    place,
                    banner.as_str(),
                    draw::FONT,
                    draw::FAILED,
                    HorizontalAlign::Left,
                );
            }
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
                Action::Play => ("Play", Tone::Main),
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
        } else if !self.abilities.edits_catalog && (self.missing || self.banner.is_none()) {
            vec![
                (
                    "Nothing to launch yet".to_string(),
                    draw::TEXT,
                    draw::NAME_FONT,
                ),
                (
                    "The list is sent to this device from a computer.".to_string(),
                    draw::DIM_TEXT,
                    draw::FONT,
                ),
                (
                    "How is in loadngo's docs/LAUNCHER.md.".to_string(),
                    draw::DIM_TEXT,
                    draw::FONT,
                ),
            ]
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
                    x: layout.list.x + PAD,
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
            (Kind::BuiltIn, _, _) => "Plays in this window; leaving it comes back here".to_string(),
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
        let edges = self.edges;
        let x = edges.left + PAD;
        let width = (layout.detail.width - edges.left - edges.right - 2.0 * PAD).max(0.0);
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
        let room = ((layout.detail.bottom() - edges.bottom - 8.0 - y) / LINE_HEIGHT)
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

    /// An iPhone held upright, in points, and what its system keeps.
    const PHONE: (f32, f32) = (428.0, 926.0);
    const PHONE_EDGES: Edges = Edges {
        left: 0.0,
        top: 47.0,
        right: 0.0,
        bottom: 34.0,
    };

    fn phone(text: &str) -> LauncherApp {
        let mut app = LauncherApp::new("/data/catalog.json".into(), "/data/logs".into())
            .with_abilities(Abilities::PHONE);
        app.set_edges(PHONE_EDGES);
        app.take_effects();
        app.apply_catalog(Loaded::Catalog(catalog::parse(text, None).unwrap()));
        app.frame(&InputSnapshot::default(), PHONE);
        app
    }

    fn texts(app: &LauncherApp) -> Vec<(String, Rect)> {
        let mut scene = Vec::new();
        app.paint(&mut scene);
        scene
            .into_iter()
            .filter_map(|op| match op {
                PaintOp::Text { text, rect, .. } => Some((text, rect)),
                _ => None,
            })
            .collect()
    }

    fn touch(app: &mut LauncherApp, point: Point, pressed: bool, down: bool, released: bool) {
        app.frame(
            &InputSnapshot {
                mouse_x: point.x,
                mouse_y: point.y,
                mouse_pressed: pressed,
                mouse_down: down,
                mouse_released: released,
                ..Default::default()
            },
            PHONE,
        );
    }

    fn entry_row(app: &LauncherApp, name: &str) -> (usize, Rect) {
        let index = app
            .rows
            .iter()
            .position(|row| row.entry.name == name)
            .unwrap();
        let y = app
            .placed
            .iter()
            .find_map(|placed| match placed {
                Placed::Entry { row, y } if *row == index => Some(*y),
                _ => None,
            })
            .unwrap();
        (index, app.row_rect(&app.layout(), y))
    }

    #[test]
    fn a_phone_opens_links_and_says_a_program_cannot_be_started_there() {
        let mut app = phone(CATALOG);
        assert_eq!(app.rows[2].actions(), [Action::Open]);
        assert!(app.rows[0].actions().is_empty() && app.rows[1].actions().is_empty());
        let shown: Vec<String> = texts(&app).into_iter().map(|(text, _)| text).collect();
        assert_eq!(
            shown
                .iter()
                .filter(|text| *text == "Starts a program, which this device cannot do")
                .count(),
            2,
            "{shown:?}"
        );
        assert!(!shown.iter().any(|text| text == "Start"), "{shown:?}");
        // A phone has no editor for the list, so no button for one.
        assert!(!shown.iter().any(|text| text == "Edit list"), "{shown:?}");
        assert!(shown.iter().any(|text| text == "Reload"), "{shown:?}");
        // Enter on a program starts nothing.
        app.frame(
            &InputSnapshot {
                key_events: vec![HostKeyEvent {
                    key: HostKey::Enter,
                    modifiers: Default::default(),
                }],
                ..Default::default()
            },
            PHONE,
        );
        assert_eq!(app.take_effects(), []);

        // Asked for by name at the start, a program says why not.
        let mut app = LauncherApp::new("/data/catalog.json".into(), "/data/logs".into())
            .with_abilities(Abilities::PHONE);
        app.start_when_loaded("Editor".to_string());
        app.take_effects();
        app.apply_catalog(Loaded::Catalog(catalog::parse(CATALOG, None).unwrap()));
        assert_eq!(app.take_effects(), []);
        assert_eq!(
            app.banner.as_deref(),
            Some("\"Editor\" cannot be started on this device.")
        );
    }

    #[test]
    fn a_phone_with_no_list_says_where_one_comes_from() {
        let mut app = LauncherApp::new("/data/catalog.json".into(), "/data/logs".into())
            .with_abilities(Abilities::PHONE);
        app.set_edges(PHONE_EDGES);
        app.apply_catalog(Loaded::Missing);
        app.frame(&InputSnapshot::default(), PHONE);
        let shown: Vec<String> = texts(&app).into_iter().map(|(text, _)| text).collect();
        assert!(
            shown.contains(&"The list is sent to this device from a computer.".to_string()),
            "{shown:?}"
        );
        assert!(
            !shown.iter().any(|text| text.contains("Edit list")),
            "{shown:?}"
        );
    }

    #[test]
    fn nothing_is_put_under_the_edges_the_system_keeps() {
        let entries: Vec<String> = (0..3)
            .map(|n| format!(r#"{{ "name": "Entry {n}", "open": "https://example.org/{n}" }}"#))
            .collect();
        let text = format!(r#"{{ "entries": [{}] }}"#, entries.join(","));
        for (size, edges) in [
            (PHONE, PHONE_EDGES),
            // On its side: the camera housing to the left, the home
            // indicator below.
            (
                (926.0, 428.0),
                Edges {
                    left: 47.0,
                    top: 0.0,
                    right: 47.0,
                    bottom: 21.0,
                },
            ),
        ] {
            let mut app = phone(&text);
            app.set_edges(edges);
            app.banner = Some("Could not open something".to_string());
            app.frame(&InputSnapshot::default(), size);
            let shown = texts(&app);
            assert!(shown.len() > 8, "{shown:?}");
            for (text, rect) in shown {
                assert!(
                    rect.x >= edges.left
                        && rect.right() <= size.0 - edges.right
                        && rect.y >= edges.top
                        && rect.bottom() <= size.1 - edges.bottom,
                    "{text:?} at {rect:?} in {size:?} with {edges:?}"
                );
            }
            // Nor is a row: its panel stops at the side edges.
            let mut scene = Vec::new();
            app.paint(&mut scene);
            let panels: Vec<Rect> = scene
                .iter()
                .filter_map(|op| match op {
                    PaintOp::FillRect { rect, color }
                        if *color == draw::PANEL || *color == draw::PANEL_SELECTED =>
                    {
                        Some(*rect)
                    }
                    _ => None,
                })
                .filter(|rect| rect.height == ROW_HEIGHT)
                .collect();
            assert_eq!(panels.len(), 3, "{panels:?}");
            for rect in panels {
                assert!(
                    rect.x >= edges.left && rect.right() <= size.0 - edges.right,
                    "a row at {rect:?} in {size:?} with {edges:?}"
                );
            }
            // The list begins below the header's bar and the banner, not
            // under them.
            assert_eq!(
                app.layout().list.y,
                edges.top + HEADER_HEIGHT + TALL_BANNER_HEIGHT
            );
            // With no edges kept, the same window puts its title at the top.
            app.set_edges(Edges::default());
            app.frame(&InputSnapshot::default(), size);
            assert!(texts(&app).iter().any(|(_, rect)| rect.y == 0.0));
        }

        // The lines a failed program printed stop above the bottom edge too.
        // Only a machine that starts programs has any to show.
        let mut rig = Rig::new();
        rig.app.set_edges(PHONE_EDGES);
        let run = Rig::started(&rig.click("Editor", Action::Start));
        for n in 0..12 {
            rig.happen(run, What::Line(format!("line {n}")));
        }
        rig.happen(
            run,
            What::Ended(Ending::Failed {
                why: "It ended with a failure".to_string(),
                tail: Vec::new(),
            }),
        );
        let shown = texts(&rig.app);
        assert!(shown.iter().any(|(text, _)| text == "line 11"), "{shown:?}");
        for (text, rect) in shown {
            assert!(
                rect.y >= PHONE_EDGES.top && rect.bottom() <= SIZE.1 - PHONE_EDGES.bottom,
                "{text:?} at {rect:?}"
            );
        }
    }

    #[test]
    fn a_finger_drags_the_list_and_a_drag_presses_nothing() {
        let entries: Vec<String> = (0..40)
            .map(|n| format!(r#"{{ "name": "Entry {n}", "open": "https://example.org/{n}" }}"#))
            .collect();
        let text = format!(r#"{{ "entries": [{}] }}"#, entries.join(","));
        let mut app = phone(&text);
        let (index, row) = entry_row(&app, "Entry 3");
        let (_, button) = app.buttons(index, row).next().unwrap();
        let landed = Point {
            x: button.x + button.width / 2.0,
            y: button.y + button.height / 2.0,
        };
        // Down on a button, then up the screen by 100: the list follows the
        // finger and the button is not pressed.
        touch(&mut app, landed, true, true, false);
        let moved = Point {
            y: landed.y - 100.0,
            ..landed
        };
        touch(&mut app, moved, false, true, false);
        assert_eq!(app.scroll, 100.0);
        touch(&mut app, moved, false, false, true);
        assert_eq!(app.take_effects(), []);
        assert_eq!(app.scroll, 100.0);
        // Back down again, and past the top stops at the top.
        touch(&mut app, moved, true, true, false);
        let far = Point {
            y: moved.y + 500.0,
            ..moved
        };
        touch(&mut app, far, false, true, false);
        touch(&mut app, far, false, false, true);
        assert_eq!(app.scroll, 0.0);
        assert_eq!(app.take_effects(), []);
        // A touch that stays put is a press. It lands in the row above the
        // button's own rectangle, where a finger still reaches it.
        let (index, row) = entry_row(&app, "Entry 2");
        let (_, button) = app.buttons(index, row).next().unwrap();
        let above = Point {
            x: button.x + button.width / 2.0,
            y: row.y + 3.0,
        };
        assert!(!button.contains(above));
        touch(&mut app, above, true, true, false);
        touch(
            &mut app,
            Point {
                y: above.y + 4.0,
                ..above
            },
            false,
            false,
            true,
        );
        assert_eq!(
            app.take_effects(),
            [Effect::Open {
                target: "https://example.org/2".to_string()
            }]
        );
        // Nothing is lit once the finger has gone.
        let mut scene = Vec::new();
        app.paint(&mut scene);
        assert!(!scene.iter().any(
            |op| matches!(op, PaintOp::FillRect { color, .. } if *color == draw::ACCENT_HOT || *color == draw::PANEL_HOT)
        ));
    }

    #[test]
    fn a_pointer_does_not_drag_the_list_and_presses_only_the_button() {
        let entries: Vec<String> = (0..40)
            .map(|n| format!(r#"{{ "name": "Entry {n}", "open": "https://example.org/{n}" }}"#))
            .collect();
        let text = format!(r#"{{ "entries": [{}] }}"#, entries.join(","));
        let mut app = LauncherApp::new("/data/catalog.json".into(), "/data/logs".into());
        app.take_effects();
        app.apply_catalog(Loaded::Catalog(catalog::parse(&text, None).unwrap()));
        app.frame(&InputSnapshot::default(), SIZE);
        let (index, row) = entry_row(&app, "Entry 3");
        let (_, button) = app.buttons(index, row).next().unwrap();
        let landed = Point {
            x: button.x + button.width / 2.0,
            y: button.y + button.height / 2.0,
        };
        for (y, pressed, down, released) in [
            (landed.y, true, true, false),
            (landed.y - 100.0, false, true, false),
            (landed.y - 100.0, false, false, true),
        ] {
            app.frame(
                &InputSnapshot {
                    mouse_x: landed.x,
                    mouse_y: y,
                    mouse_pressed: pressed,
                    mouse_down: down,
                    mouse_released: released,
                    ..Default::default()
                },
                SIZE,
            );
        }
        assert_eq!(app.scroll, 0.0);
        // Above the button, inside its row, is the row and not the button.
        let above = Point {
            x: landed.x,
            y: row.y + 3.0,
        };
        assert_eq!(app.hit(&app.layout(), above), Some(Target::Row(index)));
    }

    /// A phone's launcher with one game built into it.
    fn with_a_game(loaded: Loaded) -> LauncherApp {
        let mut app = LauncherApp::new("/data/catalog.json".into(), "/data/logs".into())
            .with_abilities(Abilities::PHONE)
            .with_built_ins(vec![Entry::built_in("Tiles", "Games", "Match the pairs")]);
        app.set_edges(PHONE_EDGES);
        app.take_effects();
        app.apply_catalog(loaded);
        app.frame(&InputSnapshot::default(), PHONE);
        app
    }

    fn names(app: &LauncherApp) -> Vec<&str> {
        app.rows.iter().map(|row| row.entry.name.as_str()).collect()
    }

    #[test]
    fn what_is_built_in_heads_the_list_and_its_play_asks_for_the_window() {
        // The list was made for a launcher without the game: it links to
        // the game as an app of its own, and to a page.
        let listed = r#"{ "entries": [
            { "name": "Tiles", "group": "Games", "open": "tiles://" },
            { "name": "Demo", "group": "Pages", "open": "https://example.org/" }
        ] }"#;
        let mut app = with_a_game(Loaded::Catalog(catalog::parse(listed, None).unwrap()));
        // Its own comes first, once: the link of the same name is left out.
        assert_eq!(names(&app), ["Tiles", "Demo"]);
        assert_eq!(app.rows[0].entry.kind(), Kind::BuiltIn);
        assert_eq!(app.rows[0].actions(), [Action::Play]);
        let shown: Vec<String> = texts(&app).into_iter().map(|(text, _)| text).collect();
        assert!(shown.contains(&"Play".to_string()), "{shown:?}");
        assert!(shown.contains(&"Match the pairs".to_string()), "{shown:?}");
        assert!(
            shown.contains(&"Plays in this window; leaving it comes back here".to_string()),
            "{shown:?}"
        );

        let (index, row) = entry_row(&app, "Tiles");
        let (action, button) = app.buttons(index, row).next().unwrap();
        assert_eq!(action, Action::Play);
        let on = Point {
            x: button.x + button.width / 2.0,
            y: button.y + button.height / 2.0,
        };
        touch(&mut app, on, true, true, false);
        assert_eq!(app.take_effects(), [], "nothing until the finger lifts");
        touch(&mut app, on, false, false, true);
        assert_eq!(
            app.take_effects(),
            [Effect::Play {
                name: "Tiles".to_string()
            }]
        );
        // The launcher is not what runs it, so it has nothing to show as
        // running, and it can be played again when it has been left.
        assert_eq!(app.state_of("Tiles"), Some(&State::Idle));
        touch(&mut app, on, true, true, false);
        touch(&mut app, on, false, false, true);
        assert_eq!(app.take_effects().len(), 1);

        // Reading the list again keeps it, and keeps it first.
        app.apply_catalog(Loaded::Catalog(catalog::parse(listed, None).unwrap()));
        assert_eq!(names(&app), ["Tiles", "Demo"]);
    }

    #[test]
    fn what_is_built_in_is_listed_with_no_catalog_at_all() {
        let app = with_a_game(Loaded::Missing);
        assert_eq!(names(&app), ["Tiles"]);
        let shown: Vec<String> = texts(&app).into_iter().map(|(text, _)| text).collect();
        assert!(shown.contains(&"Play".to_string()), "{shown:?}");
        assert!(
            !shown.contains(&"The list is sent to this device from a computer.".to_string()),
            "{shown:?}"
        );

        // A catalog that cannot be read takes nothing away either.
        let mut app = with_a_game(Loaded::Missing);
        app.apply_catalog(Loaded::Wrong("line 3: no".to_string()));
        assert_eq!(names(&app), ["Tiles"]);
    }

    #[test]
    fn what_is_built_in_is_played_by_enter_and_by_name_at_the_start() {
        let mut app = LauncherApp::new("/data/catalog.json".into(), "/data/logs".into())
            .with_built_ins(vec![Entry::built_in("Tiles", "Games", "Match the pairs")]);
        app.start_when_loaded("Tiles".to_string());
        app.take_effects();
        app.apply_catalog(Loaded::Catalog(catalog::parse(CATALOG, None).unwrap()));
        let play = Effect::Play {
            name: "Tiles".to_string(),
        };
        assert_eq!(app.take_effects(), std::slice::from_ref(&play));
        assert_eq!(app.banner, None);
        // A computer starts programs as before; the game is one more row.
        assert_eq!(names(&app), ["Tiles", "Editor", "Site", "Demo"]);
        assert_eq!(app.rows[1].actions(), [Action::Start]);

        app.frame(
            &InputSnapshot {
                key_events: vec![HostKeyEvent {
                    key: HostKey::Enter,
                    modifiers: Default::default(),
                }],
                ..Default::default()
            },
            SIZE,
        );
        assert_eq!(app.take_effects(), [play]);
    }
}
