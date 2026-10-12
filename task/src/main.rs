//! Task application entrypoint (Rust Win32 port of TaskWindow/TaskMainWnd).

#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod date_banner;
#[cfg(windows)]
mod day_plan;
#[cfg(windows)]
mod day_planner;
#[cfg(windows)]
mod dragdrop;
#[cfg(windows)]
mod project_plan;
#[cfg(windows)]
mod project_planner;
#[cfg(windows)]
mod tabs;
#[cfg(windows)]
mod task_list;
#[cfg(windows)]
mod task_window;
#[cfg(windows)]
mod toolbar;
#[cfg(windows)]
mod winutil;

#[cfg(windows)]
use anyhow::Result;
#[cfg(windows)]
use tracing::Level;
#[cfg(windows)]
use tracing_subscriber::{fmt::writer::MakeWriterExt, EnvFilter};
#[cfg(windows)]
use windows::Win32::{
    Foundation::HWND,
    System::{
        LibraryLoader::GetModuleHandleW,
        Ole::{OleInitialize, OleUninitialize},
    },
    UI::WindowsAndMessaging::{ShowWindow, SW_SHOW},
};

#[cfg(windows)]
use task_window::{
    build_services, create_main_window, init_common_controls, load_all, message_loop,
    register_window_class,
};

#[cfg(windows)]
fn main() -> Result<()> {
    let file_appender = tracing_appender::rolling::never(".", "task.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive(Level::DEBUG.into()))
        .with_writer(non_blocking.and(std::io::stdout))
        .init();

    unsafe {
        // Match legacy startup: OleInitialize instead of CoInitializeEx.
        OleInitialize(None)
            .ok()
            .ok_or_else(|| anyhow::anyhow!("OleInitialize failed"))?;
        init_common_controls();
        let hinstance = GetModuleHandleW(None)?.into();
        let (mut service, mut network) = build_services()?;
        load_all(&mut service, "user_plan");
        network.init()?;
        register_window_class(hinstance)?;
        let hwnd: HWND = create_main_window(hinstance, service, network, "user_plan".to_string())?;
        let _ = ShowWindow(hwnd, SW_SHOW);
        tracing::info!("TaskWindow started");
        message_loop();
        OleUninitialize();
    }
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    desktop::main();
}

/// The Task window on `loadngo-host-desktop`: `task::TaskApp` with its file
/// work on the host's offload workers, whose results come back as host
/// proactor completions, and its save deadline as a host frame deadline.
#[cfg(not(windows))]
mod desktop {
    use std::collections::VecDeque;
    use std::path::PathBuf;
    use std::time::Instant;

    use loadngo_host_core::{FrameDemand, WindowDescriptor};
    use loadngo_host_desktop::Offloaded;
    use task::store::{perform, IoRequest, IoResponse};
    use task::{TaskApp, TaskHost};

    const APP_ID: &str = "loadngo-task";
    /// File jobs on the offload workers at once.
    const MAX_IN_FLIGHT: usize = 2;

    const HELP: &str = "\
task - plan your day and your projects

Usage: task

  -h, --help   optional  Print this help.

Two tabs. Day Plan: click a time in the Planned or Actual column and type a
task, or drag one from the list on the right; drag an entry to move it, its
bottom edge to change its length. Project Plan: projects on the right, the
selected one's tasks as a tree on the left; drag a box onto another to move
it there. Drag anything onto the bin at the top right to delete it; Undo
brings it back.

The plan is saved a second after each change, in the app data folder (on
macOS ~/Library/Application Support/loadngo-task): user_plan.json,
user_plan-entries.json and config.json, the files the Windows Task keeps in
its taskdata folder, so that folder can be copied here. Set TASK_STATE_DIR
to keep the plan elsewhere (for test runs).

Example:
  task
";

    pub fn main() {
        if let Some(arg) = std::env::args().nth(1) {
            match arg.as_str() {
                "-h" | "--help" => print!("{HELP}"),
                other => {
                    eprintln!("task: unknown argument {other}; see --help");
                    std::process::exit(2);
                }
            }
            return;
        }
        loadngo_host_desktop::launch(
            WindowDescriptor {
                title: "loadngo Task".to_string(),
                width: Some(1100),
                height: Some(760),
                high_dpi: true,
                linux_wm_class: Some("loadngo-task"),
            },
            None,
            run(),
        );
    }

    struct DesktopHost;

    impl TaskHost for DesktopHost {
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

    fn owner() -> String {
        std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_else(|_| "local-user".to_string())
    }

    async fn run() {
        let host = DesktopHost;
        let data_dir = match std::env::var_os("TASK_STATE_DIR") {
            Some(dir) => Some(PathBuf::from(dir)),
            None => loadngo_host_desktop::app_data_dir(APP_ID)
                .map(PathBuf::from)
                .map_err(|error| {
                    loadngo_host_desktop::log_error(format_args!("no app data folder: {error}"))
                })
                .ok(),
        };
        let mut app = TaskApp::new(data_dir, &owner(), task::clock::today());
        // macOS shows the menus in the system menu bar; elsewhere the window
        // draws its own.
        let native_menu = loadngo_host_desktop::set_menu_bar(&ui_core::MenuBar::default());
        app.set_native_menu(native_menu);
        let mut waiting: VecDeque<IoRequest> = VecDeque::new();
        let mut in_flight: Vec<Offloaded<IoResponse>> = Vec::new();
        let mut scene = Vec::new();

        loop {
            let frame = loadngo_host_desktop::capture_frame();
            let now = Instant::now();
            in_flight.retain_mut(|job| match job.try_take() {
                Some(Ok(response)) => {
                    app.apply(response, now);
                    false
                }
                Some(Err(panic)) => {
                    loadngo_host_desktop::log_error(format_args!("file job failed: {panic}"));
                    false
                }
                None => true,
            });

            let outcome = app.frame(
                &frame.input,
                (frame.surface.width, frame.surface.height),
                now,
                &host,
            );
            loadngo_host_desktop::set_text_cursor_active(outcome.text_cursor);
            if let Some(menu_bar) = &outcome.menu_bar {
                loadngo_host_desktop::set_menu_bar(menu_bar);
            }

            waiting.extend(app.take_requests());
            // Saves run in order: the next one starts when the last lands.
            while in_flight.len() < MAX_IN_FLIGHT {
                let Some(request) = waiting.pop_front() else {
                    break;
                };
                in_flight.push(loadngo_host_desktop::offload(move || perform(request)));
            }

            if outcome.quit && waiting.is_empty() && in_flight.is_empty() {
                break;
            }

            scene.clear();
            app.paint(&mut scene, &host);
            loadngo_host_desktop::clear(task_background());
            loadngo_host_desktop::render_widget_paint_ops(&scene);

            // Input and finished file jobs bring the next frame; otherwise
            // only a save that is due does.
            let demand = match app.next_wake(Instant::now()) {
                Some(delay) => FrameDemand::idle_until(delay),
                None => FrameDemand::idle(),
            };
            loadngo_host_desktop::next_frame(demand).await;
        }
    }

    fn task_background() -> ui_core::Color {
        ui_core::Color::rgba(0xf4, 0xf4, 0xf2, 0xff)
    }
}
