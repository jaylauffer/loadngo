//! `bitmap_editor`: the editor window, and the same pictures without one
//! (`--probe`, `--edit`, `--render`) for scripts and checks.

// A window, not a console program: Windows opens no console beside it. The
// headless modes borrow the console they were started from (`console`).
#![cfg_attr(windows, windows_subsystem = "windows")]

use std::path::PathBuf;
use std::process::ExitCode;

use bitmap_editor::editor::Editor;
use bitmap_editor::picture::{self, Format, SaveRequest};
use bitmap_editor::render;

const HELP: &str = "\
bitmap_editor - look at and edit pictures pixel by pixel

Usage:
  bitmap_editor [FILE]
  bitmap_editor --probe FILE [--frame N] [--pixel X,Y]
  bitmap_editor --edit FILE [--frame N] [--set-alpha X,Y,A]... [--premultiply]
                [--make-opaque] --save OUT
  bitmap_editor --render FILE --out PNG --size WxH [--frame N] [--hover X,Y]

  FILE                optional  Opens the window on FILE (none: an empty window).
  --probe FILE        required for probing  Prints the format, size and frame
                                count, and with --pixel one pixel.
  --edit FILE         required for editing  Applies the edits in order and saves.
  --render FILE       required for rendering  Draws the editor window into a PNG
                                with the software rasterizer (no text: fonts
                                belong to a window) and prints the hovered
                                pixel's cell.
  --frame N           optional  The frame, counting from 0 (default 0).
  --pixel X,Y         optional  Prints \"pixel: X,Y = rrggbbaa\".
  --set-alpha X,Y,A   optional  Sets pixel X,Y's alpha to A (0-255), keeping
                                its colour. Repeatable.
  --premultiply       optional  Multiplies every colour by its alpha.
  --make-opaque       optional  Sets every alpha to 255.
  --save OUT          required with --edit  Where to write; .png, .jpg, .bmp,
                                .tiff or .gif. An animated GIF saved as .gif
                                keeps every frame; otherwise the one frame.
  --out PNG           required with --render  The picture of the window.
  --size WxH          required with --render  The window's size in pixels.
  --hover X,Y         optional  The pixel the mouse is over, for --render.
  -h, --help          optional  Prints this help.

PNG, JPEG, BMP, TIFF and GIF are read directly; video (MP4, MOV, WebM, MKV,
AVI) and other formats such as WebP through ffmpeg and ffprobe, found on PATH
or in the usual Homebrew and /usr/local places.

In the window: File > Open (or drop a file on the window), Save, Save As;
Edit > Undo, Redo; Image > Pre-Multiply Alpha, Make Opaque, Previous Frame,
Next Frame. A click on a cell of the grid sets that pixel's alpha to the
slider's value; the arrow keys move the hovered pixel and Enter sets its
alpha. Press and drag on the picture, or use the wheel over the grid, to move
the part the grid shows.

Examples:
  bitmap_editor photo.png
  bitmap_editor --probe clip.mp4 --frame 5 --pixel 10,10
  bitmap_editor --edit icon.png --set-alpha 3,4,128 --premultiply --save out.png
  bitmap_editor --render icon.png --out scene.png --size 1000x700 --hover 10,10
";

#[derive(Debug, Clone, PartialEq)]
enum Edit {
    SetAlpha(u32, u32, u8),
    Premultiply,
    MakeOpaque,
}

#[derive(Debug, Clone, PartialEq)]
enum Mode {
    Help,
    Window(Option<PathBuf>),
    Probe {
        file: PathBuf,
        frame: usize,
        pixel: Option<(u32, u32)>,
    },
    Edit {
        file: PathBuf,
        frame: usize,
        edits: Vec<Edit>,
        save: PathBuf,
    },
    Render {
        file: PathBuf,
        frame: usize,
        out: PathBuf,
        size: (u32, u32),
        hover: Option<(u32, u32)>,
    },
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = match parse(&args) {
        Ok(mode) => mode,
        Err(error) => {
            say(&format!("bitmap_editor: {error}; see --help\n"), true);
            return ExitCode::from(2);
        }
    };
    let result = match mode {
        Mode::Help => {
            say(HELP, false);
            Ok(())
        }
        Mode::Window(file) => {
            window::open(file);
            Ok(())
        }
        Mode::Probe { file, frame, pixel } => probe(file, frame, pixel),
        Mode::Edit {
            file,
            frame,
            edits,
            save,
        } => edit(file, frame, &edits, save),
        Mode::Render {
            file,
            frame,
            out,
            size,
            hover,
        } => render_to(file, frame, out, size, hover),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            say(&format!("bitmap_editor: {error:#}\n"), true);
            ExitCode::FAILURE
        }
    }
}

fn parse(args: &[String]) -> Result<Mode, String> {
    let Some(first) = args.first() else {
        return Ok(Mode::Window(None));
    };
    let mut rest = args[1..].iter();
    let mut value = |flag: &str| {
        rest.next()
            .cloned()
            .ok_or_else(|| format!("{flag} needs a value"))
    };
    let (kind, file) = match first.as_str() {
        "-h" | "--help" => return Ok(Mode::Help),
        "--probe" | "--edit" | "--render" => (first.as_str(), PathBuf::from(value(first)?)),
        flag if flag.starts_with('-') => return Err(format!("unknown option {flag}")),
        file => {
            if args.len() > 1 {
                return Err(format!("one file at a time, not {}", args[1]));
            }
            return Ok(Mode::Window(Some(PathBuf::from(file))));
        }
    };
    let (mut frame, mut pixel, mut hover, mut size) = (0usize, None, None, None);
    let (mut save, mut out, mut edits) = (None, None, Vec::new());
    while let Some(flag) = rest.next() {
        let mut value = || {
            rest.next()
                .cloned()
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        let allowed = match flag.as_str() {
            "--frame" => {
                let text = value()?;
                frame = text
                    .parse()
                    .map_err(|_| format!("--frame takes a number from 0, not {text:?}"))?;
                true
            }
            "--pixel" if kind == "--probe" => {
                pixel = Some(point(flag, &value()?)?);
                true
            }
            "--hover" if kind == "--render" => {
                hover = Some(point(flag, &value()?)?);
                true
            }
            "--size" if kind == "--render" => {
                let text = value()?;
                let (w, h) = text
                    .split_once('x')
                    .and_then(|(w, h)| Some((w.parse::<u32>().ok()?, h.parse::<u32>().ok()?)))
                    .filter(|(w, h)| (1..=16384).contains(w) && (1..=16384).contains(h))
                    .ok_or_else(|| format!("--size takes WxH, such as 1000x700, not {text:?}"))?;
                size = Some((w, h));
                true
            }
            "--out" if kind == "--render" => {
                out = Some(PathBuf::from(value()?));
                true
            }
            "--save" if kind == "--edit" => {
                save = Some(PathBuf::from(value()?));
                true
            }
            "--set-alpha" if kind == "--edit" => {
                let text = value()?;
                let parts: Vec<&str> = text.split(',').collect();
                let parsed = match parts.as_slice() {
                    [x, y, a] => x
                        .parse()
                        .ok()
                        .zip(y.parse().ok())
                        .zip(a.parse().ok())
                        .map(|((x, y), a)| Edit::SetAlpha(x, y, a)),
                    _ => None,
                };
                edits.push(parsed.ok_or_else(|| {
                    format!("--set-alpha takes X,Y,A with A from 0 to 255, not {text:?}")
                })?);
                true
            }
            "--premultiply" if kind == "--edit" => {
                edits.push(Edit::Premultiply);
                true
            }
            "--make-opaque" if kind == "--edit" => {
                edits.push(Edit::MakeOpaque);
                true
            }
            _ => false,
        };
        if !allowed {
            return Err(format!("{flag} does not go with {kind}"));
        }
    }
    Ok(match kind {
        "--probe" => Mode::Probe { file, frame, pixel },
        "--edit" => Mode::Edit {
            file,
            frame,
            edits,
            save: save.ok_or("--edit needs --save OUT")?,
        },
        _ => Mode::Render {
            file,
            frame,
            out: out.ok_or("--render needs --out PNG")?,
            size: size.ok_or("--render needs --size WxH")?,
            hover,
        },
    })
}

fn point(flag: &str, text: &str) -> Result<(u32, u32), String> {
    text.split_once(',')
        .and_then(|(x, y)| Some((x.trim().parse().ok()?, y.trim().parse().ok()?)))
        .ok_or_else(|| format!("{flag} takes X,Y, such as 10,10, not {text:?}"))
}

fn probe(file: PathBuf, frame: usize, pixel: Option<(u32, u32)>) -> anyhow::Result<()> {
    let opened = picture::open(&file, frame)?;
    let source = &opened.source;
    let mut report = format!(
        "format: {}\nsize: {}x{}\nframes: {}\n",
        source.format.name(),
        opened.image.width,
        opened.image.height,
        source.frames
    );
    if let Some((x, y)) = pixel {
        let [r, g, b, a] = picture::pixel(&opened.image, x, y).ok_or_else(|| {
            anyhow::anyhow!(
                "pixel {x},{y} is outside the {}x{} picture",
                opened.image.width,
                opened.image.height
            )
        })?;
        report.push_str(&format!("pixel: {x},{y} = {r:02x}{g:02x}{b:02x}{a:02x}\n"));
    }
    say(&report, false);
    Ok(())
}

fn edit(file: PathBuf, frame: usize, edits: &[Edit], save: PathBuf) -> anyhow::Result<()> {
    Format::for_saving(&save)?;
    let opened = picture::open(&file, frame)?;
    let mut image = opened.image;
    for edit in edits {
        match *edit {
            Edit::SetAlpha(x, y, alpha) => {
                picture::set_alpha(&mut image, x, y, alpha).ok_or_else(|| {
                    anyhow::anyhow!(
                        "pixel {x},{y} is outside the {}x{} picture",
                        image.width,
                        image.height
                    )
                })?;
            }
            Edit::Premultiply => picture::premultiply(&mut image),
            Edit::MakeOpaque => picture::make_opaque(&mut image),
        }
    }
    let saved = picture::save(&SaveRequest {
        out: save,
        source: opened.source,
        frame,
        edited: vec![(frame, image)],
    })?;
    say(
        &format!(
            "saved: {} ({}, {} {})\n",
            saved.path.display(),
            saved.format.name(),
            saved.frames,
            if saved.frames == 1 { "frame" } else { "frames" }
        ),
        false,
    );
    Ok(())
}

fn render_to(
    file: PathBuf,
    frame: usize,
    out: PathBuf,
    (width, height): (u32, u32),
    hover: Option<(u32, u32)>,
) -> anyhow::Result<()> {
    let opened = picture::open(&file, frame)?;
    let (image_width, image_height) = (opened.image.width, opened.image.height);
    // As on macOS: the menus are in the system menu bar, not the window.
    let mut editor = Editor::new(true);
    editor.show(opened);
    let surface = (width as f32, height as f32);
    let input = loadngo_host_core::InputSnapshot::default();
    let _ = editor.frame(&input, surface, 0.0, &render::approximate_width);
    if let Some((x, y)) = hover {
        if x >= image_width || y >= image_height {
            anyhow::bail!("--hover {x},{y} is outside the {image_width}x{image_height} picture");
        }
        editor.hover_pixel(x, y);
        let _ = editor.frame(&input, surface, 0.0, &render::approximate_width);
    }
    let scene = render::render(&editor, width, height);
    std::fs::write(&out, picture::encode(&scene, &Format::Png)?)
        .map_err(|error| anyhow::anyhow!("cannot write {}: {error}", out.display()))?;
    let mut report = format!("rendered: {} ({width}x{height})\n", out.display());
    if let Some(cell) = editor.hover_cell() {
        report.push_str(&format!(
            "hover_cell: {},{},{},{}\n",
            cell.x as i64, cell.y as i64, cell.width as i64, cell.height as i64
        ));
    }
    say(&report, false);
    Ok(())
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
/// the program was started from, if any (as the Task app does).
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

/// The window on `loadngo-host-desktop`: the editor model, its file work on
/// the host's offload workers (results come back as host proactor
/// completions and wake the frame), and nothing drawn until input or a
/// result arrives.
mod window {
    use std::path::PathBuf;
    use std::time::Duration;

    use bitmap_editor::editor::{Editor, JobResult, IMAGE_KEY};
    use loadngo_host_core::{FrameDemand, WindowDescriptor};
    use loadngo_host_desktop::Offloaded;

    pub fn open(file: Option<PathBuf>) {
        loadngo_host_desktop::launch(
            WindowDescriptor {
                title: "Bitmap Editor".to_string(),
                width: Some(1100),
                height: Some(760),
                high_dpi: true,
                linux_wm_class: Some("loadngo-bitmap-editor"),
            },
            None,
            run(file),
        );
    }

    async fn run(file: Option<PathBuf>) {
        // macOS shows the menus in the system menu bar; elsewhere the window
        // draws its own.
        let native = loadngo_host_desktop::set_menu_bar(&ui_core::MenuBar::default());
        let mut editor = Editor::new(native);
        if let Some(file) = file {
            editor.request_open(file);
        }
        let measure = |text: &str, size: u16| {
            loadngo_host_desktop::measure_text_metrics(text, None, size, 1.0).width
        };
        let mut running: Vec<Offloaded<JobResult>> = Vec::new();
        let mut uploaded = None;
        let mut announced = false;
        let mut scene = Vec::new();

        loop {
            let frame = loadngo_host_desktop::capture_frame();
            running.retain_mut(|job| match job.try_take() {
                Some(Ok(result)) => {
                    editor.apply(result);
                    false
                }
                Some(Err(failure)) => {
                    // perform() contains decoder panics; this is the pool's
                    // own failure.
                    loadngo_host_desktop::log_error(format_args!("file job failed: {failure}"));
                    false
                }
                None => true,
            });

            let outcome = editor.frame(
                &frame.input,
                (frame.surface.width, frame.surface.height),
                frame.timing.delta_seconds,
                &measure,
            );
            if let Some(menu_bar) = &outcome.menu_bar {
                loadngo_host_desktop::set_menu_bar(menu_bar);
            }
            for job in editor.take_jobs() {
                running.push(loadngo_host_desktop::offload(move || job.perform()));
            }
            if outcome.quit {
                break;
            }

            if let Some((image, revision)) = editor.image() {
                if uploaded != Some(revision) {
                    if let Err(error) =
                        loadngo_host_desktop::upload_texture_with_image_key(Some(IMAGE_KEY), image)
                    {
                        loadngo_host_desktop::log_error(format_args!(
                            "cannot show the frame: {error}"
                        ));
                    }
                    uploaded = Some(revision);
                }
            }
            scene.clear();
            editor.paint(&mut scene);
            loadngo_host_desktop::clear(ui_core::Color::rgba(0x1b, 0x1e, 0x25, 0xff));
            loadngo_host_desktop::render_widget_paint_ops(&scene);
            if !announced {
                if let Some((source, shown)) = editor.shown() {
                    eprintln!(
                        "bitmap_editor_loaded file={} format={} size={}x{} frames={} frame={shown}",
                        source.path.display(),
                        source.format.name(),
                        source.width,
                        source.height,
                        source.frames
                    );
                    announced = true;
                }
            }

            // Input and finished file work bring the next frame; only the
            // file dialog's scroll glide asks for frames by itself.
            let demand = if editor.animating() {
                FrameDemand::after(Duration::from_millis(16))
            } else {
                FrameDemand::idle()
            };
            loadngo_host_desktop::next_frame(demand).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{parse, Edit, Mode};
    use std::path::PathBuf;

    fn args(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn the_command_line_parses_every_mode_and_refuses_what_does_not_fit() {
        assert_eq!(parse(&[]).unwrap(), Mode::Window(None));
        assert_eq!(parse(&args("--help")).unwrap(), Mode::Help);
        assert_eq!(
            parse(&args("a.png")).unwrap(),
            Mode::Window(Some(PathBuf::from("a.png")))
        );
        assert_eq!(
            parse(&args("--probe c.mp4 --frame 5 --pixel 10,10")).unwrap(),
            Mode::Probe {
                file: PathBuf::from("c.mp4"),
                frame: 5,
                pixel: Some((10, 10)),
            }
        );
        assert_eq!(
            parse(&args(
                "--edit a.png --set-alpha 1,2,128 --premultiply --make-opaque --save b.png"
            ))
            .unwrap(),
            Mode::Edit {
                file: PathBuf::from("a.png"),
                frame: 0,
                edits: vec![
                    Edit::SetAlpha(1, 2, 128),
                    Edit::Premultiply,
                    Edit::MakeOpaque
                ],
                save: PathBuf::from("b.png"),
            }
        );
        assert_eq!(
            parse(&args(
                "--render a.png --out s.png --size 1000x700 --hover 3,4"
            ))
            .unwrap(),
            Mode::Render {
                file: PathBuf::from("a.png"),
                frame: 0,
                out: PathBuf::from("s.png"),
                size: (1000, 700),
                hover: Some((3, 4)),
            }
        );
        for bad in [
            "--probe",
            "--probe a.png --save b.png",
            "--edit a.png",
            "--edit a.png --set-alpha 1,2,300 --save b.png",
            "--render a.png --out s.png",
            "--render a.png --out s.png --size 0x5",
            "--frame 1",
            "a.png b.png",
        ] {
            assert!(parse(&args(bad)).is_err(), "{bad}");
        }
    }
}
