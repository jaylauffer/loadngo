//! The vault's screens: making one, unlocking it, the entries it keeps, and
//! the form an entry is added or changed in. `docs/VAULT.md` says what the
//! vault is; `loadngo_vault` makes the passwords and seals the file.
//!
//! Like the launcher's list, the screens own no threads and do no blocking
//! work. What they want done leaves as [`Ask`]s for the host loop
//! (`host.rs`), and the answers come back through [`VaultView::found`],
//! [`VaultView::unlocked`], [`VaultView::written`], [`VaultView::made`],
//! [`VaultView::copied`] and [`VaultView::pasted`].

use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use loadngo_host_core::{HostKey, HostKeyEvent, InputSnapshot};
use loadngo_vault::{
    generate, Alphabet, Costs, Entry, Error, Reading, Recipe, Sealed, System, Vault,
};
use ui_core::{Color, HorizontalAlign, PaintOp, Point, PointerButton, Rect};
use zeroize::Zeroizing;

use crate::app::Edges;
use crate::draw::{self, Tone};
use crate::field::{roughly, Field, Measure};

const PAD: f32 = 16.0;
const HEADER_HEIGHT: f32 = 56.0;
const BANNER_HEIGHT: f32 = 28.0;
const ROW_HEIGHT: f32 = 58.0;
const ROW_GAP: f32 = 6.0;
const BUTTON_WIDTH: f32 = 76.0;
const BUTTON_HEIGHT: f32 = 30.0;
const BUTTON_GAP: f32 = 8.0;
const FIELD_HEIGHT: f32 = 34.0;
const LABEL_HEIGHT: f32 = 22.0;
/// The widest a column of fields grows.
const FORM_WIDTH: f32 = 440.0;
/// The pane under the list: the selected entry.
const DETAIL_HEIGHT: f32 = 132.0;
/// A line of a shown password's number.
const NUMBER_LINE: f32 = 17.0;
/// Digits of a shown password's number to a line.
const NUMBER_DIGITS: usize = 64;

/// A copied password is taken off the clipboard after this long, if it is
/// still there.
pub const FORGET_AFTER: Duration = Duration::from_secs(43);
/// The vault locks itself after this long with nothing pressed or typed in
/// it.
pub const LOCK_AFTER: Duration = Duration::from_secs(307);
/// The fewest characters a master password has.
pub const MASTER_LEAST: usize = 8;

/// Text that is wiped when dropped and is never printed by `{:?}`: a master
/// password on its way to the key derivation, a password on its way to the
/// clipboard.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(Zeroizing<String>);

impl Secret {
    pub fn new(text: &str) -> Self {
        Secret(Zeroizing::new(text.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret")
    }
}

/// What the host loop is asked to do for the vault.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ask {
    /// Is there a vault's file? Answer with [`VaultView::found`].
    Look,
    /// Make a vault under `master` ([`create`]); answer with
    /// [`VaultView::unlocked`].
    Create { master: Secret },
    /// Open the vault with `master` ([`unlock`]); answer with
    /// [`VaultView::unlocked`].
    Open { master: Secret },
    /// Write the sealed file; answer with [`VaultView::written`].
    Write(Sealed),
    /// Make a password; answer with [`VaultView::made`].
    Make(Recipe),
    /// Put `text` on the clipboard; answer with [`VaultView::copied`].
    Copy { text: Secret },
    /// Empty the clipboard if it still holds `text`. No answer.
    Forget { text: Secret },
    /// Read the clipboard; answer with [`VaultView::pasted`].
    Paste,
}

/// A vault that has just been opened, with what each of its passwords is
/// read as a number (in the order the vault lists its entries).
#[derive(Debug)]
pub struct Unlocked {
    pub vault: Vault,
    pub readings: Vec<Reading>,
}

/// What `text` is, read as a number.
fn reading(text: &str) -> Reading {
    generate::read(text, &mut System).unwrap_or(Reading::NotANumber)
}

fn read_all(vault: Vault) -> Unlocked {
    let readings = vault
        .entries()
        .iter()
        .map(|entry| reading(&entry.password))
        .collect();
    Unlocked { vault, readings }
}

/// Opens the vault at `path`. Blocks on the disk and on the key derivation:
/// the host runs it on another thread.
pub fn unlock(path: &Path, master: &Secret) -> Result<Unlocked, Error> {
    Vault::open(path, master.as_str()).map(read_all)
}

/// Makes a new vault at `path`. Blocks, as [`unlock`] does.
pub fn create(path: &Path, master: &Secret, costs: Costs) -> Result<Unlocked, Error> {
    Vault::create(path, master.as_str(), costs, &mut System).map(read_all)
}

/// Makes a password to `recipe`. A moment's arithmetic; the host runs it on
/// another thread all the same.
pub fn make(recipe: Recipe) -> Result<Secret, Error> {
    generate::make(recipe, &mut System).map(|password| Secret::new(&password))
}

/// Something that can be pressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hit {
    /// Back to the launcher's list.
    Back,
    Lock,
    New,
    /// Write again after a write that failed.
    SaveAgain,
    /// Unlock, or make the vault.
    Submit,
    Row(usize),
    Copy(usize),
    Show,
    CopyAccount,
    Edit,
    Delete,
    Field(usize),
    Alphabet(Alphabet),
    Shorter,
    Longer,
    Make,
    /// Show or hide the password being typed.
    Reveal,
    Save,
    Cancel,
}

/// The fields of a screen and which of them is typed into.
#[derive(Debug)]
struct Fields {
    all: Vec<Field>,
    focus: usize,
}

impl Fields {
    fn focused(&mut self) -> Option<&mut Field> {
        self.all.get_mut(self.focus)
    }

    fn step(&mut self, back: bool) {
        let count = self.all.len();
        if count > 0 {
            self.focus = (self.focus + if back { count - 1 } else { 1 }) % count;
        }
    }
}

/// The form an entry is added or changed in.
#[derive(Debug)]
struct Form {
    /// The entry being changed; none for a new one.
    at: Option<usize>,
    /// Name, account, address, password.
    fields: Fields,
    recipe: Recipe,
    /// A password has been asked for and has not come yet.
    making: bool,
    /// The password is shown as typed.
    show: bool,
    /// What the password in the form is, read as a number.
    reading: Reading,
}

const NAME: usize = 0;
const ACCOUNT: usize = 1;
const ADDRESS: usize = 2;
const PASSWORD: usize = 3;
const LABELS: [&str; 4] = ["Name", "Account", "Address", "Password"];

impl Form {
    fn new(at: Option<usize>, entry: Option<&Entry>) -> Self {
        let mut all = vec![
            Field::holding(entry.map_or("", |e| e.name.as_str())),
            Field::holding(entry.map_or("", |e| e.account.as_str())),
            Field::holding(entry.map_or("", |e| e.address.as_str())),
            Field::holding(entry.map_or("", |e| e.password.as_str())),
        ];
        all[PASSWORD].hidden = true;
        let reading = entry.map_or(Reading::NotANumber, |e| reading(&e.password));
        // A password to be made anew is made the way this one was.
        let recipe = match (&reading, entry) {
            (Reading::Prime { alphabet, .. }, Some(entry)) => Recipe {
                alphabet: *alphabet,
                length: entry.password.chars().count(),
            }
            .fitted(),
            _ => Recipe::default(),
        };
        Form {
            at,
            fields: Fields { all, focus: NAME },
            recipe,
            making: false,
            show: false,
            reading,
        }
    }

    fn reread(&mut self) {
        self.reading = reading(self.fields.all[PASSWORD].text());
    }
}

/// An unlocked vault and what is being looked at in it.
#[derive(Debug)]
struct Opened {
    vault: Vault,
    /// What each entry's password is, read as a number; as the entries are
    /// listed.
    readings: Vec<Reading>,
    /// For each entry, another that has the same password.
    twins: Vec<Option<usize>>,
    selected: usize,
    scroll: f32,
    /// The selected entry's password is shown.
    shown: bool,
    /// Delete has been pressed once and waits for a second press.
    armed: bool,
    form: Option<Form>,
    /// A write is under way.
    saving: bool,
    /// The entries have changed since the last write was sealed.
    dirty: bool,
    /// The last write failed: what is on disk is behind what is here.
    behind: bool,
    /// Lock as soon as what is here is on disk.
    lock_wanted: bool,
    /// Lock was pressed once with a change unsaved and waits for a second.
    lock_armed: bool,
}

impl Opened {
    fn new(unlocked: Unlocked) -> Self {
        let mut opened = Opened {
            vault: unlocked.vault,
            readings: unlocked.readings,
            twins: Vec::new(),
            selected: 0,
            scroll: 0.0,
            shown: false,
            armed: false,
            form: None,
            saving: false,
            dirty: false,
            behind: false,
            lock_wanted: false,
            lock_armed: false,
        };
        opened.pair();
        opened
    }

    /// Finds, for each entry, another with the same password.
    fn pair(&mut self) {
        let entries = self.vault.entries();
        self.twins = (0..entries.len())
            .map(|index| {
                (0..entries.len()).find(|&other| {
                    other != index && entries[other].password == entries[index].password
                })
            })
            .collect();
    }

    fn select(&mut self, index: usize, list_height: f32) {
        let count = self.vault.entries().len();
        if count == 0 {
            self.selected = 0;
            return;
        }
        let index = index.min(count - 1);
        if index != self.selected {
            self.shown = false;
        }
        self.selected = index;
        self.armed = false;
        let top = PAD + index as f32 * (ROW_HEIGHT + ROW_GAP);
        let bottom = top + ROW_HEIGHT + PAD;
        if top - PAD < self.scroll {
            self.scroll = (top - PAD).max(0.0);
        } else if bottom > self.scroll + list_height {
            self.scroll = bottom - list_height;
        }
    }

    fn content_height(&self) -> f32 {
        let count = self.vault.entries().len() as f32;
        2.0 * PAD + count * ROW_HEIGHT + (count - 1.0).max(0.0) * ROW_GAP
    }
}

#[derive(Debug)]
enum Stage {
    /// Whether there is a vault has not been answered yet.
    Unknown,
    /// There is none: two fields to choose a master password in.
    Absent {
        fields: Fields,
        busy: bool,
    },
    /// There is one, locked.
    Locked {
        fields: Fields,
        busy: bool,
    },
    Open(Box<Opened>),
}

/// A button, placed.
struct Button {
    hit: Hit,
    rect: Rect,
    label: String,
    tone: Tone,
    /// Marked as the one of several that is in force.
    chosen: bool,
    /// Drawn, and does nothing: its turn has not come.
    idle: bool,
}

/// Words, placed.
struct Words {
    rect: Rect,
    text: String,
    font: u16,
    color: Color,
}

/// Where everything on the screen is, for one size of window.
struct Plan {
    header: Rect,
    banner: Rect,
    body: Rect,
    buttons: Vec<Button>,
    /// Each field of the screen: its index and its box.
    fields: Vec<(usize, Rect)>,
    words: Vec<Words>,
    /// The list of entries, and the row of each that is in sight.
    list: Rect,
    rows: Vec<(usize, Rect)>,
    detail: Rect,
    /// Twelve dots standing for a hidden password, in the detail pane.
    dots: Option<Point>,
}

impl Plan {
    fn hit(&self, point: Point) -> Option<Hit> {
        if let Some(button) = self
            .buttons
            .iter()
            .find(|button| !button.idle && button.rect.contains(point))
        {
            return Some(button.hit);
        }
        if let Some((index, _)) = self.fields.iter().find(|(_, rect)| rect.contains(point)) {
            return Some(Hit::Field(*index));
        }
        if self.list.contains(point) {
            return self
                .rows
                .iter()
                .find(|(_, rect)| rect.contains(point))
                .map(|(index, _)| Hit::Row(*index));
        }
        None
    }

    fn button(&mut self, hit: Hit, rect: Rect, label: &str, tone: Tone) -> &mut Button {
        self.buttons.push(Button {
            hit,
            rect,
            label: label.to_string(),
            tone,
            chosen: false,
            idle: false,
        });
        self.buttons.last_mut().expect("just pushed")
    }

    fn say(&mut self, rect: Rect, text: impl Into<String>, font: u16, color: Color) {
        self.words.push(Words {
            rect,
            text: text.into(),
            font,
            color,
        });
    }
}

pub struct VaultView {
    stage: Stage,
    size: (f32, f32),
    edges: Edges,
    pointer: Point,
    pressed: Option<Hit>,
    asks: Vec<Ask>,
    /// Back to the launcher's list has been asked for.
    leaving: bool,
    now: Instant,
    /// When something was last pressed or typed here.
    touched: Instant,
    /// When the clipboard is to be emptied of a copied password.
    forget: Option<(Instant, Secret)>,
    /// A line under the header, and whether it reports a failure.
    note: Option<(String, bool)>,
    measure: Measure,
}

impl Default for VaultView {
    fn default() -> Self {
        Self::new()
    }
}

impl VaultView {
    pub fn new() -> Self {
        let now = Instant::now();
        VaultView {
            stage: Stage::Unknown,
            size: (0.0, 0.0),
            edges: Edges::default(),
            pointer: Point::default(),
            pressed: None,
            asks: Vec::new(),
            leaving: false,
            now,
            touched: now,
            forget: None,
            note: None,
            measure: roughly,
        }
    }

    /// How the host measures text, so that a caret lands between the right
    /// characters.
    pub fn set_measure(&mut self, measure: Measure) {
        self.measure = measure;
    }

    /// The vault's screens are about to be shown.
    pub fn enter(&mut self) {
        self.touched = self.now;
        if matches!(self.stage, Stage::Unknown) && !self.asks.contains(&Ask::Look) {
            self.asks.push(Ask::Look);
        }
    }

    /// What the host loop should do now; each is handed over once.
    pub fn take_asks(&mut self) -> Vec<Ask> {
        std::mem::take(&mut self.asks)
    }

    /// Whether the launcher's list has been asked for; asked once.
    pub fn take_leaving(&mut self) -> bool {
        std::mem::take(&mut self.leaving)
    }

    /// Whether the vault is unlocked.
    pub fn is_open(&self) -> bool {
        matches!(self.stage, Stage::Open(_))
    }

    /// The time now. Carries out what has come due: emptying the clipboard
    /// of a copied password, locking a vault left alone.
    pub fn tick(&mut self, now: Instant) {
        self.now = now;
        if self.forget.as_ref().is_some_and(|(due, _)| *due <= now) {
            if let Some((_, text)) = self.forget.take() {
                self.asks.push(Ask::Forget { text });
            }
            if self.note.as_ref().is_some_and(|(_, failure)| !failure) {
                self.note = None;
            }
        }
        // A change that is not on disk is not thrown away by a clock.
        if self.settled() && now.duration_since(self.touched) >= LOCK_AFTER {
            self.lock_now();
        }
    }

    /// Whether the vault is unlocked with nothing waiting to reach the disk:
    /// the state a clock may lock.
    fn settled(&self) -> bool {
        matches!(&self.stage, Stage::Open(opened)
            if !(opened.saving || opened.dirty || opened.behind))
    }

    /// How long until [`VaultView::tick`] has something to do, if it will.
    /// A vault with a change unsaved is not locked by the clock, so its
    /// clock is not waited on either: a wait already over, for something
    /// that will not happen, would be a loop.
    pub fn wake_in(&self) -> Option<Duration> {
        let forget = self.forget.as_ref().map(|(due, _)| *due);
        let lock = self.settled().then(|| self.touched + LOCK_AFTER);
        [forget, lock]
            .into_iter()
            .flatten()
            .min()
            .map(|due| due.saturating_duration_since(self.now))
    }

    /// There is, or is not, a vault's file.
    pub fn found(&mut self, exists: bool) {
        if !matches!(self.stage, Stage::Unknown) {
            return;
        }
        self.stage = if exists {
            Self::locked()
        } else {
            Stage::Absent {
                fields: Fields {
                    all: vec![Field::hidden(), Field::hidden()],
                    focus: 0,
                },
                busy: false,
            }
        };
    }

    fn locked() -> Stage {
        Stage::Locked {
            fields: Fields {
                all: vec![Field::hidden()],
                focus: 0,
            },
            busy: false,
        }
    }

    /// The vault was opened or made, or was not.
    pub fn unlocked(&mut self, result: Result<Unlocked, Error>) {
        match result {
            Ok(unlocked) => {
                self.stage = Stage::Open(Box::new(Opened::new(unlocked)));
                self.note = None;
                self.touched = self.now;
            }
            Err(error) => {
                self.note = Some((error.to_string(), true));
                match &mut self.stage {
                    Stage::Absent { fields, busy } if error == Error::Exists => {
                        // Something made one meanwhile: it is there to unlock.
                        let _ = (fields, busy);
                        self.stage = Self::locked();
                    }
                    Stage::Absent { busy, .. } => *busy = false,
                    Stage::Locked { fields, busy } => {
                        *busy = false;
                        // What was typed is selected, so typing replaces it.
                        if let Some(field) = fields.focused() {
                            field.select_all();
                        }
                    }
                    Stage::Unknown | Stage::Open(_) => {}
                }
            }
        }
    }

    /// A write finished.
    pub fn written(&mut self, result: Result<Vec<u8>, Error>) {
        let Stage::Open(opened) = &mut self.stage else {
            return;
        };
        opened.saving = false;
        match result {
            Ok(bytes) => {
                opened.vault.wrote(bytes);
                opened.behind = false;
                if self.note.as_ref().is_some_and(|(_, failure)| *failure) {
                    self.note = None;
                }
                if opened.dirty {
                    self.flush();
                } else if opened.lock_wanted {
                    self.lock_now();
                }
            }
            Err(error) => {
                opened.behind = true;
                opened.dirty = true;
                opened.lock_wanted = false;
                self.note = Some((format!("Not saved. {error}"), true));
            }
        }
    }

    /// A password was made, or was not.
    pub fn made(&mut self, result: Result<Secret, Error>) {
        let Stage::Open(opened) = &mut self.stage else {
            return;
        };
        let Some(form) = opened.form.as_mut() else {
            return;
        };
        form.making = false;
        match result {
            Ok(password) => {
                form.fields.all[PASSWORD].set(password.as_str());
                form.reread();
            }
            Err(error) => self.note = Some((error.to_string(), true)),
        }
    }

    /// A password reached the clipboard, or did not.
    pub fn copied(&mut self, text: Secret, result: Result<(), String>) {
        match result {
            Ok(()) => {
                self.forget = Some((self.now + FORGET_AFTER, text));
                self.note = Some((
                    format!(
                        "Copied. It is taken off the clipboard in {} seconds.",
                        FORGET_AFTER.as_secs()
                    ),
                    false,
                ));
            }
            Err(error) => self.note = Some((format!("Could not copy: {error}"), true)),
        }
    }

    /// What the clipboard held, for the field being typed into.
    pub fn pasted(&mut self, text: Option<String>) {
        let Some(text) = text.map(Zeroizing::new) else {
            return;
        };
        let changed = self
            .fields()
            .and_then(Fields::focused)
            .is_some_and(|field| field.insert(&text));
        if changed {
            self.typed_into_form();
        }
    }

    /// The fields of the screen that is up, if it has any.
    fn fields(&mut self) -> Option<&mut Fields> {
        match &mut self.stage {
            Stage::Absent { fields, busy } | Stage::Locked { fields, busy } if !*busy => {
                Some(fields)
            }
            Stage::Open(opened) => opened.form.as_mut().map(|form| &mut form.fields),
            _ => None,
        }
    }

    /// The form's text changed: what its password is may have, too.
    fn typed_into_form(&mut self) {
        if let Stage::Open(opened) = &mut self.stage {
            if let Some(form) = opened.form.as_mut() {
                if form.fields.focus == PASSWORD {
                    form.reread();
                }
            }
        }
    }

    /// Seals what is here and asks for it to be written, unless a write is
    /// under way: then this happens when that one ends.
    fn flush(&mut self) {
        let Stage::Open(opened) = &mut self.stage else {
            return;
        };
        if opened.saving || !opened.dirty {
            return;
        }
        match opened.vault.seal(&mut System) {
            Ok(sealed) => {
                opened.saving = true;
                opened.dirty = false;
                self.asks.push(Ask::Write(sealed));
            }
            Err(error) => {
                opened.behind = true;
                self.note = Some((format!("Not saved. {error}"), true));
            }
        }
    }

    /// Locks: the entries and the key are dropped, which wipes them.
    fn lock_now(&mut self) {
        self.stage = Self::locked();
        if self.note.as_ref().is_some_and(|(_, failure)| *failure) {
            self.note = None;
        }
    }

    /// Lock was asked for. A change still on its way to disk is waited for;
    /// one that could not be written is given up only at a second asking.
    fn lock(&mut self) {
        let Stage::Open(opened) = &mut self.stage else {
            return;
        };
        if opened.saving || (opened.dirty && !opened.behind) {
            opened.lock_wanted = true;
            self.flush();
        } else if opened.behind && !opened.lock_armed {
            opened.lock_armed = true;
            self.note = Some((
                "The last change is not saved. Lock again to give it up.".to_string(),
                true,
            ));
        } else {
            self.lock_now();
        }
    }

    fn copy(&mut self, text: &str) {
        self.asks.push(Ask::Copy {
            text: Secret::new(text),
        });
    }

    fn submit(&mut self) {
        match &mut self.stage {
            Stage::Absent { fields, busy } if !*busy => {
                let first = fields.all[0].text();
                if first.chars().count() < MASTER_LEAST {
                    self.note = Some((
                        format!("A master password has at least {MASTER_LEAST} characters."),
                        true,
                    ));
                    fields.focus = 0;
                } else if first != fields.all[1].text() {
                    self.note = Some(("The two are not the same.".to_string(), true));
                    fields.focus = 1;
                    fields.all[1].select_all();
                } else {
                    self.asks.push(Ask::Create {
                        master: Secret::new(first),
                    });
                    *busy = true;
                    self.note = None;
                }
            }
            Stage::Locked { fields, busy } if !*busy && !fields.all[0].is_empty() => {
                self.asks.push(Ask::Open {
                    master: Secret::new(fields.all[0].text()),
                });
                *busy = true;
                self.note = None;
            }
            _ => {}
        }
    }

    /// Saves the form as an entry.
    fn save(&mut self) {
        let Stage::Open(opened) = &mut self.stage else {
            return;
        };
        let Some(form) = opened.form.as_mut() else {
            return;
        };
        let text = |index: usize| form.fields.all[index].text().trim().to_string();
        let name = text(NAME);
        // A password is kept as typed, spaces and all.
        let password = form.fields.all[PASSWORD].text().to_string();
        if name.is_empty() {
            form.fields.focus = NAME;
            self.note = Some(("It needs a name.".to_string(), true));
            return;
        }
        if password.is_empty() {
            form.fields.focus = PASSWORD;
            self.note = Some(("It needs a password.".to_string(), true));
            return;
        }
        let before = form.at.and_then(|at| opened.vault.entries().get(at));
        let changed = match before {
            Some(entry) if entry.password == password => entry.changed,
            _ => SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |since| since.as_secs()),
        };
        let entry = Entry {
            name,
            account: text(ACCOUNT),
            address: text(ADDRESS),
            password,
            changed,
        };
        let read = form.reading.clone();
        let at = match form.at {
            Some(at) if at < opened.readings.len() => {
                opened.readings.remove(at);
                opened.vault.replace(at, entry)
            }
            _ => opened.vault.add(entry),
        };
        opened.readings.insert(at.min(opened.readings.len()), read);
        opened.pair();
        opened.form = None;
        opened.selected = at;
        opened.shown = false;
        opened.armed = false;
        opened.dirty = true;
        self.note = None;
        self.flush();
        let list_height = self.plan().list.height;
        if let Stage::Open(opened) = &mut self.stage {
            opened.select(at, list_height);
        }
    }

    fn act(&mut self, hit: Hit) {
        let list_height = self.plan().list.height;
        match hit {
            Hit::Back => self.leaving = true,
            Hit::Lock => self.lock(),
            Hit::Submit => self.submit(),
            Hit::Save => self.save(),
            Hit::SaveAgain => {
                if let Stage::Open(opened) = &mut self.stage {
                    opened.dirty = true;
                }
                self.flush();
            }
            Hit::Field(_) => {}
            _ => {}
        }
        let Stage::Open(opened) = &mut self.stage else {
            return;
        };
        if hit != Hit::Lock {
            opened.lock_armed = false;
        }
        match hit {
            Hit::New => {
                opened.form = Some(Form::new(None, None));
                self.note = None;
            }
            Hit::Row(index) => opened.select(index, list_height),
            Hit::Copy(index) => {
                opened.select(index, list_height);
                if let Some(entry) = opened.vault.entries().get(index) {
                    let text = Secret::new(&entry.password);
                    self.asks.push(Ask::Copy { text });
                }
            }
            Hit::CopyAccount => {
                if let Some(entry) = opened.vault.entries().get(opened.selected) {
                    let account = entry.account.clone();
                    self.copy(&account);
                }
            }
            Hit::Show => opened.shown = !opened.shown,
            Hit::Edit => {
                let at = opened.selected;
                if let Some(entry) = opened.vault.entries().get(at) {
                    opened.form = Some(Form::new(Some(at), Some(entry)));
                    self.note = None;
                }
            }
            Hit::Delete => {
                if opened.armed {
                    let at = opened.selected;
                    if at < opened.readings.len() {
                        opened.readings.remove(at);
                    }
                    opened.vault.remove(at);
                    opened.pair();
                    opened.armed = false;
                    opened.shown = false;
                    opened.selected = at.min(opened.vault.entries().len().saturating_sub(1));
                    opened.dirty = true;
                    self.flush();
                } else {
                    opened.armed = true;
                }
            }
            Hit::Cancel => {
                opened.form = None;
                self.note = None;
            }
            _ => {}
        }
        let Stage::Open(opened) = &mut self.stage else {
            return;
        };
        let Some(form) = opened.form.as_mut() else {
            return;
        };
        match hit {
            Hit::Alphabet(alphabet) => {
                form.recipe = Recipe {
                    alphabet,
                    ..form.recipe
                }
                .fitted();
            }
            Hit::Shorter | Hit::Longer => {
                let length = if hit == Hit::Shorter {
                    form.recipe.length.saturating_sub(1)
                } else {
                    form.recipe.length + 1
                };
                form.recipe = Recipe {
                    length,
                    ..form.recipe
                }
                .fitted();
            }
            Hit::Make if !form.making => {
                form.making = true;
                form.fields.focus = PASSWORD;
                self.asks.push(Ask::Make(form.recipe));
            }
            Hit::Reveal => {
                form.show = !form.show;
                form.fields.all[PASSWORD].hidden = !form.show;
            }
            _ => {}
        }
    }

    fn key(&mut self, event: &HostKeyEvent) {
        let command = event.modifiers.meta || event.modifiers.ctrl;
        let in_form = matches!(&self.stage, Stage::Open(opened) if opened.form.is_some());
        let in_list = matches!(&self.stage, Stage::Open(opened) if opened.form.is_none());
        match event.key {
            HostKey::Escape if in_form => self.act(Hit::Cancel),
            HostKey::Escape => self.leaving = true,
            HostKey::Enter if in_form => self.save(),
            HostKey::Enter if in_list => {
                if let Stage::Open(opened) = &self.stage {
                    let index = opened.selected;
                    if index < opened.vault.entries().len() {
                        self.act(Hit::Copy(index));
                    }
                }
            }
            HostKey::Enter => self.submit(),
            HostKey::Up | HostKey::Down if in_list => {
                let list_height = self.plan().list.height;
                if let Stage::Open(opened) = &mut self.stage {
                    let next = if event.key == HostKey::Up {
                        opened.selected.saturating_sub(1)
                    } else {
                        opened.selected + 1
                    };
                    opened.select(next, list_height);
                }
            }
            // New: an entry from the list, a password from the form.
            HostKey::N if command && in_list => self.act(Hit::New),
            HostKey::N if command && in_form => self.act(Hit::Make),
            HostKey::S if command && in_form => self.save(),
            HostKey::C if command && in_list => {
                let index = match &self.stage {
                    Stage::Open(opened) => opened.selected,
                    _ => 0,
                };
                self.act(Hit::Copy(index));
            }
            HostKey::L if command => self.lock(),
            // A look at the selected entry's password, and away again.
            HostKey::Space if in_list => self.act(Hit::Show),
            HostKey::Tab => {
                if let Some(fields) = self.fields() {
                    fields.step(event.modifiers.shift);
                }
            }
            HostKey::V if command => {
                if self.fields().is_some() {
                    self.asks.push(Ask::Paste);
                }
            }
            _ => {
                let changed = self
                    .fields()
                    .and_then(Fields::focused)
                    .is_some_and(|field| field.key(event));
                if changed {
                    self.typed_into_form();
                }
            }
        }
    }

    /// One frame of input for a window of `size`.
    pub fn frame(&mut self, input: &InputSnapshot, size: (f32, f32), edges: Edges) {
        self.size = size;
        self.edges = edges;
        self.pointer = Point {
            x: input.mouse_x,
            y: input.mouse_y,
        };
        let primary = input.mouse_button == PointerButton::Primary;
        if input.mouse_pressed && primary {
            self.touched = self.now;
            let plan = self.plan();
            self.pressed = plan.hit(self.pointer);
            if let Some(Hit::Field(index)) = self.pressed {
                let rect = plan
                    .fields
                    .iter()
                    .find(|(at, _)| *at == index)
                    .map(|(_, rect)| *rect);
                let (point, measure) = (self.pointer, self.measure);
                if let (Some(rect), Some(fields)) = (rect, self.fields()) {
                    fields.focus = index;
                    if let Some(field) = fields.focused() {
                        field.press(point, rect, measure, draw::FONT);
                    }
                }
            }
            // A press anywhere but Delete takes its question back.
            if self.pressed != Some(Hit::Delete) {
                if let Stage::Open(opened) = &mut self.stage {
                    opened.armed = false;
                }
            }
        }
        if input.mouse_released && primary {
            if let Some(hit) = self.pressed.take() {
                if self.plan().hit(self.pointer) == Some(hit) {
                    self.act(hit);
                }
            }
        }
        if input.mouse_wheel_y != 0.0 {
            let plan = self.plan();
            if let Stage::Open(opened) = &mut self.stage {
                if plan.list.contains(self.pointer) {
                    opened.scroll += if input.mouse_wheel_precise {
                        -input.mouse_wheel_y
                    } else {
                        -input.mouse_wheel_y * ROW_HEIGHT
                    };
                }
            }
        }
        for event in &input.key_events {
            self.touched = self.now;
            self.key(event);
        }
        if !input.typed_text.is_empty() {
            self.touched = self.now;
            let changed = self
                .fields()
                .and_then(Fields::focused)
                .is_some_and(|field| field.insert(&input.typed_text));
            if changed {
                self.typed_into_form();
            }
        }
        let list_height = self.plan().list.height;
        if let Stage::Open(opened) = &mut self.stage {
            let most = (opened.content_height() - list_height).max(0.0);
            opened.scroll = opened.scroll.clamp(0.0, most);
        }
    }

    /// Where everything is.
    fn plan(&self) -> Plan {
        let (width, height) = self.size;
        let edges = self.edges;
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
            height: if self.note.is_some() {
                BANNER_HEIGHT
            } else {
                0.0
            },
        };
        let body = Rect {
            x: edges.left,
            y: banner.bottom(),
            width: (width - edges.left - edges.right).max(0.0),
            height: (height - banner.bottom() - edges.bottom).max(0.0),
        };
        let mut plan = Plan {
            header,
            banner,
            body,
            buttons: Vec::new(),
            fields: Vec::new(),
            words: Vec::new(),
            list: Rect::default(),
            rows: Vec::new(),
            detail: Rect::default(),
            dots: None,
        };

        // The header's buttons, from the right.
        let mut right = width - edges.right - PAD;
        let mut header_button = |plan: &mut Plan, hit: Hit, label: &str, tone: Tone, wide: f32| {
            right -= wide;
            plan.button(
                hit,
                Rect {
                    x: right,
                    y: edges.top + (HEADER_HEIGHT - BUTTON_HEIGHT) / 2.0,
                    width: wide,
                    height: BUTTON_HEIGHT,
                },
                label,
                tone,
            );
            right -= BUTTON_GAP;
        };
        header_button(&mut plan, Hit::Back, "List", Tone::Plain, BUTTON_WIDTH);
        if let Stage::Open(opened) = &self.stage {
            header_button(&mut plan, Hit::Lock, "Lock", Tone::Plain, BUTTON_WIDTH);
            if opened.form.is_none() {
                header_button(&mut plan, Hit::New, "New", Tone::Main, BUTTON_WIDTH);
            }
            if opened.behind && !opened.saving {
                header_button(&mut plan, Hit::SaveAgain, "Save again", Tone::Warn, 96.0);
            }
        }

        let column = Rect {
            x: body.x + PAD,
            y: body.y + PAD,
            width: (body.width - 2.0 * PAD).clamp(0.0, FORM_WIDTH),
            height: 0.0,
        };
        match &self.stage {
            Stage::Unknown => {}
            Stage::Absent { busy, .. } => {
                let mut y = column.y;
                plan.say(
                    Rect {
                        y,
                        height: 26.0,
                        ..column
                    },
                    "Make a vault",
                    draw::NAME_FONT,
                    draw::TEXT,
                );
                y += 26.0 + 8.0;
                for (index, label) in ["Master password", "Once more"].into_iter().enumerate() {
                    y = Self::labelled(&mut plan, index, label, column, y, column.width);
                }
                plan.say(
                    Rect {
                        y,
                        height: 20.0,
                        ..column
                    },
                    "It is kept nowhere. If it is lost, what is in the vault is lost with it.",
                    draw::SMALL_FONT,
                    draw::DIM_TEXT,
                );
                y += 20.0 + 12.0;
                plan.button(
                    Hit::Submit,
                    Rect {
                        x: column.x,
                        y,
                        width: 132.0,
                        height: BUTTON_HEIGHT,
                    },
                    if *busy { "Making it" } else { "Make the vault" },
                    Tone::Main,
                )
                .idle = *busy;
            }
            Stage::Locked { busy, .. } => {
                let y = Self::labelled(
                    &mut plan,
                    0,
                    "Master password",
                    column,
                    column.y,
                    column.width,
                );
                plan.button(
                    Hit::Submit,
                    Rect {
                        x: column.x,
                        y: y + 4.0,
                        width: 96.0,
                        height: BUTTON_HEIGHT,
                    },
                    if *busy { "Unlocking" } else { "Unlock" },
                    Tone::Main,
                )
                .idle = *busy;
            }
            Stage::Open(opened) => match &opened.form {
                Some(form) => self.plan_form(&mut plan, opened, form, column),
                None => self.plan_list(&mut plan, opened),
            },
        }
        plan
    }

    /// A label and, under it, field `index`; returns where the next thing
    /// goes.
    fn labelled(
        plan: &mut Plan,
        index: usize,
        label: &str,
        column: Rect,
        y: f32,
        wide: f32,
    ) -> f32 {
        plan.say(
            Rect {
                y,
                height: LABEL_HEIGHT,
                ..column
            },
            label,
            draw::SMALL_FONT,
            draw::DIM_TEXT,
        );
        plan.fields.push((
            index,
            Rect {
                x: column.x,
                y: y + LABEL_HEIGHT,
                width: wide,
                height: FIELD_HEIGHT,
            },
        ));
        y + LABEL_HEIGHT + FIELD_HEIGHT + 8.0
    }

    fn plan_form(&self, plan: &mut Plan, opened: &Opened, form: &Form, column: Rect) {
        let mut y = column.y;
        let title = form
            .at
            .and_then(|at| opened.vault.entries().get(at))
            .map_or("New", |entry| entry.name.as_str());
        plan.say(
            Rect {
                y,
                height: 26.0,
                ..column
            },
            title,
            draw::NAME_FONT,
            draw::TEXT,
        );
        y += 26.0 + 8.0;
        for (index, label) in LABELS.into_iter().enumerate() {
            let wide = if index == PASSWORD {
                (column.width - BUTTON_WIDTH - BUTTON_GAP).max(0.0)
            } else {
                column.width
            };
            let next = Self::labelled(plan, index, label, column, y, wide);
            if index == PASSWORD {
                plan.button(
                    Hit::Reveal,
                    Rect {
                        x: column.x + wide + BUTTON_GAP,
                        y: y + LABEL_HEIGHT + (FIELD_HEIGHT - BUTTON_HEIGHT) / 2.0,
                        width: BUTTON_WIDTH,
                        height: BUTTON_HEIGHT,
                    },
                    if form.show { "Hide" } else { "Show" },
                    Tone::Plain,
                );
            }
            y = next;
        }
        // What the password in the form is.
        if !form.fields.all[PASSWORD].is_empty() {
            let (text, color) = if form.reading.is_prime() {
                ("A prime number", draw::UP_DOT)
            } else {
                ("Not a prime number", draw::DIM_TEXT)
            };
            plan.say(
                Rect {
                    y: y - 6.0,
                    height: 20.0,
                    ..column
                },
                text,
                draw::SMALL_FONT,
                color,
            );
        }
        y += 20.0;

        // The maker: which characters, how many, and the button.
        let mut x = column.x;
        for (alphabet, wide) in [
            (Alphabet::Digits, 64.0),
            (Alphabet::Letters, 136.0),
            (Alphabet::Keyboard, 176.0),
        ] {
            plan.button(
                Hit::Alphabet(alphabet),
                Rect {
                    x,
                    y,
                    width: wide,
                    height: BUTTON_HEIGHT,
                },
                alphabet.name(),
                Tone::Plain,
            )
            .chosen = form.recipe.alphabet == alphabet;
            x += wide + BUTTON_GAP;
        }
        y += BUTTON_HEIGHT + BUTTON_GAP;
        let lengths = form.recipe.alphabet.lengths();
        let small = Rect {
            x: column.x,
            y,
            width: BUTTON_HEIGHT,
            height: BUTTON_HEIGHT,
        };
        plan.button(Hit::Shorter, small, "-", Tone::Plain).idle =
            form.recipe.length <= *lengths.start();
        plan.button(
            Hit::Longer,
            Rect {
                x: small.right() + BUTTON_GAP,
                ..small
            },
            "+",
            Tone::Plain,
        )
        .idle = form.recipe.length >= *lengths.end();
        let made_wide = 112.0;
        let said = Rect {
            x: small.right() + BUTTON_GAP + BUTTON_HEIGHT + 12.0,
            y,
            width: (column.width - 2.0 * BUTTON_HEIGHT - BUTTON_GAP - 24.0 - made_wide).max(0.0),
            height: BUTTON_HEIGHT,
        };
        plan.say(
            said,
            format!(
                "{} characters, about {} bits",
                form.recipe.length,
                form.recipe.bits().round()
            ),
            draw::FONT,
            draw::TEXT,
        );
        plan.button(
            Hit::Make,
            Rect {
                x: column.right() - made_wide,
                y,
                width: made_wide,
                height: BUTTON_HEIGHT,
            },
            "Make a prime",
            Tone::Plain,
        )
        .idle = form.making;
        y += BUTTON_HEIGHT + 20.0;

        plan.button(
            Hit::Save,
            Rect {
                x: column.x,
                y,
                width: BUTTON_WIDTH,
                height: BUTTON_HEIGHT,
            },
            "Save",
            Tone::Main,
        );
        plan.button(
            Hit::Cancel,
            Rect {
                x: column.x + BUTTON_WIDTH + BUTTON_GAP,
                y,
                width: BUTTON_WIDTH,
                height: BUTTON_HEIGHT,
            },
            "Cancel",
            Tone::Plain,
        );
    }

    fn plan_list(&self, plan: &mut Plan, opened: &Opened) {
        let body = plan.body;
        let entries = opened.vault.entries();
        let selected = entries.get(opened.selected);
        let number = match (selected, opened.readings.get(opened.selected)) {
            (Some(_), Some(Reading::Prime { number, .. })) if opened.shown => {
                Some(number.decimal())
            }
            _ => None,
        };
        let number_lines = number
            .as_ref()
            .map_or(0, |digits| digits.len().div_ceil(NUMBER_DIGITS));
        let twin = opened
            .twins
            .get(opened.selected)
            .copied()
            .flatten()
            .and_then(|other| entries.get(other));
        let wanted = if selected.is_some() {
            DETAIL_HEIGHT
                + number_lines as f32 * NUMBER_LINE
                + if twin.is_some() { 20.0 } else { 0.0 }
                + self.edges.bottom
        } else {
            0.0
        };
        let detail_height = wanted.min(body.height * 0.6);
        plan.detail = Rect {
            x: 0.0,
            y: self.size.1 - detail_height,
            width: self.size.0,
            height: detail_height,
        };
        plan.list = Rect {
            height: (plan.detail.y - body.y).max(0.0),
            ..body
        };

        if entries.is_empty() {
            plan.say(
                Rect {
                    x: body.x + PAD,
                    y: body.y + PAD,
                    width: (body.width - 2.0 * PAD).max(0.0),
                    height: 24.0,
                },
                "Nothing is kept here yet.",
                draw::FONT,
                draw::DIM_TEXT,
            );
            return;
        }
        for index in 0..entries.len() {
            let rect = Rect {
                x: plan.list.x + PAD,
                y: plan.list.y + PAD + index as f32 * (ROW_HEIGHT + ROW_GAP) - opened.scroll,
                width: (plan.list.width - 2.0 * PAD).max(0.0),
                height: ROW_HEIGHT,
            };
            if rect.bottom() < plan.list.y || rect.y > plan.list.bottom() {
                continue;
            }
            plan.rows.push((index, rect));
            let button = Rect {
                x: rect.right() - 12.0 - BUTTON_WIDTH,
                y: rect.y + (ROW_HEIGHT - BUTTON_HEIGHT) / 2.0,
                width: BUTTON_WIDTH,
                height: BUTTON_HEIGHT,
            };
            // A row half under the header or the pane is pressed where it
            // shows.
            if button.y >= plan.list.y && button.bottom() <= plan.list.bottom() {
                plan.button(Hit::Copy(index), button, "Copy", Tone::Main);
            }
        }

        let Some(entry) = selected else {
            return;
        };
        let pane = Rect {
            x: body.x + PAD,
            y: plan.detail.y + 10.0,
            width: (body.width - 2.0 * PAD).max(0.0),
            height: BUTTON_HEIGHT,
        };
        // Name, and what can be done with the entry.
        let mut right = pane.right();
        for (hit, label, tone, wide) in [
            (
                Hit::Delete,
                if opened.armed { "Delete it?" } else { "Delete" },
                if opened.armed {
                    Tone::Warn
                } else {
                    Tone::Plain
                },
                if opened.armed { 92.0 } else { BUTTON_WIDTH },
            ),
            (Hit::Edit, "Change", Tone::Plain, BUTTON_WIDTH),
            (
                Hit::Show,
                if opened.shown { "Hide" } else { "Show" },
                Tone::Plain,
                BUTTON_WIDTH,
            ),
        ] {
            right -= wide;
            plan.button(
                hit,
                Rect {
                    x: right,
                    width: wide,
                    ..pane
                },
                label,
                tone,
            );
            right -= BUTTON_GAP;
        }
        plan.say(
            Rect {
                width: (right - pane.x).max(0.0),
                ..pane
            },
            entry.name.as_str(),
            draw::NAME_FONT,
            draw::TEXT,
        );
        // Who signs in, and where.
        let mut y = pane.y + BUTTON_HEIGHT + 6.0;
        let who = match (entry.account.is_empty(), entry.address.is_empty()) {
            (false, false) => format!("{}  at  {}", entry.account, entry.address),
            (false, true) => entry.account.clone(),
            (true, false) => entry.address.clone(),
            (true, true) => String::new(),
        };
        let copy_wide = 116.0;
        if !entry.account.is_empty() {
            plan.button(
                Hit::CopyAccount,
                Rect {
                    x: pane.right() - copy_wide,
                    y,
                    width: copy_wide,
                    height: BUTTON_HEIGHT,
                },
                "Copy account",
                Tone::Plain,
            );
        }
        plan.say(
            Rect {
                y,
                width: (pane.width - copy_wide - BUTTON_GAP).max(0.0),
                ..pane
            },
            who,
            draw::FONT,
            draw::DIM_TEXT,
        );
        y += BUTTON_HEIGHT + 4.0;
        // The password, or dots for it.
        let line = Rect {
            y,
            height: 22.0,
            ..pane
        };
        if opened.shown {
            plan.say(line, entry.password.as_str(), draw::FONT, draw::TEXT);
        } else {
            plan.dots = Some(Point {
                x: line.x + 4.0,
                y: line.y + line.height / 2.0,
            });
        }
        y += 22.0;
        if let Some(digits) = &number {
            for (row, chunk) in digits.as_bytes().chunks(NUMBER_DIGITS).enumerate() {
                plan.say(
                    Rect {
                        y: y + row as f32 * NUMBER_LINE,
                        height: NUMBER_LINE,
                        ..pane
                    },
                    String::from_utf8_lossy(chunk),
                    draw::SMALL_FONT,
                    draw::DIM_TEXT,
                );
            }
            y += number_lines as f32 * NUMBER_LINE;
        }
        let prime = opened
            .readings
            .get(opened.selected)
            .is_some_and(Reading::is_prime);
        plan.say(
            Rect {
                y,
                height: 20.0,
                ..pane
            },
            if prime {
                "A prime number"
            } else {
                "Not a prime number"
            },
            draw::SMALL_FONT,
            if prime { draw::UP_DOT } else { draw::DIM_TEXT },
        );
        if let Some(other) = twin {
            plan.say(
                Rect {
                    y: y + 20.0,
                    height: 20.0,
                    ..pane
                },
                format!("The same password as {}", other.name),
                draw::SMALL_FONT,
                draw::FAILED,
            );
        }
    }

    /// The fields of the screen that is up, to draw.
    fn shown_fields(&self) -> Option<&Fields> {
        match &self.stage {
            Stage::Absent { fields, .. } | Stage::Locked { fields, .. } => Some(fields),
            Stage::Open(opened) => opened.form.as_ref().map(|form| &form.fields),
            Stage::Unknown => None,
        }
    }

    pub fn paint(&self, scene: &mut Vec<PaintOp>) {
        let plan = self.plan();
        let hot = plan.hit(self.pointer);
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

        // The list first: the header and the pane are painted over its
        // edges, which is what clips a row scrolled half out of view.
        if let Stage::Open(opened) = &self.stage {
            if opened.form.is_none() {
                self.paint_rows(scene, &plan, opened, hot);
                draw::fill(scene, plan.detail, draw::HEADER);
                draw::line(
                    scene,
                    Point {
                        x: 0.0,
                        y: plan.detail.y,
                    },
                    Point {
                        x: plan.detail.width,
                        y: plan.detail.y,
                    },
                    draw::EDGE,
                );
            }
        }
        draw::fill(scene, plan.header, draw::HEADER);
        draw::text(
            scene,
            Rect {
                x: self.edges.left + PAD,
                y: self.edges.top,
                width: 110.0,
                height: HEADER_HEIGHT,
            },
            "Vault",
            draw::TITLE_FONT,
            draw::TEXT,
            HorizontalAlign::Left,
        );
        draw::line(
            scene,
            Point {
                x: 0.0,
                y: plan.header.bottom(),
            },
            Point {
                x: plan.header.width,
                y: plan.header.bottom(),
            },
            draw::EDGE,
        );
        if let Some((text, failure)) = &self.note {
            draw::fill(scene, plan.banner, draw::PANEL);
            draw::text(
                scene,
                Rect {
                    x: self.edges.left + PAD,
                    width: (plan.banner.width - self.edges.left - self.edges.right - 2.0 * PAD)
                        .max(0.0),
                    ..plan.banner
                },
                text.as_str(),
                draw::FONT,
                if *failure {
                    draw::FAILED
                } else {
                    draw::DIM_TEXT
                },
                HorizontalAlign::Left,
            );
        }

        for words in &plan.words {
            draw::text(
                scene,
                words.rect,
                words.text.as_str(),
                words.font,
                words.color,
                HorizontalAlign::Left,
            );
        }
        if let Some(from) = plan.dots {
            for index in 0..12 {
                draw::dot(
                    scene,
                    Point {
                        x: from.x + index as f32 * 10.0,
                        y: from.y,
                    },
                    3.2,
                    draw::DIM_TEXT,
                );
            }
        }
        if let Some(fields) = self.shown_fields() {
            for (index, rect) in &plan.fields {
                if let Some(field) = fields.all.get(*index) {
                    field.paint(
                        scene,
                        *rect,
                        fields.focus == *index,
                        self.measure,
                        draw::FONT,
                    );
                }
            }
        }
        for button in &plan.buttons {
            if matches!(button.hit, Hit::Copy(_)) {
                // Drawn with its row.
                continue;
            }
            Self::paint_button(scene, button, hot == Some(button.hit));
        }
    }

    fn paint_button(scene: &mut Vec<PaintOp>, button: &Button, hot: bool) {
        if button.idle {
            draw::fill(scene, button.rect, draw::PANEL);
            draw::text(
                scene,
                button.rect,
                button.label.as_str(),
                draw::FONT,
                draw::HEADING_TEXT,
                HorizontalAlign::Center,
            );
            return;
        }
        draw::button(scene, button.rect, &button.label, button.tone, hot);
        if button.chosen {
            draw::stroke(scene, button.rect, draw::ACCENT);
        }
    }

    fn paint_rows(&self, scene: &mut Vec<PaintOp>, plan: &Plan, opened: &Opened, hot: Option<Hit>) {
        let entries = opened.vault.entries();
        for (index, rect) in &plan.rows {
            let (index, rect) = (*index, *rect);
            let Some(entry) = entries.get(index) else {
                continue;
            };
            let selected = index == opened.selected;
            let over = matches!(hot, Some(Hit::Row(at) | Hit::Copy(at)) if at == index);
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
                draw::stroke(scene, rect, draw::ACCENT);
            }
            // Green: its password is a prime. Red: another entry has the
            // same password.
            let prime = opened.readings.get(index).is_some_and(Reading::is_prime);
            let twin = opened.twins.get(index).copied().flatten().is_some();
            draw::dot(
                scene,
                Point {
                    x: rect.x + 18.0,
                    y: rect.y + ROW_HEIGHT / 2.0,
                },
                5.0,
                if twin {
                    draw::FAILED
                } else if prime {
                    draw::UP_DOT
                } else {
                    draw::IDLE_DOT
                },
            );
            let text = Rect {
                x: rect.x + 36.0,
                y: rect.y + 8.0,
                width: (rect.width - 36.0 - BUTTON_WIDTH - 24.0).max(0.0),
                height: 22.0,
            };
            draw::text(
                scene,
                text,
                entry.name.as_str(),
                draw::NAME_FONT,
                draw::TEXT,
                HorizontalAlign::Left,
            );
            let under = if entry.account.is_empty() {
                entry.address.as_str()
            } else {
                entry.account.as_str()
            };
            draw::text(
                scene,
                Rect {
                    y: rect.y + 30.0,
                    height: 20.0,
                    ..text
                },
                under,
                draw::FONT,
                draw::DIM_TEXT,
                HorizontalAlign::Left,
            );
            if let Some(button) = plan
                .buttons
                .iter()
                .find(|button| button.hit == Hit::Copy(index))
            {
                Self::paint_button(scene, button, hot == Some(Hit::Copy(index)));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui_core::Modifiers;

    const SIZE: (f32, f32) = (760.0, 640.0);

    /// The screens with a stand-in for the host loop: asks are carried out
    /// at once, on this thread, against a vault in a scratch folder and a
    /// clipboard that is a variable.
    struct Desk {
        view: VaultView,
        folder: tempfile::TempDir,
        clipboard: Option<String>,
        /// Every ask the screens made, in order.
        asked: Vec<Ask>,
        /// Writes are kept here, not carried out, while this is set.
        hold_writes: bool,
        held: Vec<Sealed>,
        /// What the next write is answered with, in place of writing.
        fail_write: Option<Error>,
        start: Instant,
    }

    impl Desk {
        fn new() -> Self {
            let mut view = VaultView::new();
            let start = Instant::now();
            view.tick(start);
            let mut desk = Desk {
                view,
                folder: tempfile::tempdir().unwrap(),
                clipboard: None,
                asked: Vec::new(),
                hold_writes: false,
                held: Vec::new(),
                fail_write: None,
                start,
            };
            desk.view.enter();
            desk.frame(InputSnapshot::default());
            desk
        }

        fn path(&self) -> std::path::PathBuf {
            self.folder.path().join("passwords.vault")
        }

        /// A desk whose vault exists, holds `entries` and is unlocked.
        fn with(entries: &[(&str, &str, &str)]) -> Self {
            let mut desk = Desk::new();
            desk.make_vault("correct horse");
            for (name, account, password) in entries {
                desk.add(name, account, password);
            }
            // The first entry is the one selected.
            for _ in entries {
                desk.key(HostKey::Up);
            }
            desk
        }

        fn serve(&mut self) {
            loop {
                let asks = self.view.take_asks();
                if asks.is_empty() {
                    break;
                }
                for ask in asks {
                    self.asked.push(ask.clone());
                    match ask {
                        Ask::Look => self.view.found(Vault::exists(&self.path())),
                        Ask::Create { master } => {
                            let made = create(&self.path(), &master, Costs::TRIVIAL);
                            self.view.unlocked(made);
                        }
                        Ask::Open { master } => {
                            let opened = unlock(&self.path(), &master);
                            self.view.unlocked(opened);
                        }
                        Ask::Write(sealed) => {
                            if let Some(error) = self.fail_write.take() {
                                self.view.written(Err(error));
                            } else if self.hold_writes {
                                self.held.push(sealed);
                            } else {
                                self.view.written(sealed.write());
                            }
                        }
                        Ask::Make(recipe) => self.view.made(make(recipe)),
                        Ask::Copy { text } => {
                            self.clipboard = Some(text.as_str().to_string());
                            self.view.copied(text, Ok(()));
                        }
                        Ask::Forget { text } => {
                            if self.clipboard.as_deref() == Some(text.as_str()) {
                                self.clipboard = Some(String::new());
                            }
                        }
                        Ask::Paste => self.view.pasted(self.clipboard.clone()),
                    }
                }
            }
        }

        fn frame(&mut self, input: InputSnapshot) {
            self.view.frame(&input, SIZE, Edges::default());
            self.serve();
        }

        fn after(&mut self, seconds: u64) {
            self.view.tick(self.start + Duration::from_secs(seconds));
            self.serve();
        }

        fn type_text(&mut self, text: &str) {
            self.frame(InputSnapshot {
                typed_text: text.to_string(),
                ..InputSnapshot::default()
            });
        }

        fn key(&mut self, key: HostKey) {
            self.key_with(key, Modifiers::default());
        }

        fn key_with(&mut self, key: HostKey, modifiers: Modifiers) {
            self.frame(InputSnapshot {
                key_events: vec![HostKeyEvent { key, modifiers }],
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
        }

        fn middle(rect: Rect) -> Point {
            Point {
                x: rect.x + rect.width / 2.0,
                y: rect.y + rect.height / 2.0,
            }
        }

        /// The labels of the buttons that can be pressed now.
        fn buttons(&self) -> Vec<String> {
            self.view
                .plan()
                .buttons
                .iter()
                .filter(|button| !button.idle)
                .map(|button| button.label.clone())
                .collect()
        }

        fn press(&mut self, label: &str) {
            let plan = self.view.plan();
            let button = plan
                .buttons
                .iter()
                .find(|button| button.label == label && !button.idle)
                .unwrap_or_else(|| panic!("no button {label:?} among {:?}", self.buttons()));
            let point = Self::middle(button.rect);
            self.click(point);
        }

        fn click_field(&mut self, index: usize) {
            let plan = self.view.plan();
            let rect = plan.fields.iter().find(|(at, _)| *at == index).unwrap().1;
            self.click(Point {
                x: rect.right() - 4.0,
                y: rect.y + rect.height / 2.0,
            });
        }

        /// Everything drawn, as text: the words, and the debug form of the
        /// rest.
        fn drawn(&self) -> String {
            let mut scene = Vec::new();
            self.view.paint(&mut scene);
            format!("{scene:?}")
        }

        /// The words drawn, one to a line.
        fn words(&self) -> Vec<String> {
            let mut scene = Vec::new();
            self.view.paint(&mut scene);
            scene
                .into_iter()
                .filter_map(|op| match op {
                    PaintOp::Text { text, .. } => Some(text),
                    _ => None,
                })
                .collect()
        }

        fn says(&self, text: &str) -> bool {
            self.words().iter().any(|words| words.contains(text))
        }

        fn note(&self) -> Option<&str> {
            self.view.note.as_ref().map(|(text, _)| text.as_str())
        }

        fn make_vault(&mut self, master: &str) {
            self.type_text(master);
            self.key(HostKey::Tab);
            self.type_text(master);
            self.key(HostKey::Enter);
            assert!(self.view.is_open(), "{:?}", self.note());
        }

        /// Adds an entry through the form, typing its password.
        fn add(&mut self, name: &str, account: &str, password: &str) {
            self.press("New");
            self.type_text(name);
            self.key(HostKey::Tab);
            self.type_text(account);
            self.key(HostKey::Tab);
            self.key(HostKey::Tab);
            self.type_text(password);
            self.press("Save");
            assert!(self.opened().form.is_none(), "{:?}", self.note());
        }

        fn opened(&self) -> &Opened {
            match &self.view.stage {
                Stage::Open(opened) => opened,
                other => panic!("the vault is not open: {other:?}"),
            }
        }

        /// What is on disk, read with the master password.
        fn on_disk(&self, master: &str) -> Vec<Entry> {
            Vault::open(&self.path(), master)
                .unwrap()
                .entries()
                .to_vec()
        }
    }

    #[test]
    fn with_no_vault_a_master_password_is_chosen_twice_and_the_vault_is_made() {
        let mut desk = Desk::new();
        assert_eq!(desk.asked, [Ask::Look]);
        assert!(desk.says("Make a vault"));
        assert_eq!(desk.view.plan().fields.len(), 2);

        // Too short.
        desk.type_text("short");
        desk.key(HostKey::Tab);
        desk.type_text("short");
        desk.press("Make the vault");
        assert!(desk.note().unwrap().contains("at least 8"));
        assert!(!desk.view.is_open() && !Vault::exists(&desk.path()));

        // Not the same twice. The first field is in focus again.
        desk.key_with(
            HostKey::A,
            Modifiers {
                meta: true,
                ..Modifiers::default()
            },
        );
        desk.type_text("a long master password");
        desk.key(HostKey::Tab);
        desk.key_with(
            HostKey::A,
            Modifiers {
                meta: true,
                ..Modifiers::default()
            },
        );
        desk.type_text("a long master passwerd");
        desk.key(HostKey::Enter);
        assert_eq!(desk.note(), Some("The two are not the same."));
        assert!(!Vault::exists(&desk.path()));

        // The second is selected, so typing replaces it.
        desk.type_text("a long master password");
        desk.key(HostKey::Enter);
        assert!(desk.view.is_open());
        assert_eq!(desk.note(), None);
        assert!(desk.says("Nothing is kept here yet."));
        assert!(desk.on_disk("a long master password").is_empty());
        // What was typed is in nothing that was asked for by name.
        assert!(!format!("{:?}", desk.asked).contains("a long master"));
    }

    #[test]
    fn a_locked_vault_opens_to_the_master_password_and_to_nothing_else() {
        let mut desk = Desk::with(&[("Mail", "jay", "hunter2")]);
        desk.press("Lock");
        assert!(!desk.view.is_open());
        assert!(!desk.says("Mail"));
        assert_eq!(desk.buttons(), ["List", "Unlock"]);

        desk.type_text("incorrect horse");
        desk.press("Unlock");
        assert!(!desk.view.is_open());
        assert_eq!(
            desk.note(),
            Some("Wrong master password, or the file is damaged")
        );
        // What was typed is selected: the next typing replaces it.
        desk.type_text("correct horse");
        desk.key(HostKey::Enter);
        assert!(desk.view.is_open());
        assert!(desk.says("Mail"));
        assert_eq!(desk.note(), None);
    }

    #[test]
    fn a_new_entry_is_given_a_prime_and_reaches_the_disk() {
        let mut desk = Desk::with(&[]);
        desk.press("New");
        desk.type_text("Bank");
        desk.key(HostKey::Tab);
        desk.type_text("jay@example.org");
        desk.key(HostKey::Tab);
        desk.type_text("https://bank.example");
        assert!(desk.says("23 characters, about 144 bits"));
        assert!(!desk.says("prime number"));
        desk.press("Make a prime");
        assert!(desk.asked.contains(&Ask::Make(Recipe::default())));
        assert!(desk.says("A prime number"));
        desk.press("Save");

        let kept = desk.on_disk("correct horse");
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].name, "Bank");
        assert_eq!(kept[0].account, "jay@example.org");
        assert_eq!(kept[0].address, "https://bank.example");
        assert_eq!(kept[0].password.chars().count(), 23);
        assert!(kept[0].changed > 1_700_000_000);
        // It is a prime by the vault crate's own reading.
        assert!(reading(&kept[0].password).is_prime());
        // The list has it, and says so.
        assert!(desk.says("Bank") && desk.says("A prime number"));
        assert!(desk.opened().readings[0].is_prime());
    }

    #[test]
    fn the_maker_follows_the_alphabet_and_the_length_chosen() {
        let mut desk = Desk::with(&[]);
        desk.press("New");
        desk.press("Digits");
        // 23 digits.
        assert!(desk.says("23 characters, about 71 bits"));
        for _ in 0..30 {
            if desk.buttons().contains(&"-".to_string()) {
                desk.press("-");
            }
        }
        assert!(desk.says("4 characters, about 10 bits"));
        assert!(!desk.buttons().contains(&"-".to_string()));
        desk.press("+");
        desk.press("+");
        desk.press("Make a prime");
        let made = desk.opened().form.as_ref().unwrap().fields.all[PASSWORD]
            .text()
            .to_string();
        assert_eq!(made.len(), 6);
        let value: u64 = made.parse().unwrap();
        assert!((2..1000).all(|divisor| !value.is_multiple_of(divisor)));
        // Letters asks for at least eight.
        desk.press("Letters and digits");
        assert!(desk.says("8 characters, about 42 bits"));
        desk.press("Make a prime");
        let made = desk.opened().form.as_ref().unwrap().fields.all[PASSWORD]
            .text()
            .to_string();
        assert_eq!(made.len(), 8);
        assert!(made.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    #[test]
    fn a_password_is_in_nothing_drawn_until_it_is_shown() {
        let mut desk = Desk::with(&[("Mail", "jay", "1000003")]);
        assert!(desk.says("Mail") && desk.says("jay"));
        assert!(!desk.drawn().contains("1000003"));
        // In the form too, typed or not.
        desk.press("Change");
        assert!(!desk.drawn().contains("1000003"));
        desk.press("Show");
        assert!(desk.drawn().contains("1000003"));
        desk.press("Hide");
        assert!(!desk.drawn().contains("1000003"));
        desk.press("Cancel");

        desk.press("Show");
        assert!(desk.says("1000003"));
        desk.press("Hide");
        assert!(!desk.drawn().contains("1000003"));
        // Selecting another entry hides what was shown.
        desk.add("Bank", "", "another one");
        desk.press("Show");
        assert!(desk.says("another one"));
        desk.key(HostKey::Down);
        assert!(!desk.drawn().contains("another one"));
        assert!(!desk.drawn().contains("1000003"));
    }

    #[test]
    fn a_shown_prime_is_shown_as_its_number_too() {
        // "7Kp1" in base 62 is 1748339, a prime; "1000003" is itself.
        let mut desk = Desk::with(&[("Digits", "", "1000003"), ("Letters", "", "7Kp1")]);
        assert!(desk.opened().readings.iter().all(Reading::is_prime));
        desk.press("Show");
        assert!(desk.says("1000003"));
        desk.key(HostKey::Down);
        desk.press("Show");
        assert!(desk.says("7Kp1") && desk.says("1748339"));
        // A password that is no prime has no number to show.
        desk.add("Typed", "", "correct horse battery");
        desk.press("Show");
        assert!(desk.says("Not a prime number"));
    }

    #[test]
    fn copy_puts_the_password_on_the_clipboard_and_takes_it_off_in_time() {
        let mut desk = Desk::with(&[("Mail", "jay", "hunter2")]);
        desk.press("Copy");
        assert_eq!(desk.clipboard.as_deref(), Some("hunter2"));
        assert!(desk.note().unwrap().starts_with("Copied."));
        assert!(desk.view.wake_in().unwrap() <= FORGET_AFTER);

        desk.after(42);
        assert_eq!(desk.clipboard.as_deref(), Some("hunter2"));
        desk.after(43);
        assert_eq!(desk.clipboard.as_deref(), Some(""));
        assert_eq!(desk.note(), None);
        // The password is in no ask's printed form.
        assert!(!format!("{:?}", desk.asked).contains("hunter2"));

        // Something copied since is left alone.
        desk.key(HostKey::Enter);
        assert_eq!(desk.clipboard.as_deref(), Some("hunter2"));
        desk.clipboard = Some("a recipe".to_string());
        desk.after(200);
        assert_eq!(desk.clipboard.as_deref(), Some("a recipe"));

        // The account is copied by its own button.
        desk.press("Copy account");
        assert_eq!(desk.clipboard.as_deref(), Some("jay"));
    }

    #[test]
    fn a_vault_left_alone_locks_itself() {
        let mut desk = Desk::with(&[("Mail", "jay", "hunter2")]);
        assert_eq!(desk.view.wake_in(), Some(LOCK_AFTER));
        desk.after(300);
        assert!(desk.view.is_open());
        // A key is not being left alone.
        desk.key(HostKey::Down);
        desk.after(606);
        assert!(desk.view.is_open());
        desk.after(607);
        assert!(!desk.view.is_open());
        assert!(!desk.drawn().contains("Mail"));
        // Locked, nothing more is due.
        assert_eq!(desk.view.wake_in(), None);
    }

    #[test]
    fn delete_asks_once_more_and_a_press_elsewhere_takes_it_back() {
        let mut desk = Desk::with(&[("Bank", "", "one"), ("Mail", "", "two")]);
        desk.press("Delete");
        assert_eq!(desk.on_disk("correct horse").len(), 2);
        assert!(desk.buttons().contains(&"Delete it?".to_string()));
        // Elsewhere: the question is taken back.
        desk.press("Show");
        assert!(desk.buttons().contains(&"Delete".to_string()));
        assert_eq!(desk.on_disk("correct horse").len(), 2);

        desk.press("Delete");
        desk.press("Delete it?");
        let kept = desk.on_disk("correct horse");
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].name, "Mail");
        assert_eq!(desk.opened().readings.len(), 1);
    }

    #[test]
    fn two_entries_with_one_password_are_each_told() {
        let mut desk = Desk::with(&[
            ("Bank", "", "same"),
            ("Mail", "", "same"),
            ("Zoo", "", "own"),
        ]);
        assert_eq!(desk.opened().twins, [Some(1), Some(0), None]);
        assert!(desk.says("The same password as Mail"));
        desk.key(HostKey::Down);
        assert!(desk.says("The same password as Bank"));
        desk.key(HostKey::Down);
        assert!(!desk.says("The same password as"));
        // Changing one of them clears both.
        desk.key(HostKey::Up);
        desk.press("Change");
        desk.click_field(PASSWORD);
        desk.type_text("!");
        desk.press("Save");
        assert_eq!(desk.opened().twins, [None, None, None]);
    }

    #[test]
    fn changing_an_entry_keeps_its_date_unless_the_password_changed() {
        let mut desk = Desk::with(&[("Mail", "jay", "hunter2")]);
        // An old date, as if it had been kept a long time.
        if let Stage::Open(opened) = &mut desk.view.stage {
            let mut entry = opened.vault.entries()[0].clone();
            entry.changed = 1_000;
            opened.vault.replace(0, entry);
        }
        desk.press("Change");
        desk.click_field(ACCOUNT);
        desk.type_text("-lauffer");
        desk.key(HostKey::Enter);
        let kept = desk.on_disk("correct horse");
        assert_eq!(kept[0].account, "jay-lauffer");
        assert_eq!(kept[0].changed, 1_000);

        desk.press("Change");
        desk.press("Make a prime");
        desk.press("Save");
        let kept = desk.on_disk("correct horse");
        assert_ne!(kept[0].password, "hunter2");
        assert!(kept[0].changed > 1_700_000_000);
        // The form proposed the alphabet and length it was made with.
        desk.press("Change");
        assert!(desk.says("23 characters, about 144 bits"));
    }

    #[test]
    fn an_entry_needs_a_name_and_a_password() {
        let mut desk = Desk::with(&[]);
        desk.press("New");
        desk.press("Save");
        assert_eq!(desk.note(), Some("It needs a name."));
        desk.type_text("  Mail  ");
        desk.press("Save");
        assert_eq!(desk.note(), Some("It needs a password."));
        // The password field has the focus now.
        desk.type_text(" spaces kept ");
        desk.key(HostKey::Enter);
        let kept = desk.on_disk("correct horse");
        assert_eq!(kept[0].name, "Mail");
        assert_eq!(kept[0].password, " spaces kept ");
        // Escape leaves a form without saving.
        desk.press("New");
        desk.type_text("Nope");
        desk.key(HostKey::Escape);
        assert!(desk.opened().form.is_none());
        assert_eq!(desk.on_disk("correct horse").len(), 1);
        assert!(!desk.view.take_leaving());
        // And from the list, Escape goes back to the launcher's list.
        desk.key(HostKey::Escape);
        assert!(desk.view.take_leaving());
    }

    #[test]
    fn the_keys_do_what_the_buttons_do() {
        let command = Modifiers {
            meta: true,
            ..Modifiers::default()
        };
        let mut desk = Desk::with(&[]);
        // Command-N: a new entry, and in its form a new password.
        desk.key_with(HostKey::N, command);
        assert!(desk.opened().form.is_some());
        desk.type_text("Mail");
        desk.key_with(HostKey::N, command);
        assert!(desk.says("A prime number"));
        // Typing an n or an s is typing.
        desk.key(HostKey::N);
        desk.key(HostKey::S);
        assert!(desk.opened().form.is_some());
        desk.key_with(HostKey::S, command);
        assert!(desk.opened().form.is_none());
        let kept = desk.on_disk("correct horse");
        assert_eq!(kept[0].name, "Mail");
        assert_eq!(kept[0].password.chars().count(), 23);
        // Space looks at the password and away; Command-C copies it.
        assert!(!desk.drawn().contains(&kept[0].password));
        desk.key(HostKey::Space);
        assert!(desk.says(&kept[0].password));
        desk.key(HostKey::Space);
        assert!(!desk.drawn().contains(&kept[0].password));
        desk.key_with(HostKey::C, command);
        assert_eq!(desk.clipboard.as_deref(), Some(kept[0].password.as_str()));
        // Command-L locks, from the list or from a form.
        desk.key(HostKey::L);
        assert!(desk.view.is_open());
        desk.key_with(HostKey::N, command);
        desk.key_with(HostKey::L, command);
        assert!(!desk.view.is_open());
    }

    #[test]
    fn paste_goes_into_the_field_being_typed_into() {
        let mut desk = Desk::with(&[]);
        desk.clipboard = Some("from\nelsewhere".to_string());
        desk.press("New");
        desk.type_text("Mail");
        desk.click_field(PASSWORD);
        desk.key_with(
            HostKey::V,
            Modifiers {
                meta: true,
                ..Modifiers::default()
            },
        );
        desk.press("Save");
        assert_eq!(desk.on_disk("correct horse")[0].password, "fromelsewhere");
        // With no field in focus, nothing is read from the clipboard.
        let pastes = |desk: &Desk| desk.asked.iter().filter(|ask| **ask == Ask::Paste).count();
        let before = pastes(&desk);
        desk.key_with(
            HostKey::V,
            Modifiers {
                ctrl: true,
                ..Modifiers::default()
            },
        );
        assert_eq!(pastes(&desk), before);
    }

    #[test]
    fn a_write_that_fails_is_said_and_the_change_is_not_dropped() {
        let mut desk = Desk::with(&[("Mail", "", "one")]);
        desk.fail_write = Some(Error::Changed);
        desk.add("Bank", "", "two");
        assert!(desk.note().unwrap().starts_with("Not saved."));
        assert!(desk.buttons().contains(&"Save again".to_string()));
        assert_eq!(desk.on_disk("correct horse").len(), 1);
        // Left alone, it does not lock with the change unsaved, and does
        // not ask to be woken for a lock that will not come.
        desk.after(1_000);
        assert!(desk.view.is_open());
        assert_eq!(desk.view.wake_in(), None);
        // Lock asks once more before giving the change up.
        desk.press("Lock");
        assert!(desk.view.is_open());
        assert!(desk.note().unwrap().contains("Lock again"));
        // Save again writes it.
        desk.press("Save again");
        assert_eq!(desk.on_disk("correct horse").len(), 2);
        assert_eq!(desk.note(), None);
        assert!(!desk.buttons().contains(&"Save again".to_string()));
        // Saved, the clock counts again, from the last press.
        assert_eq!(desk.view.wake_in(), Some(LOCK_AFTER));
        desk.press("Lock");
        assert!(!desk.view.is_open());
    }

    #[test]
    fn lock_waits_for_a_write_under_way() {
        let mut desk = Desk::with(&[]);
        desk.hold_writes = true;
        desk.add("Mail", "", "one");
        desk.add("Bank", "", "two");
        // One write is out; the second change waits for it.
        assert_eq!(desk.held.len(), 1);
        desk.press("Lock");
        assert!(desk.view.is_open());
        // The first write lands: the second goes out, and still no lock.
        let first = desk.held.remove(0);
        desk.view.written(first.write());
        desk.serve();
        assert_eq!(desk.held.len(), 1);
        assert!(desk.view.is_open());
        let second = desk.held.remove(0);
        desk.view.written(second.write());
        desk.serve();
        assert!(!desk.view.is_open());
        assert_eq!(desk.on_disk("correct horse").len(), 2);
    }

    #[test]
    fn a_long_list_scrolls_and_the_keys_keep_the_selection_in_sight() {
        let names: Vec<String> = (0..30).map(|index| format!("Entry {index:02}")).collect();
        let entries: Vec<(&str, &str, &str)> =
            names.iter().map(|name| (name.as_str(), "", "x")).collect();
        let mut desk = Desk::with(&entries);
        for _ in 0..29 {
            desk.key(HostKey::Down);
        }
        assert_eq!(desk.opened().selected, 29);
        let plan = desk.view.plan();
        let (_, row) = plan.rows.iter().find(|(index, _)| *index == 29).unwrap();
        assert!(row.y >= plan.list.y && row.bottom() <= plan.list.bottom());
        // Enter copies the selected entry's password.
        desk.key(HostKey::Enter);
        assert_eq!(desk.clipboard.as_deref(), Some("x"));
        desk.frame(InputSnapshot {
            mouse_x: 300.0,
            mouse_y: 300.0,
            mouse_wheel_y: 1000.0,
            ..InputSnapshot::default()
        });
        assert_eq!(desk.opened().scroll, 0.0);
    }

    #[test]
    fn nothing_is_drawn_outside_the_window() {
        let mut desk = Desk::with(&[("Mail", "jay", "1000003")]);
        desk.press("Show");
        for form in [false, true] {
            if form {
                desk.press("Change");
            }
            let plan = desk.view.plan();
            for button in &plan.buttons {
                let rect = button.rect;
                assert!(
                    rect.x >= 0.0
                        && rect.right() <= SIZE.0
                        && rect.y >= 0.0
                        && rect.bottom() <= SIZE.1,
                    "{} at {rect:?}",
                    button.label
                );
            }
            for (_, rect) in &plan.fields {
                assert!(rect.right() <= SIZE.0 && rect.bottom() <= SIZE.1);
            }
            // No two buttons overlap.
            for (index, first) in plan.buttons.iter().enumerate() {
                for second in &plan.buttons[index + 1..] {
                    let (a, b) = (first.rect, second.rect);
                    let apart = a.right() <= b.x
                        || b.right() <= a.x
                        || a.bottom() <= b.y
                        || b.bottom() <= a.y;
                    assert!(apart, "{} over {}", first.label, second.label);
                }
            }
        }
    }
}
