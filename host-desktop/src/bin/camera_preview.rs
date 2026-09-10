//! Desktop-only: the preview drives `loadngo-proactor` and `wake_host`
//! directly, neither of which the iOS or Android hosts expose. Gated the way
//! `text_metrics_harness.rs` and the `netbsd_*` bins are, so a
//! `cargo check -p loadngo-host-desktop --target aarch64-apple-ios` stays
//! green (see AGENTS.md's cross-platform build rule).

#[cfg(any(target_os = "macos", target_os = "linux", windows))]
mod harness {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::sync::{Arc, Mutex};
    use std::thread::{self, JoinHandle};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use image::codecs::jpeg::JpegEncoder;
    use image::codecs::png::PngEncoder;
    use image::{ColorType, ImageEncoder};
    use loadngo_camera::{CameraDevice, CaptureConfig, CaptureStream, StreamOutcome};
    use loadngo_host_core::{
        decode_image_from_memory, DecodedImage, FrameDemand, HostKey, RectF, WindowDescriptor,
    };
    use loadngo_proactor::{CompletionKind, PlatformPort, Proactor, ProactorHandle};
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

    #[derive(Debug)]
    enum CaptureEvent {
        Frame(DecodedImage),
        Status(String),
        Error(String),
    }

    /// Connects a running [`CaptureStream`] to the proactor.
    ///
    /// This is the only part of the capture path that is still
    /// platform-shaped, and it is shaped by the proactor, not by the camera:
    /// `ReadinessPort` is `RawFd`-based, so Unix can hand the pipe straight
    /// to `io_uring`/`kqueue`/`epoll`, while IOCP has no readiness model for
    /// an anonymous pipe and needs a reader thread posting work back into the
    /// loop instead. Frames reach `on_stream_ready` on the proactor thread
    /// either way, so nothing above here has to care.
    mod transport {
        use super::{PlatformPort, ProactorHandle, CAMERA_STREAM_TOKEN};
        use loadngo_camera::CaptureStream;

        #[cfg(unix)]
        pub(super) struct Attachment {
            fd: std::os::fd::RawFd,
        }

        /// Registers the capture pipe for readability. The pipe is
        /// non-blocking (`CaptureStream::start` sets that up), so `on_ready`
        /// can drain it without ever blocking the proactor thread.
        #[cfg(unix)]
        pub(super) fn attach(
            handle: &ProactorHandle<PlatformPort>,
            stream: &CaptureStream,
            on_ready: impl Fn() + Send + 'static,
        ) -> Result<Attachment, String> {
            let fd = stream.stdout_fd();
            handle
                .register_readable(fd, CAMERA_STREAM_TOKEN, move |_event| on_ready())
                .map_err(|err| format!("failed to register camera stream readiness: {err}"))?;
            Ok(Attachment { fd })
        }

        #[cfg(unix)]
        pub(super) fn detach(handle: &ProactorHandle<PlatformPort>, attachment: Attachment) {
            let _ = handle.deregister_readable(attachment.fd, CAMERA_STREAM_TOKEN);
        }

        #[cfg(windows)]
        pub(super) struct Attachment;

        /// Designed, not implemented -- see `docs/CAMERA_PREVIEW.md`.
        ///
        /// IOCP cannot report readiness for the anonymous pipe `ffmpeg`
        /// writes to, so the Windows path is a dedicated reader thread
        /// calling `CaptureStream::pump` (which returns after one blocking
        /// read on Windows) and handing each batch back with
        /// `ProactorHandle::enqueue_work`, so frames still arrive on the
        /// proactor thread. What blocks it today is ownership: the reader
        /// thread must own the stream to block on it, while shutdown must be
        /// able to kill the child without waiting for that read to return, so
        /// `CaptureStream` needs to split into a reader half and a control
        /// half first. Returning an error here is deliberate -- there is no
        /// Windows machine to compile or test against, and a plausible-looking
        /// deadlock would be worse than an honest refusal.
        #[cfg(windows)]
        pub(super) fn attach(
            _handle: &ProactorHandle<PlatformPort>,
            _stream: &CaptureStream,
            _on_ready: impl Fn() + Send + 'static,
        ) -> Result<Attachment, String> {
            Err("camera preview needs the Windows reader-thread transport, \
                 which is designed but not implemented (see docs/CAMERA_PREVIEW.md)"
                .to_string())
        }

        #[cfg(windows)]
        pub(super) fn detach(_handle: &ProactorHandle<PlatformPort>, _attachment: Attachment) {}
    }

    struct CaptureController {
        options: CaptureConfig,
        sender: Sender<CaptureEvent>,
        handle: ProactorHandle<PlatformPort>,
        running: AtomicBool,
        restart_pending: AtomicBool,
        active_stream: Mutex<Option<CaptureStream>>,
        attachment: Mutex<Option<transport::Attachment>>,
    }

    impl CaptureController {
        fn new(
            options: CaptureConfig,
            sender: Sender<CaptureEvent>,
            handle: ProactorHandle<PlatformPort>,
        ) -> Self {
            Self {
                options,
                sender,
                handle,
                running: AtomicBool::new(true),
                restart_pending: AtomicBool::new(false),
                active_stream: Mutex::new(None),
                attachment: Mutex::new(None),
            }
        }

        fn run(self: Arc<Self>, proactor: Proactor<PlatformPort>) {
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

            let stream = CaptureStream::start(&self.options).map_err(|err| err.to_string())?;

            let controller = Arc::clone(self);
            let attachment = transport::attach(&self.handle, &stream, move || {
                controller.on_stream_ready();
            })?;

            {
                let mut slot = self
                    .active_stream
                    .lock()
                    .map_err(|_| "camera stream lock poisoned".to_string())?;
                *slot = Some(stream);
            }
            {
                let mut slot = self
                    .attachment
                    .lock()
                    .map_err(|_| "camera attachment lock poisoned".to_string())?;
                *slot = Some(attachment);
            }

            self.restart_pending.store(false, Ordering::SeqCst);
            Ok(())
        }

        /// Drains whatever the capture pipe has. Called from readiness on
        /// Unix and from the reader thread's posted work on Windows -- either
        /// way, on the proactor thread.
        fn on_stream_ready(self: &Arc<Self>) {
            let (frames, restart_reason) = {
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
                let (frame_width, frame_height) = stream.frame_dimensions();
                let result = stream.pump();
                let frames = result
                    .frames
                    .into_iter()
                    .map(|bytes| DecodedImage::new(frame_width, frame_height, bytes))
                    .collect::<Vec<_>>();
                (frames, result.ended)
            };

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
            if let Err(err) =
                self.handle
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
            if let Ok(mut slot) = self.attachment.lock() {
                if let Some(attachment) = slot.take() {
                    transport::detach(&self.handle, attachment);
                }
            }
            let stream = {
                let mut slot = self.active_stream.lock().ok()?;
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
        fn start(options: CaptureConfig) -> Result<Self, String> {
            let (tx, rx) = mpsc::channel();
            // Whatever completion port this platform actually uses --
            // io_uring on Linux, kqueue on macOS -- rather than naming one.
            let proactor = loadngo_proactor::new_platform_proactor()
                .map_err(|err| format!("failed to create proactor: {err}"))?;
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

    fn camera_devices() -> Vec<CameraDevice> {
        loadngo_camera::list_devices().unwrap_or_default()
    }

    /// Device to use when `--device` is not given.
    fn default_camera_device() -> String {
        camera_devices()
            .into_iter()
            .next()
            .map(|device| device.id)
            .unwrap_or_default()
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
        let capture_options = CaptureConfig {
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
                        CaptureEvent::Error(message) => {
                            status = format!("Capture error: {message}")
                        }
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
        capture_options: &CaptureConfig,
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

    fn pointer_pressed_in_rect(
        input: &loadngo_host_core::InputSnapshot,
        rect: ui_core::Rect,
    ) -> bool {
        input.pointer_pressed_in_rect(RectF {
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
        })
    }

    /// One frame, decoded. MJPEG keeps the pipe small; the preview stream
    /// uses raw RGBA instead because it slices fixed-size frames.
    fn capture_single_frame(config: &CaptureConfig) -> Result<DecodedImage, String> {
        let bytes =
            loadngo_camera::capture_single_frame(config, "mjpeg").map_err(|err| err.to_string())?;
        decode_image_from_memory(&bytes)
    }

    fn parse_format(value: &str) -> Result<SaveFormat, String> {
        match value.to_ascii_lowercase().as_str() {
            "png" => Ok(SaveFormat::Png),
            "jpg" | "jpeg" => Ok(SaveFormat::Jpeg),
            other => Err(format!("unsupported format: {other}")),
        }
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
                    .write_image(&image.rgba8, image.width, image.height, ColorType::Rgba8)
                    .map_err(|err| format!("png encoding failed: {err}"))?;
            }
            SaveFormat::Jpeg => {
                let rgb = rgba_to_rgb(&image.rgba8);
                let mut encoder = JpegEncoder::new_with_quality(&mut bytes, jpeg_quality.max(1));
                encoder
                    .encode(&rgb, image.width, image.height, ColorType::Rgb8)
                    .map_err(|err| format!("jpeg encoding failed: {err}"))?;
            }
        }

        fs::write(path, bytes)
            .map_err(|err| format!("failed to write {}: {err}", path.display()))?;
        Ok(path.to_path_buf())
    }

    fn rgba_to_rgb(rgba: &[u8]) -> Vec<u8> {
        let mut rgb = Vec::with_capacity((rgba.len() / 4) * 3);
        let (chunks, _remainder) = rgba.as_chunks::<4>();
        for chunk in chunks {
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

    pub(crate) fn run() -> Result<(), String> {
        let options = parse_args()?;
        if options.list_devices {
            for device in camera_devices() {
                println!("{}\t{}", device.id, device.name);
            }
            return Ok(());
        }

        if options.once {
            let image = capture_single_frame(&CaptureConfig {
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
}

#[cfg(any(target_os = "macos", target_os = "linux", windows))]
fn main() -> Result<(), String> {
    harness::run()
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn main() {
    eprintln!("camera_preview is only supported on desktop platforms.");
}
