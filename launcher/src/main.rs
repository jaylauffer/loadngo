//! The loadngo launcher: `launcher::LauncherApp` on `loadngo-host-desktop`,
//! the same source on macOS, Linux and Windows.

// A window, not a console program: Windows opens no console beside it.
#![cfg_attr(windows, windows_subsystem = "windows")]

use std::cell::OnceCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use launcher::catalog::{self, Loaded};
use launcher::process::{SearchPath, Supervisor};
use launcher::{draw, Effect, LauncherApp};
use loadngo_host_core::{FrameDemand, WindowDescriptor};
use loadngo_host_desktop::Offloaded;

const APP_ID: &str = "loadngo-launcher";
/// How long builds and services get to stop when the launcher quits.
const QUIT_PATIENCE: Duration = Duration::from_secs(3);

const HELP: &str = "\
launcher - start what is installed on this machine from one window

Usage: launcher [--start NAME]

  --start NAME  optional  Start (or open) the entry called NAME as soon as
                          the list has been read, as its button would. May
                          be given more than once.
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

Set LAUNCHER_STATE_DIR to use another folder for the catalog and the output
(for test runs).

Examples:
  launcher
  launcher --start \"Code Editor\"
";

fn main() {
    let mut start = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                say(HELP, false);
                return;
            }
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
    loadngo_host_desktop::launch(
        WindowDescriptor {
            title: "loadngo Launcher".to_string(),
            width: Some(760),
            height: Some(840),
            high_dpi: true,
            linux_wm_class: Some(APP_ID),
        },
        None,
        run(Rc::clone(&supervisor), start),
    );
    // The window has closed. What only the launcher was reading from stops
    // with it; programs in their own windows carry on.
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

async fn run(shared: Rc<OnceCell<Supervisor>>, start: Vec<String>) {
    let data = data_dir();
    let catalog_path = data.join("catalog.json");
    // Not `logs`: the host keeps its own log there.
    let logs = data.join("output");
    let home = home_dir();
    let waker = loadngo_host_desktop::frame_waker();
    let supervisor = shared
        .get_or_init(|| Supervisor::new(logs.clone(), SearchPath::UserShell, move || waker.wake()));
    let mut app = LauncherApp::new(catalog_path.clone(), logs);
    for name in start {
        app.start_when_loaded(name);
    }
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

        app.frame(&frame.input, (frame.surface.width, frame.surface.height));

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

        // Nothing here moves by itself: input, a finished read or open, and
        // news of a started program each bring the next frame.
        loadngo_host_desktop::next_frame(FrameDemand::idle()).await;
    }
}
