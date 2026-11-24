//! The launcher on `loadngo-host-desktop`: the window, the loop that carries
//! out what [`LauncherApp`] asks for, and the hand-over of the window to
//! what is built in. The same source on macOS, Linux, Windows and iOS; on an
//! iPhone it can open addresses and play what is built in, and nothing else
//! (see "On an iPhone" in `docs/LAUNCHER.md`).
//!
//! loadngo's own `launcher` binary is [`launch`] with nothing built in.
//! Another project makes a launcher with games inside it by calling
//! [`launch`] with a [`BuiltIn`] for each.

use std::cell::OnceCell;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::rc::Rc;
use std::time::{Duration, Instant};

use loadngo_host_core::{FrameDemand, HostFrame, WindowDescriptor};
use loadngo_host_desktop::Offloaded;
use loadngo_vault::{Costs, Error as VaultError, Vault};

use crate::catalog::{self, Entry, Loaded};
use crate::process::{SearchPath, Supervisor};
use crate::vault::{self, Ask, Secret, Unlocked};
use crate::{draw, Abilities, Edges, Effect, LauncherApp};

const APP_ID: &str = "loadngo-launcher";
/// What this machine lets the launcher do.
const ABILITIES: Abilities = if cfg!(target_os = "ios") {
    Abilities::PHONE
} else {
    Abilities::DESKTOP
};
/// How long builds and services get to stop when the launcher quits.
const QUIT_PATIENCE: Duration = Duration::from_secs(3);

const HELP: &str = "\
launcher - start what is installed on this machine from one window

Usage: launcher [--start NAME] [--vault]

  --start NAME  optional  Start (or open, or play) the entry called NAME as
                          soon as the list has been read, as its button
                          would. May be given more than once.
  --vault       optional  Open on the password vault in place of the list.
  -h, --help    optional  Print this help.

The window lists the entries of the catalog, a JSON file in the app data
folder (macOS ~/Library/Application Support/loadngo-launcher, Linux
~/.local/share/loadngo-launcher, Windows %APPDATA%\\loadngo-launcher):
catalog.json. Edit list opens it; Reload (Cmd-R, Ctrl-R) reads it again. Its
format is in loadngo's docs/LAUNCHER.md.

An entry is a program with its own window (started, and left running when
the launcher quits; what it prints goes to output/ beside the catalog), a
program that serves a page (the page opens when it is ready; the program is
stopped when the launcher quits) or a link. Up, Down and Enter work the
list; Enter starts and opens, and never stops anything.

A launcher built with games inside it lists them first. Play gives one the
window; leaving the game (its Quit) brings the list back.

Vault (on a computer) is a password vault: it makes passwords, each one a
prime number, and keeps them in passwords.vault beside the catalog, sealed
under a master password that is kept nowhere. Copy puts a password on the
clipboard and takes it off again 43 seconds later; the vault locks itself
when left alone for 307 seconds. loadngo's docs/VAULT.md has the rest.

Set LAUNCHER_STATE_DIR to use another folder for the catalog, the output and
the vault (for test runs).

Examples:
  launcher
  launcher --start \"Code Editor\"
  launcher --vault
";

/// What a built-in runs: its whole loop, from its first frame to the frame
/// it is left on.
type Play = Box<dyn Fn() -> Pin<Box<dyn Future<Output = ()>>>>;

/// Something built into a launcher: a game, mostly, on a phone, where an
/// app can start no other app. It is listed ahead of the catalog and run in
/// the launcher's own window.
pub struct BuiltIn {
    key: String,
    entry: Entry,
    play: Play,
}

impl BuiltIn {
    /// `name` is what the list calls it, under the heading `group`, with
    /// `about` as its one line. `play` makes its loop: a future that runs on
    /// the launcher's host (`capture_frame`, `next_frame`) and returns when
    /// the player leaves. It may be made more than once.
    ///
    /// While it runs it is the launcher's guest
    /// (`loadngo_host_desktop::set_guest`) under `key`, its repository's
    /// name: a game that asks `offers_quit()` shows its Quit on a phone
    /// too, and one that asks `assets_root()` is given `games/<key>` in the
    /// launcher's assets, where the phone build puts its files.
    pub fn new<Make, Loop>(key: &str, name: &str, group: &str, about: &str, play: Make) -> Self
    where
        Make: Fn() -> Loop + 'static,
        Loop: Future<Output = ()> + 'static,
    {
        Self {
            key: key.to_string(),
            entry: Entry::built_in(name, group, about),
            play: Box::new(move || Box::pin(play())),
        }
    }
}

/// Runs the launcher until its window closes: reads the arguments, opens
/// the window, lists `built_ins` and then the catalog.
pub fn launch(built_ins: Vec<BuiltIn>) {
    let mut start = Vec::new();
    let mut on_vault = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                say(HELP, false);
                return;
            }
            "--vault" => on_vault = true,
            "--start" => match args.next() {
                Some(name) => start.push(name),
                None => {
                    say(
                        "launcher: --start needs an entry's name; see --help\n",
                        true,
                    );
                    std::process::exit(2);
                }
            },
            other => {
                say(
                    &format!("launcher: unknown argument {other}; see --help\n"),
                    true,
                );
                std::process::exit(2);
            }
        }
    }
    let supervisor = Rc::new(OnceCell::new());
    #[cfg(unix)]
    // SAFETY: registers a function that takes nothing and touches only the
    // mutex above and the clipboard.
    unsafe {
        libc::atexit(forget_at_exit);
    }
    loadngo_host_desktop::launch(
        WindowDescriptor {
            title: "loadngo Launcher".to_string(),
            width: Some(760),
            height: Some(840),
            high_dpi: true,
            linux_wm_class: Some(APP_ID),
        },
        None,
        run(Rc::clone(&supervisor), start, on_vault, built_ins),
    );
    // The window has closed. A copied password does not stay on the
    // clipboard behind it.
    forget_on_the_way_out();
    // What only the launcher was reading from stops with it; programs in
    // their own windows carry on.
    if let Some(supervisor) = supervisor.get() {
        supervisor.shutdown(QUIT_PATIENCE);
    }
}

/// Prints to standard output, or standard error when `error`.
fn say(text: &str, error: bool) {
    use std::io::Write;
    let mut out = console(error);
    let _ = out.write_all(text.as_bytes());
    let _ = out.flush();
}

#[cfg(not(windows))]
fn console(error: bool) -> Box<dyn std::io::Write> {
    if error {
        Box::new(std::io::stderr())
    } else {
        Box::new(std::io::stdout())
    }
}

/// A Windows GUI program starts with no console. Output redirected to a file
/// or pipe arrives as standard handles; otherwise this borrows the console
/// the launcher was started from, if any.
#[cfg(windows)]
fn console(error: bool) -> Box<dyn std::io::Write> {
    use std::os::windows::io::AsRawHandle;

    #[link(name = "kernel32")]
    extern "system" {
        fn AttachConsole(process_id: u32) -> i32;
    }
    const ATTACH_PARENT_PROCESS: u32 = u32::MAX;

    let inherited = if error {
        std::io::stderr().as_raw_handle()
    } else {
        std::io::stdout().as_raw_handle()
    };
    if !inherited.is_null() {
        return if error {
            Box::new(std::io::stderr())
        } else {
            Box::new(std::io::stdout())
        };
    }
    // SAFETY: AttachConsole takes a process id and touches no memory of
    // ours; failure (no parent console) leaves nothing to write to.
    if unsafe { AttachConsole(ATTACH_PARENT_PROCESS) } != 0 {
        if let Ok(file) = std::fs::OpenOptions::new().write(true).open("CONOUT$") {
            return Box::new(file);
        }
    }
    Box::new(std::io::sink())
}

/// The folder holding the catalog and the logs.
fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("LAUNCHER_STATE_DIR") {
        return PathBuf::from(dir);
    }
    match loadngo_host_desktop::app_data_dir(APP_ID) {
        Ok(dir) => PathBuf::from(dir),
        Err(error) => {
            loadngo_host_desktop::log_error(format_args!("no app data folder: {error}"));
            std::env::temp_dir().join(APP_ID)
        }
    }
}

/// The edges of the window the system keeps for itself.
fn edges(frame: &HostFrame) -> Edges {
    Edges {
        left: frame.insets.left,
        top: frame.insets.top,
        right: frame.insets.right,
        bottom: frame.insets.bottom,
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Makes an empty catalog at `path` if there is none, and opens it in the
/// system's editor for it.
fn edit_catalog(path: &Path) -> Result<(), String> {
    if !path.exists() {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
        }
        std::fs::write(path, "{\n  \"entries\": []\n}\n").map_err(|error| error.to_string())?;
    }
    loadngo_host_desktop::open_in_system(&path.to_string_lossy())
}

/// A frame of its own between the list and a guest, each way: the press
/// that chose Play must not be the game's first input, and the press that
/// left the game must not land on the list.
async fn fresh_frame() {
    loadngo_host_desktop::next_frame(FrameDemand::after(Duration::ZERO)).await;
}

/// How wide the host draws `text`, for the vault's fields.
fn measure(text: &str, font_size: u16) -> f32 {
    loadngo_host_desktop::measure_text(text, None, font_size, 1.0).width
}

/// A job's answer, with a job that did not finish told as a failure.
fn finished<T>(job: Result<Result<T, VaultError>, String>) -> Result<T, VaultError> {
    job.unwrap_or_else(|why| Err(VaultError::File(why)))
}

/// Empties the clipboard if `text` is still what it holds: something copied
/// since is left alone.
fn forget(text: &Secret) {
    if let Ok(Some(held)) = loadngo_host_desktop::read_clipboard_text() {
        let held = zeroize::Zeroizing::new(held);
        if held.as_str() == text.as_str() {
            let _ = loadngo_host_desktop::write_clipboard_text("");
        }
    }
}

/// A copied password that has not been taken off the clipboard yet. The
/// vault's screens do that 43 seconds after the copy; this is for a launcher
/// that is quit sooner, which would otherwise leave it there for good.
static COPIED: std::sync::Mutex<Option<Secret>> = std::sync::Mutex::new(None);

fn note_copied(text: Option<Secret>) {
    if let Ok(mut copied) = COPIED.lock() {
        *copied = text;
    }
}

/// Takes a copied password off the clipboard on the way out. Reached when
/// the window closes and, on Unix, at `exit` (the Dock's Quit). Not when a
/// signal or a crash ends the launcher where it stands.
fn forget_on_the_way_out() {
    let copied = COPIED.lock().ok().and_then(|mut copied| copied.take());
    if let Some(text) = copied {
        forget(&text);
    }
}

#[cfg(unix)]
extern "C" fn forget_at_exit() {
    forget_on_the_way_out();
}

/// The jobs under way for the vault, each on another thread.
#[derive(Default)]
struct VaultJobs {
    unlocking: Option<Offloaded<Result<Unlocked, VaultError>>>,
    writing: Option<Offloaded<Result<Vec<u8>, VaultError>>>,
    making: Option<Offloaded<Result<Secret, VaultError>>>,
    copying: Vec<(Secret, Offloaded<Result<(), String>>)>,
    pasting: Option<Offloaded<Result<Option<String>, String>>>,
    forgetting: Vec<Offloaded<()>>,
}

impl VaultJobs {
    /// Hands what has finished to the vault's screens.
    fn deliver(&mut self, app: &mut LauncherApp) {
        if let Some(result) = self.unlocking.as_mut().and_then(Offloaded::try_take) {
            self.unlocking = None;
            app.vault().unlocked(finished(result));
        }
        if let Some(result) = self.writing.as_mut().and_then(Offloaded::try_take) {
            self.writing = None;
            app.vault().written(finished(result));
        }
        if let Some(result) = self.making.as_mut().and_then(Offloaded::try_take) {
            self.making = None;
            app.vault().made(finished(result));
        }
        if let Some(result) = self.pasting.as_mut().and_then(Offloaded::try_take) {
            self.pasting = None;
            app.vault()
                .pasted(result.and_then(|read| read).unwrap_or(None));
        }
        let mut waiting = Vec::new();
        for (text, mut job) in self.copying.drain(..) {
            match job.try_take() {
                Some(result) => {
                    let result = result.and_then(|wrote| wrote);
                    if result.is_ok() {
                        note_copied(Some(text.clone()));
                    }
                    app.vault().copied(text, result);
                }
                None => waiting.push((text, job)),
            }
        }
        self.copying = waiting;
        self.forgetting.retain_mut(|job| job.try_take().is_none());
    }

    /// Starts what the vault's screens asked for.
    fn start(&mut self, ask: Ask, path: &Path, app: &mut LauncherApp) {
        match ask {
            Ask::Look => app.vault().found(Vault::exists(path)),
            Ask::Create { master } => {
                let path = path.to_path_buf();
                self.unlocking = Some(loadngo_host_desktop::offload(move || {
                    vault::create(&path, &master, Costs::STANDARD)
                }));
            }
            Ask::Open { master } => {
                let path = path.to_path_buf();
                self.unlocking = Some(loadngo_host_desktop::offload(move || {
                    vault::unlock(&path, &master)
                }));
            }
            // The screens ask for one write at a time.
            Ask::Write(sealed) => {
                self.writing = Some(loadngo_host_desktop::offload(move || sealed.write()));
            }
            Ask::Make(recipe) => {
                self.making = Some(loadngo_host_desktop::offload(move || vault::make(recipe)));
            }
            Ask::Copy { text } => {
                let copy = text.clone();
                self.copying.push((
                    text,
                    loadngo_host_desktop::offload(move || {
                        loadngo_host_desktop::write_clipboard_text(copy.as_str())
                    }),
                ));
            }
            Ask::Forget { text } => {
                note_copied(None);
                self.forgetting
                    .push(loadngo_host_desktop::offload(move || forget(&text)));
            }
            Ask::Paste => {
                self.pasting = Some(loadngo_host_desktop::offload(
                    loadngo_host_desktop::read_clipboard_text,
                ));
            }
        }
    }
}

async fn run(
    shared: Rc<OnceCell<Supervisor>>,
    start: Vec<String>,
    on_vault: bool,
    built_ins: Vec<BuiltIn>,
) {
    let data = data_dir();
    let catalog_path = data.join("catalog.json");
    let vault_path = data.join("passwords.vault");
    // Not `logs`: the host keeps its own log there.
    let logs = data.join("output");
    let home = home_dir();
    let waker = loadngo_host_desktop::frame_waker();
    // Where programs cannot be started there is no shell to ask for a PATH.
    let search = if ABILITIES.starts_programs {
        SearchPath::UserShell
    } else {
        SearchPath::Inherited
    };
    let supervisor =
        shared.get_or_init(|| Supervisor::new(logs.clone(), search, move || waker.wake()));
    let mut app = LauncherApp::new(catalog_path.clone(), logs)
        .with_abilities(ABILITIES)
        .with_built_ins(built_ins.iter().map(|own| own.entry.clone()).collect());
    for name in start {
        app.start_when_loaded(name);
    }
    app.vault().set_measure(measure);
    if on_vault {
        app.show_vault();
    }
    let mut vault_jobs = VaultJobs::default();
    let mut loading: Option<Offloaded<Loaded>> = None;
    // Reload was asked while a read was under way: read once more after it.
    let mut load_again = false;
    let mut opening: Vec<(String, Offloaded<Result<(), String>>)> = Vec::new();
    let mut scene = Vec::new();

    loop {
        let frame = loadngo_host_desktop::capture_frame();
        if let Some(result) = loading.as_mut().and_then(Offloaded::try_take) {
            loading = None;
            app.apply_catalog(result.unwrap_or_else(Loaded::Wrong));
        }
        opening.retain_mut(|(target, job)| match job.try_take() {
            Some(result) => {
                app.opened(target, result.unwrap_or_else(Err));
                false
            }
            None => true,
        });
        for event in supervisor.take_events() {
            app.apply_process(event);
        }

        vault_jobs.deliver(&mut app);
        app.tick(Instant::now());

        app.set_edges(edges(&frame));
        app.frame(&frame.input, (frame.surface.width, frame.surface.height));

        // The built-in to give the window to once this frame is drawn. One
        // at a time: a second asked for in the same frame is not played.
        let mut play = None;
        for effect in app.take_effects() {
            match effect {
                Effect::LoadCatalog => load_again = true,
                Effect::EditCatalog => {
                    let path = catalog_path.clone();
                    let target = path.to_string_lossy().into_owned();
                    opening.push((
                        target,
                        loadngo_host_desktop::offload(move || edit_catalog(&path)),
                    ));
                }
                Effect::Start { run, entry } => supervisor.start(run, &entry),
                Effect::Stop { run } => supervisor.stop(run),
                Effect::Open { target } => {
                    let open = target.clone();
                    opening.push((
                        target,
                        loadngo_host_desktop::offload(move || {
                            loadngo_host_desktop::open_in_system(&open)
                        }),
                    ));
                }
                Effect::Play { name } => {
                    if play.is_none() {
                        play = built_ins.iter().find(|own| own.entry.name == name);
                    }
                }
                Effect::Vault(ask) => vault_jobs.start(ask, &vault_path, &mut app),
            }
        }
        if load_again && loading.is_none() {
            load_again = false;
            let path = catalog_path.clone();
            let home = home.clone();
            loading = Some(loadngo_host_desktop::offload(move || {
                catalog::load(&path, home.as_deref())
            }));
        }

        scene.clear();
        app.paint(&mut scene);
        loadngo_host_desktop::clear(draw::BACKGROUND);
        loadngo_host_desktop::render_widget_paint_ops(&scene);

        if let Some(own) = play {
            fresh_frame().await;
            loadngo_host_desktop::set_guest(Some(&own.key));
            (own.play)().await;
            loadngo_host_desktop::set_guest(None);
            fresh_frame().await;
            continue;
        }

        // Nothing here moves by itself: input, a finished read or open, and
        // news of a started program each bring the next frame. The vault
        // adds two things due by the clock (a copied password to take off
        // the clipboard, a vault left alone to lock): then the frame comes
        // no later than the first of them.
        let demand = match app.wake_in() {
            Some(due) => FrameDemand::idle_until(due),
            None => FrameDemand::idle(),
        };
        loadngo_host_desktop::next_frame(demand).await;
    }
}
