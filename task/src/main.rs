//! The Task app: `task::TaskApp` on `loadngo-host-desktop`, the same source
//! on macOS, Linux and Windows.

fn main() {
    desktop::main();
}

/// The Task window on `loadngo-host-desktop`: `task::TaskApp` with its file
/// work on the host's offload workers, whose results come back as host
/// proactor completions, and its save deadline as a host frame deadline.
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

The plan is saved a second after each change, in the app data folder
(macOS ~/Library/Application Support/loadngo-task, Linux
~/.local/share/loadngo-task, Windows %APPDATA%\\loadngo-task):
user_plan.json, user_plan-entries.json and config.json, the files the old
Win32 Task kept in its taskdata folder, so that folder can be copied there.
Set TASK_STATE_DIR to keep the plan elsewhere (for test runs).

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
