//! Desktop validation surface for `ui_core::FileDialogModel` against the real
//! filesystem. Starts with an Open dialog showing; `--save` starts with Save.
//! When a dialog closes, the outcome is shown and `O` / `S` open another.
//!
//! ```bash
//! cargo run -p loadngo-host-desktop --bin file_dialog_harness
//! cargo run -p loadngo-host-desktop --bin file_dialog_harness -- --save
//! ```

use std::path::PathBuf;
use std::time::Duration;

use loadngo_host_core::{FrameDemand, HostKey, WindowDescriptor};
use ui_core::{
    Color, DirectorySource, FileDialogModel, FileDialogOutcome, FileTypeFilter, HorizontalAlign,
    PaintOp, Rect, StdDirectorySource, TextStyle, VerticalAlign,
};

const WINDOW_WIDTH: i32 = 1100;
const WINDOW_HEIGHT: i32 = 760;
const DIALOG_WIDTH: f32 = 820.0;
const DIALOG_HEIGHT: f32 = 560.0;

fn main() {
    let start_in_save = std::env::args().any(|arg| arg == "--save");
    loadngo_host_desktop::launch(window_descriptor(), None, async move {
        run_file_dialog_harness(start_in_save).await;
    });
}

fn window_descriptor() -> WindowDescriptor {
    WindowDescriptor {
        title: "loadngo file dialog harness".to_string(),
        width: Some(WINDOW_WIDTH),
        height: Some(WINDOW_HEIGHT),
        high_dpi: true,
        linux_wm_class: Some("loadngo-file-dialog-harness"),
    }
}

fn start_directory() -> PathBuf {
    StdDirectorySource
        .home_dir()
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn open_dialog() -> FileDialogModel {
    let mut dialog = FileDialogModel::open(
        "Open a file",
        start_directory(),
        None,
        Box::new(StdDirectorySource),
    );
    dialog.set_places(StdDirectorySource.standard_places());
    dialog
}

fn save_dialog() -> FileDialogModel {
    let mut dialog = FileDialogModel::save(
        "Save a WAV file",
        start_directory(),
        "untitled.wav",
        Some(FileTypeFilter::new("WAV audio", &["wav"])),
        Box::new(StdDirectorySource),
    );
    dialog.set_places(StdDirectorySource.standard_places());
    dialog
}

async fn run_file_dialog_harness(start_in_save: bool) {
    let mut dialog = Some(if start_in_save {
        save_dialog()
    } else {
        open_dialog()
    });
    let mut last_outcome = String::from("(no dialog closed yet)");

    loop {
        let frame = loadngo_host_desktop::capture_frame();
        let surface = Rect {
            x: 0.0,
            y: 0.0,
            width: frame.surface.width,
            height: frame.surface.height,
        };
        let mut scene = Vec::new();
        scene.push(PaintOp::FillRect {
            rect: surface,
            color: Color::rgba(0x10, 0x15, 0x1d, 0xff),
        });
        push_line(
            &mut scene,
            40.0,
            "O: open dialog   S: save dialog   Esc (no dialog): quit",
        );
        push_line(&mut scene, 70.0, &format!("Last outcome: {last_outcome}"));

        match dialog.as_mut() {
            Some(active) => {
                active.set_bounds(Rect {
                    x: ((surface.width - DIALOG_WIDTH) / 2.0).max(0.0),
                    y: ((surface.height - DIALOG_HEIGHT) / 2.0).max(0.0),
                    width: DIALOG_WIDTH.min(surface.width),
                    height: DIALOG_HEIGHT.min(surface.height),
                });
                active.relayout(measure_width);
                let mut outcome = None;
                for event in frame.input.ui_events() {
                    if let Some(done) = active.handle_event(event).outcome {
                        outcome = Some(done);
                        break;
                    }
                }
                active.scroll_wheel(
                    frame.input.mouse_pointer().position,
                    frame.input.mouse_wheel_y,
                    frame.input.mouse_wheel_precise,
                );
                active.advance(frame.timing.delta_seconds);
                active.relayout(measure_width);
                loadngo_host_desktop::set_text_cursor_active(
                    active.prefers_text_cursor(frame.input.mouse_pointer().position),
                );

                FileDialogModel::paint_scrim(surface, &mut scene);
                active.paint(&mut scene);

                if let Some(outcome) = outcome {
                    last_outcome = match outcome {
                        FileDialogOutcome::Confirmed(path) => {
                            format!("{:?} -> {}", active.mode(), path.display())
                        }
                        FileDialogOutcome::Cancelled => format!("{:?} cancelled", active.mode()),
                    };
                    println!("file_dialog_harness: {last_outcome}");
                    dialog = None;
                    loadngo_host_desktop::set_text_cursor_active(false);
                }
            }
            None => {
                if frame.input.key_pressed(HostKey::Escape) {
                    break;
                }
                if frame.input.key_pressed(HostKey::S) {
                    dialog = Some(save_dialog());
                } else if frame.typed_text_contains('o') {
                    dialog = Some(open_dialog());
                }
            }
        }

        loadngo_host_desktop::render_widget_paint_ops(&scene);
        loadngo_host_desktop::next_frame(FrameDemand::after(Duration::from_millis(16))).await;
    }
}

trait TypedText {
    fn typed_text_contains(&self, ch: char) -> bool;
}

impl TypedText for loadngo_host_core::HostFrame {
    /// `HostKey` has no `O`, so read it from typed text instead.
    fn typed_text_contains(&self, ch: char) -> bool {
        self.input.typed_text.to_lowercase().contains(ch)
    }
}

fn push_line(scene: &mut Vec<PaintOp>, y: f32, text: &str) {
    scene.push(PaintOp::Text {
        rect: Rect {
            x: 40.0,
            y,
            width: 1000.0,
            height: 24.0,
        },
        clip_rect: None,
        text: text.to_string(),
        style: TextStyle {
            color: Color::rgba(0xd8, 0xe1, 0xf0, 0xff),
            font_size: 16,
            horizontal_align: HorizontalAlign::Left,
            vertical_align: VerticalAlign::Middle,
            ..TextStyle::default()
        },
    });
}

fn measure_width(text: &str, font_size: u16) -> f32 {
    loadngo_host_desktop::measure_text_metrics(text, None, font_size, 1.0).width
}
