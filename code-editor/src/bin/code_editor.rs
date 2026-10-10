//! `code_editor [FOLDER]`: the loadngo code editor. See docs/CODE_EDITOR.md.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::Instant;

use loadngo_code_editor::cargo_check::{self, Cancel, CheckOutcome};
use loadngo_code_editor::lsp::{transport::find_program, LspProcess};
use loadngo_code_editor::{perform, Editor, EditorHost, IoRequest, IoResponse};
use loadngo_host_core::{FrameDemand, WindowDescriptor};
use loadngo_host_desktop::Offloaded;

const APP_ID: &str = "loadngo-code-editor";
/// File jobs on the offload workers at once; the rest wait their turn.
const MAX_IN_FLIGHT: usize = 8;

const HELP: &str = "\
code_editor - edit the files of a folder in tabs

Usage: code_editor [FOLDER]

  FOLDER       optional  The folder to show. Without it, the folder open
                         when the editor last closed, or else the current
                         directory.
  -h, --help   optional  Print this help.

Everything is in the File and Edit menus (the system menu bar on macOS, a
menu bar in the window elsewhere) and on the toolbar. Keys: Cmd-S saves the
file in front, Cmd-Shift-S saves all files, plus the platform's own open,
close, quit, undo, redo, cut, copy, paste, select all and find (Ctrl instead
of Cmd on Linux and Windows).

Open tabs, the folder and unsaved edits are kept in the app data folder
(on macOS ~/Library/Application Support/loadngo-code-editor), so closing the
window loses nothing; unsaved edits come back at the next launch.

Example:
  code_editor ~/pudding/loadngo
";

fn main() {
    let mut folder = None;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{HELP}");
                return;
            }
            flag if flag.starts_with('-') => {
                eprintln!("code_editor: unknown option {flag}; see --help");
                std::process::exit(2);
            }
            path if folder.is_none() => folder = Some(PathBuf::from(path)),
            extra => {
                eprintln!(
                    "code_editor: only one folder can be given (also got {extra}); see --help"
                );
                std::process::exit(2);
            }
        }
    }
    let folder = folder.map(|path| std::fs::canonicalize(&path).unwrap_or(path));
    loadngo_host_desktop::launch(
        WindowDescriptor {
            title: "Code Editor".to_string(),
            width: Some(1440),
            height: Some(920),
            high_dpi: true,
            linux_wm_class: Some("loadngo-code-editor"),
        },
        None,
        run(folder),
    );
}

struct DesktopHost;

impl EditorHost for DesktopHost {
    fn measure(&self, text: &str, font_size: u16) -> f32 {
        loadngo_host_desktop::measure_text_metrics(text, None, font_size, 1.0).width
    }

    fn read_clipboard(&self) -> Option<String> {
        loadngo_host_desktop::read_clipboard_text().ok().flatten()
    }

    fn write_clipboard(&self, text: &str) {
        if let Err(error) = loadngo_host_desktop::write_clipboard_text(text) {
            loadngo_host_desktop::log_error(format_args!("clipboard write failed: {error}"));
        }
    }
}

/// The current directory, unless it is the filesystem root (as when the app
/// is opened from the Finder), then the home directory.
fn fallback_root() -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    if cwd.parent().is_some() {
        return cwd;
    }
    std::env::var_os("HOME").map_or(cwd, PathBuf::from)
}

async fn run(folder: Option<PathBuf>) {
    let host = DesktopHost;
    let state_dir = loadngo_host_desktop::app_data_dir(APP_ID)
        .map(PathBuf::from)
        .map_err(|error| {
            loadngo_host_desktop::log_error(format_args!("no app data folder: {error}"))
        })
        .ok();
    let mut editor = Editor::new(state_dir, folder, fallback_root(), Instant::now());
    // macOS shows the menus in the system menu bar; elsewhere the editor
    // draws its own.
    // CODE_EDITOR_DRAWN_MENU=1 shows the drawn menu bar on macOS too, to
    // check what Linux and Windows get.
    let native_menu = std::env::var_os("CODE_EDITOR_DRAWN_MENU").is_none()
        && loadngo_host_desktop::set_menu_bar(&editor.menu_bar());
    editor.set_native_menu(native_menu);
    let mut waiting: VecDeque<IoRequest> = VecDeque::new();
    let mut in_flight: Vec<Offloaded<IoResponse>> = Vec::new();
    let mut scene = Vec::new();
    // The one cargo check running: its id, how to stop it, its result.
    let mut check: Option<(u64, Cancel, Offloaded<CheckOutcome>)> = None;
    // rust-analyzer, while one runs; its messages wake the frame.
    let mut server: Option<LspProcess> = None;
    let waker = loadngo_host_desktop::frame_waker();
    let trace = std::env::var_os("CODE_EDITOR_TRACE").is_some();

    loop {
        let frame = loadngo_host_desktop::capture_frame();
        let now = Instant::now();

        in_flight.retain_mut(|job| match job.try_take() {
            Some(Ok(response)) => {
                editor.apply(response, now);
                false
            }
            Some(Err(panic)) => {
                loadngo_host_desktop::log_error(format_args!("file job failed: {panic}"));
                false
            }
            None => true,
        });

        if let Some((id, _, result)) = &mut check {
            match result.try_take() {
                Some(Ok(outcome)) => {
                    editor.apply_check(*id, outcome);
                    check = None;
                }
                Some(Err(error)) => {
                    loadngo_host_desktop::log_error(format_args!("cargo check failed: {error}"));
                    check = None;
                }
                None => {}
            }
        }

        if let Some(process) = &server {
            let messages = process.drain();
            if !messages.is_empty() {
                editor.apply_lsp(messages);
            }
            if let Some(reason) = process.ended() {
                editor.lsp_ended(reason);
                server = None;
            }
        }

        let outcome = editor.frame(
            &frame.input,
            frame.focused,
            (frame.surface.width, frame.surface.height),
            now,
            &host,
        );
        loadngo_host_desktop::set_text_cursor_active(outcome.text_cursor);
        if let Some(menu_bar) = &outcome.menu_bar {
            loadngo_host_desktop::set_menu_bar(menu_bar);
        }

        if let Some(request) = editor.take_check_request() {
            if let Some((_, cancel, _)) = check.take() {
                cancel.cancel();
            }
            check = start_check(request.id, request.file);
            if check.is_none() {
                editor.apply_check(
                    request.id,
                    CheckOutcome::failed("could not start a thread for cargo"),
                );
            }
        }

        if editor.take_lsp_stop() {
            if let Some(process) = &server {
                for message in editor.lsp_outgoing() {
                    process.send(&message);
                }
            }
            server = None;
        }
        if let Some(root) = editor.take_lsp_start() {
            let wake = waker.clone();
            match LspProcess::start(&find_program("rust-analyzer"), &root, move || wake.wake()) {
                Ok(process) => server = Some(process),
                Err(error) => editor.lsp_ended(format!("cannot start rust-analyzer: {error}")),
            }
        }
        if let Some(process) = &server {
            for message in editor.lsp_outgoing() {
                process.send(&message);
            }
        }

        waiting.extend(editor.take_requests());
        while in_flight.len() < MAX_IN_FLIGHT {
            let Some(request) = waiting.pop_front() else {
                break;
            };
            in_flight.push(loadngo_host_desktop::offload(move || perform(request)));
        }

        // Quit once the last saves and the session write have landed.
        if outcome.quit && waiting.is_empty() && in_flight.is_empty() {
            if let Some((_, cancel, _)) = check.take() {
                cancel.cancel();
            }
            break;
        }

        scene.clear();
        editor.paint(&mut scene);
        loadngo_host_desktop::clear(loadngo_code_editor::theme::BACKGROUND);
        loadngo_host_desktop::render_widget_paint_ops(&scene);
        if trace {
            eprintln!(
                "frame work {:.2} ms, {} paint ops, {} file jobs",
                now.elapsed().as_secs_f64() * 1000.0,
                scene.len(),
                in_flight.len()
            );
        }

        // Input and finished file jobs bring the next frame; otherwise only
        // the caret's blink or a due backup or session write does.
        let demand = match editor.next_wake(Instant::now()) {
            Some(delay) => FrameDemand::idle_until(delay),
            None => FrameDemand::idle(),
        };
        loadngo_host_desktop::next_frame(demand).await;
    }
}

/// Runs `cargo check` on a thread of its own (it can take minutes, so not on
/// an offload worker); the result comes back like an offloaded job's.
fn start_check(id: u64, file: PathBuf) -> Option<(u64, Cancel, Offloaded<CheckOutcome>)> {
    let (completer, result) = loadngo_host_desktop::completion();
    let cancel = Cancel::default();
    let running = cancel.clone();
    let spawned = std::thread::Builder::new()
        .name("cargo-check".to_string())
        .spawn(move || completer.complete(cargo_check::run(&file, &running)));
    match spawned {
        Ok(_) => Some((id, cancel, result)),
        Err(error) => {
            loadngo_host_desktop::log_error(format_args!("cannot start cargo check: {error}"));
            None
        }
    }
}
