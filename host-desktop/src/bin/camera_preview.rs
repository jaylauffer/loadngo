#[cfg(not(target_os = "linux"))]
compile_error!("camera_preview currently supports Linux only");

use std::fs;
use std::io::{ErrorKind, Read};
use std::os::fd::{AsRawFd, RawFd};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::{ColorType, ImageEncoder};
use loadngo_host_core::{
    decode_image_from_memory, DecodedImage, FrameDemand, HostKey, RectF, WindowDescriptor,
};
use loadngo_proactor::{CompletionKind, EpollPort, Proactor, ProactorHandle, ReadinessEvent};
use ui_core::Color;

const WINDOW_WIDTH: i32 = 1320;
const WINDOW_HEIGHT: i32 = 920;
const PANEL_MARGIN: f32 = 24.0;
const BUTTON_HEIGHT: f32 = 56.0;
const BUTTON_GAP: f32 = 16.0;
const BUTTON_WIDTH: f32 = 180.0;
const PREVIEW_IMAGE_KEY: &str = "camera/live";
const CAMERA_STREAM_TOKEN: u64 = 0x4341_4d45_5241;
const DEFAULT_FRAME_RATE: u32 = 6;
const DEFAULT_VIDEO_SIZE: &str = "1280x720";
const DEFAULT_JPEG_QUALITY: u8 = 92;
const RESTART_BACKOFF: Duration = Duration::from_millis(800);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SaveFormat {
    Png,
    Jpeg,
}

impl SaveFormat {
    fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Png => "PNG",
            Self::Jpeg => "JPG",
        }
    }
}

#[derive(Clone, Debug)]
struct AppOptions {
    device: String,
    video_size: Option<String>,
    frame_rate: u32,
    output_dir: PathBuf,
    jpeg_quality: u8,
    once: bool,
    once_format: SaveFormat,
    once_output: Option<PathBuf>,
    list_devices: bool,
}

impl Default for AppOptions {
    fn default() -> Self {
        Self {
            device: default_camera_device(),
            video_size: Some(DEFAULT_VIDEO_SIZE.to_string()),
            frame_rate: DEFAULT_FRAME_RATE,
            output_dir: default_output_dir(),
            jpeg_quality: DEFAULT_JPEG_QUALITY,
            once: false,
            once_format: SaveFormat::Png,
            once_output: None,
            list_devices: false,
        }
    }
}

#[derive(Clone, Debug)]
struct CaptureOptions {
    device: String,
    video_size: Option<String>,
    frame_rate: u32,
}

#[derive(Debug)]
enum CaptureEvent {
    Frame(DecodedImage),
    Status(String),
    Error(String),
}

struct ActiveStream {
    child: Child,
    stdout: ChildStdout,
    stdout_fd: RawFd,
    stderr_text: Arc<Mutex<String>>,
    stderr_join: Option<JoinHandle<()>>,
    frame_width: u32,
    frame_height: u32,
    frame_len: usize,
    pending: Vec<u8>,
    delivered_frame: bool,
}

impl ActiveStream {
    fn kill(&mut self) {
        let _ = self.child.kill();
    }

    fn finish(mut self) -> StreamOutcome {
        let status_text = match self.child.wait() {
            Ok(status) => status.to_string(),
            Err(err) => format!("failed to wait for ffmpeg: {err}"),
        };
        if let Some(join) = self.stderr_join.take() {
            let _ = join.join();
        }
        let stderr_text = self
            .stderr_text
            .lock()
            .map(|text| text.clone())
            .unwrap_or_else(|_| String::new());
        StreamOutcome {
            status_text,
            stderr_text,
            delivered_frame: self.delivered_frame,
        }
    }
}

struct StreamOutcome {
    status_text: String,
    stderr_text: String,
    delivered_frame: bool,
}

struct CaptureController {
    options: CaptureOptions,
    sender: Sender<CaptureEvent>,
    handle: ProactorHandle<EpollPort>,
    running: AtomicBool,
    restart_pending: AtomicBool,
    active_stream: Mutex<Option<ActiveStream>>,
}

impl CaptureController {
    fn new(
        options: CaptureOptions,
        sender: Sender<CaptureEvent>,
        handle: ProactorHandle<EpollPort>,
    ) -> Self {
        Self {
            options,
            sender,
            handle,
            running: AtomicBool::new(true),
            restart_pending: AtomicBool::new(false),
            active_stream: Mutex::new(None),
        }
    }

    fn run(self: Arc<Self>, proactor: Proactor<EpollPort>) {
        if let Err(err) = self.start_stream() {
            self.schedule_restart(err);
        }

        while self.handle.is_running() {
            match proactor.run_once() {
                Ok(_) => {}
                Err(err) => {
                    let _ = self.sender.send(CaptureEvent::Error(format!(
                        "camera proactor failed: {err}"
                    )));
                    break;
                }
            }
        }

        self.running.store(false, Ordering::SeqCst);
        self.shutdown_active_stream(true);
    }

    fn start_stream(self: &Arc<Self>) -> Result<(), String> {
        if !self.running.load(Ordering::SeqCst) {
            return Ok(());
        }

        self.shutdown_active_stream(true);
        let _ = self.sender.send(CaptureEvent::Status(format!(
            "Starting camera stream on {}...",
            self.options.device
        )));
        loadngo_host_desktop::wake_host();

        let (frame_width, frame_height) = preview_frame_dimensions(&self.options)?;
        let frame_len = frame_width as usize * frame_height as usize * 4;
        let mut child = spawn_stream_process(&self.options)?;
        let stderr_text = Arc::new(Mutex::new(String::new()));
        let stderr_text_thread = Arc::clone(&stderr_text);
        let mut stderr = child
            .stderr
            .take()
            .ok_or_else(|| "failed to capture ffmpeg stderr".to_string())?;
        let stderr_join = thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = stderr.read_to_end(&mut bytes);
            if let Ok(mut sink) = stderr_text_thread.lock() {
                *sink = String::from_utf8_lossy(&bytes).trim().to_string();
            }
        });

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "failed to capture ffmpeg stdout".to_string())?;
        let stdout_fd = stdout.as_raw_fd();
        set_nonblocking(stdout_fd)?;

        {
            let mut slot = self
                .active_stream
                .lock()
                .map_err(|_| "camera stream lock poisoned".to_string())?;
            *slot = Some(ActiveStream {
                child,
                stdout,
                stdout_fd,
                stderr_text,
                stderr_join: Some(stderr_join),
                frame_width,
                frame_height,
                frame_len,
                pending: Vec::new(),
                delivered_frame: false,
            });
        }

        let controller = Arc::clone(self);
        if let Err(err) = self.handle.register_readable(
            stdout_fd,
            CAMERA_STREAM_TOKEN,
            move |_event: ReadinessEvent| {
                controller.on_stdout_ready();
            },
        ) {
            let _ = self.shutdown_active_stream(true);
            return Err(format!(
                "failed to register camera stream with epoll: {err}"
            ));
        }

        self.restart_pending.store(false, Ordering::SeqCst);
        Ok(())
    }

    fn on_stdout_ready(self: &Arc<Self>) {
        let mut frames = Vec::new();
        let mut restart_reason = None;

        {
            let mut slot = match self.active_stream.lock() {
                Ok(slot) => slot,
                Err(_) => {
                    let _ = self.sender.send(CaptureEvent::Error(
                        "camera stream lock poisoned".to_string(),
                    ));
                    loadngo_host_desktop::wake_host();
                    return;
                }
            };
            let Some(stream) = slot.as_mut() else {
                return;
            };

            let mut scratch = [0u8; 64 * 1024];
            loop {
                match stream.stdout.read(&mut scratch) {
                    Ok(0) => {
                        restart_reason = Some("camera stream ended unexpectedly".to_string());
                        break;
                    }
                    Ok(read) => {
                        stream.pending.extend_from_slice(&scratch[..read]);
                        while let Some(image) = extract_next_raw_rgba_frame(
                            &mut stream.pending,
                            stream.frame_width,
                            stream.frame_height,
                            stream.frame_len,
                        ) {
                            stream.delivered_frame = true;
                            frames.push(image);
                        }
                    }
                    Err(err) if err.kind() == ErrorKind::WouldBlock => break,
                    Err(err) if err.kind() == ErrorKind::Interrupted => continue,
                    Err(err) => {
                        restart_reason = Some(format!("failed reading ffmpeg output: {err}"));
                        break;
                    }
                }
            }

            if stream.pending.len() > 8 * 1024 * 1024 {
                stream.pending.clear();
            }
        }

        let delivered_frames = !frames.is_empty();
        for frame in frames {
            let _ = self.sender.send(CaptureEvent::Frame(frame));
        }
        if delivered_frames {
            loadngo_host_desktop::wake_host();
        }

        if let Some(reason) = restart_reason {
            self.finish_stream_and_retry(reason);
        }
    }

    fn finish_stream_and_retry(self: &Arc<Self>, fallback_reason: String) {
        let outcome = self.shutdown_active_stream(false);
        if !self.running.load(Ordering::SeqCst) {
            return;
        }

        let reason = if let Some(outcome) = outcome {
            if !outcome.stderr_text.is_empty() {
                outcome.stderr_text
            } else if !outcome.delivered_frame {
                "ffmpeg exited before delivering any camera frames".to_string()
            } else if outcome.status_text != "exit status: 0" {
                format!("{fallback_reason}; {}", outcome.status_text)
            } else {
                fallback_reason
            }
        } else {
            fallback_reason
        };
        self.schedule_restart(reason);
    }

    fn schedule_restart(self: &Arc<Self>, reason: String) {
        if !self.running.load(Ordering::SeqCst) {
            return;
        }

        let _ = self
            .sender
            .send(CaptureEvent::Error(format!("{reason}; retrying")));
        loadngo_host_desktop::wake_host();

        if self.restart_pending.swap(true, Ordering::SeqCst) {
            return;
        }

        let controller = Arc::clone(self);
        if let Err(err) = self
            .handle
            .defer_for(RESTART_BACKOFF, CompletionKind::Io, 0, move |_| {
                controller.restart_pending.store(false, Ordering::SeqCst);
                if !controller.running.load(Ordering::SeqCst) {
                    return;
                }
                if let Err(err) = controller.start_stream() {
                    controller.schedule_restart(err);
                }
            })
        {
            let _ = self.sender.send(CaptureEvent::Error(format!(
                "failed to schedule camera restart: {err}"
            )));
            let _ = self.handle.stop();
        }
    }

    fn shutdown_active_stream(&self, terminate: bool) -> Option<StreamOutcome> {
        let stream = {
            let mut slot = self.active_stream.lock().ok()?;
            if let Some(stream) = slot.as_ref() {
                let _ = self
                    .handle
                    .deregister_readable(stream.stdout_fd, CAMERA_STREAM_TOKEN);
            }
            slot.take()
        }?;

        let mut stream = stream;
        if terminate {
            stream.kill();
        }
        Some(stream.finish())
    }

    fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
        self.restart_pending.store(false, Ordering::SeqCst);
        self.shutdown_active_stream(true);
        loadngo_host_desktop::wake_host();
        let _ = self.handle.stop();
    }
}

struct CaptureWorker {
    controller: Arc<CaptureController>,
    receiver: Receiver<CaptureEvent>,
    join: Option<JoinHandle<()>>,
}

impl CaptureWorker {
    fn start(options: CaptureOptions) -> Result<Self, String> {
        let (tx, rx) = mpsc::channel();
        let proactor = Proactor::new(
            EpollPort::new().map_err(|err| format!("failed to create epoll port: {err}"))?,
        );
        let handle = proactor.handle();
        let controller = Arc::new(CaptureController::new(options, tx, handle));
        let controller_thread = Arc::clone(&controller);
        let join = thread::spawn(move || controller_thread.run(proactor));

        Ok(Self {
            controller,
            receiver: rx,
            join: Some(join),
        })
    }

    fn try_recv(&self) -> Result<CaptureEvent, mpsc::TryRecvError> {
        self.receiver.try_recv()
    }

    fn stop(&mut self) {
        self.controller.stop();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for CaptureWorker {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Clone)]
struct ButtonSpec {
    rect: ui_core::Rect,
    label: &'static str,
    tint: Color,
}

fn main() -> Result<(), String> {
    let options = parse_args()?;
    if options.list_devices {
        for (device, label) in available_camera_devices_with_labels() {
            println!("{device}\t{label}");
        }
        return Ok(());
    }

    if options.once {
        let image = capture_single_frame(&CaptureOptions {
            device: options.device.clone(),
            video_size: options.video_size.clone(),
            frame_rate: options.frame_rate,
        })?;
        let output = if let Some(path) = options.once_output.clone() {
            path
        } else {
            build_output_path(&options.output_dir, options.once_format)
        };
        let saved = save_image(&image, options.once_format, &output, options.jpeg_quality)?;
        println!(
            "Saved {} to {}",
            options.once_format.label(),
            saved.display()
        );
        return Ok(());
    }

    loadngo_host_desktop::launch(window_descriptor(), None, async move {
        run_preview(options).await;
    });
    Ok(())
}

fn preview_trace_enabled() -> bool {
    match std::env::var("LOADNGO_CAMERA_PREVIEW_TRACE") {
        Ok(value) => {
            let normalized = value.trim().to_ascii_lowercase();
            !matches!(normalized.as_str(), "" | "0" | "false" | "no" | "off")
        }
        Err(_) => false,
    }
}

fn parse_args() -> Result<AppOptions, String> {
    let mut options = AppOptions::default();
    let mut args = std::env::args();
    let program = args.next().unwrap_or_else(|| "camera_preview".to_string());

    let mut pending_flag: Option<String> = None;
    for arg in args {
        if let Some(flag) = pending_flag.take() {
            match flag.as_str() {
                "--device" => options.device = arg,
                "--video-size" => options.video_size = Some(arg),
                "--frame-rate" => {
                    options.frame_rate = arg
                        .parse::<u32>()
                        .map_err(|_| format!("invalid frame rate: {arg}"))?
                        .max(1);
                }
                "--output-dir" => options.output_dir = parse_path(&arg),
                "--output" => options.once_output = Some(parse_path(&arg)),
                "--format" => {
                    options.once_format = parse_format(&arg)?;
                }
                "--jpeg-quality" => {
                    options.jpeg_quality = arg
                        .parse::<u8>()
                        .map_err(|_| format!("invalid jpeg quality: {arg}"))?;
                }
                _ => return Err(format!("unknown option: {flag}")),
            }
            continue;
        }

        match arg.as_str() {
            "--help" | "-h" => {
                println!("{}", usage(&program));
                std::process::exit(0);
            }
            "--list-devices" => options.list_devices = true,
            "--capture-once" => options.once = true,
            "--device" | "--video-size" | "--frame-rate" | "--output-dir" | "--output"
            | "--format" | "--jpeg-quality" => pending_flag = Some(arg),
            _ => return Err(format!("unknown argument: {arg}\n\n{}", usage(&program))),
        }
    }

    if let Some(flag) = pending_flag {
        return Err(format!("missing value for {flag}"));
    }

    if options.jpeg_quality == 0 {
        options.jpeg_quality = 1;
    }

    Ok(options)
}

fn usage(program: &str) -> String {
    format!(
        "Usage: {program} [--device PATH] [--video-size WxH] [--frame-rate FPS] [--output-dir DIR]\n\
         \n\
         Default mode opens a loadngo preview window with Save PNG / Save JPG controls.\n\
         \n\
         Options:\n\
           --list-devices          list /dev/video* devices and exit\n\
           --capture-once          grab one frame and save it without opening the preview window\n\
           --device PATH           camera device path (default: /dev/video0)\n\
           --video-size WxH        requested capture size (default: {DEFAULT_VIDEO_SIZE})\n\
           --frame-rate FPS        requested capture rate (default: {DEFAULT_FRAME_RATE})\n\
           --output-dir DIR        save directory for GUI captures (default: ~/Pictures or cwd)\n\
           --output PATH           explicit file path for --capture-once\n\
           --format png|jpg        file format for --capture-once (default: png)\n\
           --jpeg-quality 1-100    JPEG save quality (default: {DEFAULT_JPEG_QUALITY})\n\
         \n\
         Preview controls:\n\
           Save PNG button         save lossless frame to the output directory\n\
           Save JPG button         save JPEG frame to the output directory\n\
           Restart Stream          restart the proactor-driven ffmpeg capture path\n\
           Esc                     quit\n\
           S                       save PNG\n\
           R                       restart capture\n"
    )
}

fn window_descriptor() -> WindowDescriptor {
    WindowDescriptor {
        title: "loadngo camera preview".to_string(),
        width: Some(WINDOW_WIDTH),
        height: Some(WINDOW_HEIGHT),
        high_dpi: true,
        linux_wm_class: Some("loadngo-camera-preview"),
    }
}

async fn run_preview(options: AppOptions) {
    let capture_options = CaptureOptions {
        device: options.device.clone(),
        video_size: options.video_size.clone(),
        frame_rate: options.frame_rate,
    };
    let mut worker = match CaptureWorker::start(capture_options.clone()) {
        Ok(worker) => Some(worker),
        Err(err) => None.or_else(|| {
            eprintln!("camera_preview startup error: {err}");
            None
        }),
    };
    let mut current_image: Option<DecodedImage> = None;
    let mut current_texture: Option<loadngo_host_desktop::DesktopTexture> = None;
    let mut status = if worker.is_some() {
        format!("Connecting to {}...", capture_options.device)
    } else {
        format!(
            "Capture startup failed for {}. Press Restart Stream after fixing the camera path.",
            capture_options.device
        )
    };
    let mut last_saved: Option<PathBuf> = None;
    let trace_enabled = preview_trace_enabled();
    let mut trace_started = Instant::now();
    let mut trace_frames_since_log: u64 = 0;
    let mut trace_total_frames: u64 = 0;

    loop {
        let frame = loadngo_host_desktop::capture_frame();
        if frame.input.key_pressed(HostKey::Escape) {
            break;
        }

        if let Some(active_worker) = worker.as_ref() {
            while let Ok(event) = active_worker.try_recv() {
                match event {
                    CaptureEvent::Frame(image) => {
                        trace_total_frames = trace_total_frames.saturating_add(1);
                        trace_frames_since_log = trace_frames_since_log.saturating_add(1);
                        if trace_enabled && trace_total_frames == 1 {
                            eprintln!(
                                "[camera_preview] first live frame {}x{} from {}",
                                image.width, image.height, capture_options.device
                            );
                        }
                        if trace_enabled
                            && trace_started.elapsed() >= Duration::from_secs(1)
                            && trace_frames_since_log > 0
                        {
                            let elapsed = trace_started.elapsed().as_secs_f32().max(0.001);
                            let fps = trace_frames_since_log as f32 / elapsed;
                            eprintln!(
                                "[camera_preview] live preview receiving {:.2} fps ({} frames / {:.2}s)",
                                fps, trace_frames_since_log, elapsed
                            );
                            trace_started = Instant::now();
                            trace_frames_since_log = 0;
                        }
                        match loadngo_host_desktop::upload_texture_with_image_key(
                            Some(PREVIEW_IMAGE_KEY),
                            &image,
                        ) {
                            Ok(texture) => {
                                current_texture = Some(texture);
                                current_image = Some(image.clone());
                                status = format!(
                                    "Live preview: {}x{} from {} via loadngo proactor",
                                    image.width, image.height, capture_options.device
                                );
                            }
                            Err(err) => {
                                status = format!("Texture upload failed: {err}");
                            }
                        }
                    }
                    CaptureEvent::Status(message) => status = message,
                    CaptureEvent::Error(message) => status = format!("Capture error: {message}"),
                }
            }
        }

        let layout = build_layout(frame.surface.width, frame.surface.height);
        let save_png_clicked = pointer_pressed_in_rect(&frame.input, layout.save_png.rect);
        let save_jpg_clicked = pointer_pressed_in_rect(&frame.input, layout.save_jpg.rect);
        let restart_clicked = pointer_pressed_in_rect(&frame.input, layout.restart.rect);
        let restart_requested = frame.input.key_pressed(HostKey::R) || restart_clicked;

        if frame.input.key_pressed(HostKey::S) || save_png_clicked {
            if let Some(image) = current_image.as_ref() {
                match save_image(
                    image,
                    SaveFormat::Png,
                    &build_output_path(&options.output_dir, SaveFormat::Png),
                    options.jpeg_quality,
                ) {
                    Ok(path) => {
                        last_saved = Some(path.clone());
                        status = format!("Saved PNG to {}", path.display());
                    }
                    Err(err) => status = format!("Save failed: {err}"),
                }
            } else {
                status = "No camera frame available to save yet".to_string();
            }
        }

        if save_jpg_clicked {
            if let Some(image) = current_image.as_ref() {
                match save_image(
                    image,
                    SaveFormat::Jpeg,
                    &build_output_path(&options.output_dir, SaveFormat::Jpeg),
                    options.jpeg_quality,
                ) {
                    Ok(path) => {
                        last_saved = Some(path.clone());
                        status = format!("Saved JPG to {}", path.display());
                    }
                    Err(err) => status = format!("Save failed: {err}"),
                }
            } else {
                status = "No camera frame available to save yet".to_string();
            }
        }

        if restart_requested {
            if let Some(mut active_worker) = worker.take() {
                active_worker.stop();
            }
            match CaptureWorker::start(capture_options.clone()) {
                Ok(new_worker) => {
                    worker = Some(new_worker);
                    status = format!(
                        "Restarting stream on {} via loadngo proactor...",
                        capture_options.device
                    );
                }
                Err(err) => {
                    status = format!("Restart failed: {err}");
                }
            }
        }

        draw_scene(
            &capture_options,
            &current_texture,
            &status,
            last_saved.as_deref(),
            &layout,
            &frame,
        );

        loadngo_host_desktop::next_frame(FrameDemand::after(Duration::from_millis(16))).await;
    }

    if let Some(mut active_worker) = worker {
        active_worker.stop();
    }
}

struct Layout {
    preview_panel: ui_core::Rect,
    toolbar_panel: ui_core::Rect,
    save_png: ButtonSpec,
    save_jpg: ButtonSpec,
    restart: ButtonSpec,
}

fn build_layout(width: f32, height: f32) -> Layout {
    let preview_height = (height - PANEL_MARGIN * 3.0 - BUTTON_HEIGHT - 140.0).max(240.0);
    let preview_panel = ui_core::Rect {
        x: PANEL_MARGIN,
        y: PANEL_MARGIN + 68.0,
        width: (width - PANEL_MARGIN * 2.0).max(320.0),
        height: preview_height,
    };
    let toolbar_panel = ui_core::Rect {
        x: PANEL_MARGIN,
        y: preview_panel.y + preview_panel.height + PANEL_MARGIN,
        width: preview_panel.width,
        height: BUTTON_HEIGHT + 32.0,
    };

    let base_x = toolbar_panel.x + 16.0;
    let base_y = toolbar_panel.y + 16.0;
    let save_png = ButtonSpec {
        rect: ui_core::Rect {
            x: base_x,
            y: base_y,
            width: BUTTON_WIDTH,
            height: BUTTON_HEIGHT,
        },
        label: "Save PNG",
        tint: Color::rgba(0x2d, 0x8f, 0x63, 0xff),
    };
    let save_jpg = ButtonSpec {
        rect: ui_core::Rect {
            x: base_x + BUTTON_WIDTH + BUTTON_GAP,
            y: base_y,
            width: BUTTON_WIDTH,
            height: BUTTON_HEIGHT,
        },
        label: "Save JPG",
        tint: Color::rgba(0x8f, 0x5b, 0x2d, 0xff),
    };
    let restart = ButtonSpec {
        rect: ui_core::Rect {
            x: base_x + (BUTTON_WIDTH + BUTTON_GAP) * 2.0,
            y: base_y,
            width: BUTTON_WIDTH + 28.0,
            height: BUTTON_HEIGHT,
        },
        label: "Restart Stream",
        tint: Color::rgba(0x2d, 0x5c, 0x8f, 0xff),
    };

    Layout {
        preview_panel,
        toolbar_panel,
        save_png,
        save_jpg,
        restart,
    }
}

fn draw_scene(
    capture_options: &CaptureOptions,
    current_texture: &Option<loadngo_host_desktop::DesktopTexture>,
    status: &str,
    last_saved: Option<&Path>,
    layout: &Layout,
    frame: &loadngo_host_core::HostFrame,
) {
    loadngo_host_desktop::clear(Color::rgba(0x10, 0x14, 0x1c, 0xff));

    loadngo_host_desktop::draw_text(
        "loadngo camera preview",
        PANEL_MARGIN,
        PANEL_MARGIN,
        34.0,
        Color::rgba(0xf4, 0xf7, 0xfb, 0xff),
    );
    loadngo_host_desktop::draw_text(
        &format!(
            "device {}   requested {} @ {} fps",
            capture_options.device,
            capture_options
                .video_size
                .as_deref()
                .unwrap_or("camera default"),
            capture_options.frame_rate
        ),
        PANEL_MARGIN,
        PANEL_MARGIN + 34.0,
        20.0,
        Color::rgba(0xb9, 0xc7, 0xda, 0xff),
    );

    draw_panel(layout.preview_panel, Color::rgba(0x17, 0x1d, 0x27, 0xff));
    if let Some(texture) = current_texture {
        let fit = fit_rect(layout.preview_panel, texture.width(), texture.height());
        loadngo_host_desktop::draw_texture_fit(texture, fit.x, fit.y, fit.width, fit.height);
        loadngo_host_desktop::draw_rectangle_lines(
            fit.x,
            fit.y,
            fit.width,
            fit.height,
            2.0,
            Color::rgba(0x7f, 0x94, 0xaf, 0xff),
        );
    } else {
        draw_centered_text(
            layout.preview_panel,
            "Waiting for camera frames...",
            26.0,
            Color::rgba(0xd9, 0xe5, 0xf6, 0xff),
        );
    }

    draw_panel(layout.toolbar_panel, Color::rgba(0x17, 0x1d, 0x27, 0xff));
    draw_button(&layout.save_png, frame);
    draw_button(&layout.save_jpg, frame);
    draw_button(&layout.restart, frame);

    let status_y = layout.toolbar_panel.y + layout.toolbar_panel.height + 24.0;
    loadngo_host_desktop::draw_text(
        status,
        PANEL_MARGIN,
        status_y,
        20.0,
        Color::rgba(0xec, 0xf2, 0xff, 0xff),
    );

    if let Some(path) = last_saved {
        loadngo_host_desktop::draw_text(
            &format!("last saved: {}", path.display()),
            PANEL_MARGIN,
            status_y + 28.0,
            18.0,
            Color::rgba(0x9f, 0xd0, 0xb3, 0xff),
        );
    } else {
        loadngo_host_desktop::draw_text(
            "PNG is the default lossless save path. JPG remains available when file size matters.",
            PANEL_MARGIN,
            status_y + 28.0,
            18.0,
            Color::rgba(0x9f, 0xb0, 0xc8, 0xff),
        );
    }
}

fn draw_panel(rect: ui_core::Rect, fill: Color) {
    loadngo_host_desktop::draw_rectangle(rect.x, rect.y, rect.width, rect.height, fill);
    loadngo_host_desktop::draw_rectangle_lines(
        rect.x,
        rect.y,
        rect.width,
        rect.height,
        2.0,
        Color::rgba(0x3f, 0x4c, 0x60, 0xff),
    );
}

fn draw_button(button: &ButtonSpec, frame: &loadngo_host_core::HostFrame) {
    let hovered = pointer_in_rect(&frame.input, button.rect);
    let fill = if hovered {
        brighten(button.tint, 24)
    } else {
        button.tint
    };
    loadngo_host_desktop::draw_rectangle(
        button.rect.x,
        button.rect.y,
        button.rect.width,
        button.rect.height,
        fill,
    );
    loadngo_host_desktop::draw_rectangle_lines(
        button.rect.x,
        button.rect.y,
        button.rect.width,
        button.rect.height,
        2.0,
        Color::rgba(0xdf, 0xe8, 0xf4, 0xff),
    );
    draw_centered_text(
        button.rect,
        button.label,
        22.0,
        Color::rgba(0xf7, 0xfa, 0xfd, 0xff),
    );
}

fn draw_centered_text(rect: ui_core::Rect, text: &str, size: f32, color: Color) {
    let metrics = loadngo_host_desktop::measure_text(text, None, size.round() as u16, 1.0);
    let x = rect.x + (rect.width - metrics.width).max(0.0) * 0.5;
    let y = rect.y + (rect.height - metrics.height).max(0.0) * 0.5;
    loadngo_host_desktop::draw_text(text, x, y, size, color);
}

fn brighten(color: Color, delta: u8) -> Color {
    Color::rgba(
        color.r.saturating_add(delta),
        color.g.saturating_add(delta),
        color.b.saturating_add(delta),
        color.a,
    )
}

fn fit_rect(panel: ui_core::Rect, image_width: f32, image_height: f32) -> ui_core::Rect {
    if image_width <= 0.0 || image_height <= 0.0 {
        return panel;
    }
    let panel_ratio = panel.width / panel.height.max(1.0);
    let image_ratio = image_width / image_height.max(1.0);
    if image_ratio > panel_ratio {
        let width = panel.width;
        let height = width / image_ratio;
        ui_core::Rect {
            x: panel.x,
            y: panel.y + (panel.height - height) * 0.5,
            width,
            height,
        }
    } else {
        let height = panel.height;
        let width = height * image_ratio;
        ui_core::Rect {
            x: panel.x + (panel.width - width) * 0.5,
            y: panel.y,
            width,
            height,
        }
    }
}

fn pointer_in_rect(input: &loadngo_host_core::InputSnapshot, rect: ui_core::Rect) -> bool {
    input.pointer_in_rect(RectF {
        x: rect.x,
        y: rect.y,
        width: rect.width,
        height: rect.height,
    })
}

fn pointer_pressed_in_rect(input: &loadngo_host_core::InputSnapshot, rect: ui_core::Rect) -> bool {
    input.pointer_pressed_in_rect(RectF {
        x: rect.x,
        y: rect.y,
        width: rect.width,
        height: rect.height,
    })
}

fn spawn_stream_process(options: &CaptureOptions) -> Result<Child, String> {
    let (frame_width, frame_height) = preview_frame_dimensions(options)?;
    let mut command = base_capture_command(options);
    let filter = preview_filter_spec(options, frame_width, frame_height);
    command.args([
        "-an", "-vf", &filter, "-pix_fmt", "rgba", "-f", "rawvideo", "pipe:1",
    ]);
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    command
        .spawn()
        .map_err(|err| format!("failed to start ffmpeg: {err}"))
}

fn base_capture_command(options: &CaptureOptions) -> Command {
    let mut command = Command::new("ffmpeg");
    command.args(["-nostdin", "-hide_banner", "-loglevel", "error"]);
    command.args(["-fflags", "nobuffer"]);
    command.args(["-f", "v4l2"]);
    command.args(["-framerate", &options.frame_rate.max(1).to_string()]);
    if let Some(video_size) = options
        .video_size
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        command.args(["-video_size", video_size]);
    }
    command.args(["-i", &options.device]);
    command
}

fn set_nonblocking(fd: RawFd) -> Result<(), String> {
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags < 0 {
            return Err(format!(
                "failed to query camera stream flags: {}",
                std::io::Error::last_os_error()
            ));
        }
        if libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return Err(format!(
                "failed to mark camera stream nonblocking: {}",
                std::io::Error::last_os_error()
            ));
        }
    }
    Ok(())
}

fn preview_frame_dimensions(options: &CaptureOptions) -> Result<(u32, u32), String> {
    let video_size = options
        .video_size
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(DEFAULT_VIDEO_SIZE);
    parse_video_size_spec(video_size)
}

fn preview_filter_spec(options: &CaptureOptions, frame_width: u32, frame_height: u32) -> String {
    format!(
        "fps={},scale={}x{}",
        options.frame_rate.max(1),
        frame_width,
        frame_height
    )
}

fn parse_video_size_spec(value: &str) -> Result<(u32, u32), String> {
    let (width, height) = value
        .trim()
        .split_once('x')
        .ok_or_else(|| format!("invalid video size: {value}"))?;
    let width = width
        .parse::<u32>()
        .map_err(|_| format!("invalid video width: {value}"))?;
    let height = height
        .parse::<u32>()
        .map_err(|_| format!("invalid video height: {value}"))?;
    if width == 0 || height == 0 {
        return Err(format!("video size must be positive: {value}"));
    }
    Ok((width, height))
}

fn extract_next_raw_rgba_frame(
    buffer: &mut Vec<u8>,
    width: u32,
    height: u32,
    frame_len: usize,
) -> Option<DecodedImage> {
    if buffer.len() < frame_len {
        return None;
    }
    let tail = buffer.split_off(frame_len);
    let frame = std::mem::replace(buffer, tail);
    Some(DecodedImage::new(width, height, frame))
}

fn parse_format(value: &str) -> Result<SaveFormat, String> {
    match value.to_ascii_lowercase().as_str() {
        "png" => Ok(SaveFormat::Png),
        "jpg" | "jpeg" => Ok(SaveFormat::Jpeg),
        other => Err(format!("unsupported format: {other}")),
    }
}

fn capture_single_frame(options: &CaptureOptions) -> Result<DecodedImage, String> {
    let mut command = base_capture_command(options);
    command.args([
        "-frames:v",
        "1",
        "-f",
        "image2pipe",
        "-vcodec",
        "mjpeg",
        "pipe:1",
    ]);

    let output = command
        .output()
        .map_err(|err| format!("failed to execute ffmpeg: {err}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            "ffmpeg capture failed".to_string()
        } else {
            stderr
        });
    }
    decode_image_from_memory(&output.stdout)
}

fn save_image(
    image: &DecodedImage,
    format: SaveFormat,
    path: &Path,
    jpeg_quality: u8,
) -> Result<PathBuf, String> {
    image.validate_rgba8()?;
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .map_err(|err| format!("failed to create {}: {err}", parent.display()))?;
        }
    }

    let mut bytes = Vec::new();
    match format {
        SaveFormat::Png => {
            let encoder = PngEncoder::new(&mut bytes);
            encoder
                .write_image(
                    &image.rgba8,
                    image.width,
                    image.height,
                    ColorType::Rgba8.into(),
                )
                .map_err(|err| format!("png encoding failed: {err}"))?;
        }
        SaveFormat::Jpeg => {
            let rgb = rgba_to_rgb(&image.rgba8);
            let mut encoder = JpegEncoder::new_with_quality(&mut bytes, jpeg_quality.max(1));
            encoder
                .encode(&rgb, image.width, image.height, ColorType::Rgb8.into())
                .map_err(|err| format!("jpeg encoding failed: {err}"))?;
        }
    }

    fs::write(path, bytes).map_err(|err| format!("failed to write {}: {err}", path.display()))?;
    Ok(path.to_path_buf())
}

fn rgba_to_rgb(rgba: &[u8]) -> Vec<u8> {
    let mut rgb = Vec::with_capacity((rgba.len() / 4) * 3);
    for chunk in rgba.chunks_exact(4) {
        rgb.extend_from_slice(&chunk[..3]);
    }
    rgb
}

fn build_output_path(output_dir: &Path, format: SaveFormat) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    output_dir.join(format!("camera-{stamp}.{}", format.extension()))
}

fn default_output_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        let pictures = PathBuf::from(home).join("Pictures");
        if pictures.is_dir() {
            return pictures;
        }
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn parse_path(value: &str) -> PathBuf {
    if let Some(stripped) = value.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(stripped);
        }
    }
    PathBuf::from(value)
}

fn default_camera_device() -> String {
    available_camera_devices_with_labels()
        .into_iter()
        .map(|(device, _)| device)
        .next()
        .unwrap_or_else(|| "/dev/video0".to_string())
}

fn available_camera_devices_with_labels() -> Vec<(String, String)> {
    let mut preferred = Vec::new();
    let mut fallback = Vec::new();
    if let Ok(entries) = fs::read_dir("/dev") {
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                if name.starts_with("video") {
                    let device = format!("/dev/{name}");
                    let label = camera_label_for(name);
                    if is_probably_camera_device(&label) {
                        preferred.push((device, label));
                    } else {
                        fallback.push((device, label));
                    }
                }
            }
        }
    }
    preferred.sort();
    preferred.dedup();
    fallback.sort();
    fallback.dedup();
    if preferred.is_empty() {
        fallback
    } else {
        preferred
    }
}

fn camera_label_for(name: &str) -> String {
    let sysfs = PathBuf::from("/sys/class/video4linux")
        .join(name)
        .join("name");
    fs::read_to_string(sysfs)
        .map(|label| label.trim().to_string())
        .unwrap_or_else(|_| name.to_string())
}

fn is_probably_camera_device(label: &str) -> bool {
    let normalized = label.to_ascii_lowercase();
    !(normalized.contains("codec")
        || normalized.contains("isp")
        || normalized.contains("decoder")
        || normalized.contains("-dec")
        || normalized.contains("encoder")
        || normalized.contains("hevc")
        || normalized.contains("v4l2 loopback")
        || normalized.contains("bcm2835"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_video_size_spec_accepts_dimensions() {
        assert_eq!(parse_video_size_spec("1280x720").unwrap(), (1280, 720));
    }

    #[test]
    fn preview_filter_spec_enforces_requested_frame_rate() {
        let options = CaptureOptions {
            device: "/dev/video0".to_string(),
            video_size: Some("1280x720".to_string()),
            frame_rate: 6,
        };
        assert_eq!(preview_filter_spec(&options, 1280, 720), "fps=6,scale=1280x720");
    }

    #[test]
    fn extract_next_raw_rgba_frame_waits_for_complete_frame() {
        let mut buffer = vec![1u8; 15];
        assert!(extract_next_raw_rgba_frame(&mut buffer, 2, 2, 16).is_none());
        assert_eq!(buffer.len(), 15);
    }

    #[test]
    fn extract_next_raw_rgba_frame_returns_one_frame_and_keeps_tail() {
        let mut buffer = vec![7u8; 20];
        let frame = extract_next_raw_rgba_frame(&mut buffer, 2, 2, 16).unwrap();
        assert_eq!(frame.width, 2);
        assert_eq!(frame.height, 2);
        assert_eq!(frame.rgba8.len(), 16);
        assert_eq!(buffer, vec![7u8; 4]);
    }
}
