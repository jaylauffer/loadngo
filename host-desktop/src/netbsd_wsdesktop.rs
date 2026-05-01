use std::{
    ffi::CString,
    io,
    mem::{self, MaybeUninit},
    os::fd::RawFd,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use loadngo_proactor::{CompletionKind, KqueuePort, Proactor, ProactorHandle, ReadinessEvent};

use crate::netbsd_wsdisplay::{WsdisplayInfo, WsdisplaySurface};

const WSCONS_EVENT_KEY_UP: u32 = 1;
const WSCONS_EVENT_KEY_DOWN: u32 = 2;
const WSCONS_EVENT_ALL_KEYS_UP: u32 = 3;
const WSCONS_EVENT_MOUSE_UP: u32 = 4;
const WSCONS_EVENT_MOUSE_DOWN: u32 = 5;
const WSCONS_EVENT_MOUSE_DELTA_X: u32 = 6;
const WSCONS_EVENT_MOUSE_DELTA_Y: u32 = 7;
const WSCONS_EVENT_MOUSE_ABSOLUTE_X: u32 = 8;
const WSCONS_EVENT_MOUSE_ABSOLUTE_Y: u32 = 9;
const WSCONS_EVENT_MOUSE_DELTA_Z: u32 = 10;
const WSCONS_EVENT_ASCII: u32 = 13;
const WSCONS_EVENT_HSCROLL: u32 = 16;
const WSCONS_EVENT_VSCROLL: u32 = 17;

const WSEVENT_VERSION: i32 = 1;
const WSDESKTOP_MOUSE_TOKEN: u64 = 200;
const WSDESKTOP_KEYBOARD_TOKEN: u64 = 201;

const WSCONS_GROUP: u8 = b'W';
const IOC_IN: u64 = 0x8000_0000;
const IOCPARM_MASK: usize = 0x1fff;
const WSKBDIO_SETVERSION: libc::c_ulong =
    iow_const(WSCONS_GROUP, 25, mem::size_of::<libc::c_int>());
const WSMOUSEIO_SETVERSION: libc::c_ulong =
    iow_const(WSCONS_GROUP, 41, mem::size_of::<libc::c_int>());

#[derive(Debug, Clone)]
pub struct WsDesktopOptions {
    pub display_path: String,
    pub mouse_path: Option<String>,
    pub keyboard_path: Option<String>,
    pub fps: u32,
    pub cursor_hz: u32,
    pub max_runtime: Option<Duration>,
    pub continuous: bool,
}

impl Default for WsDesktopOptions {
    fn default() -> Self {
        Self {
            display_path: "/dev/ttyE0".to_string(),
            mouse_path: Some("/dev/wsmouse".to_string()),
            keyboard_path: Some("/dev/wskbd".to_string()),
            fps: 2,
            cursor_hz: 60,
            max_runtime: None,
            continuous: false,
        }
    }
}

pub fn run_desktop(options: WsDesktopOptions) -> Result<(), String> {
    let mut surface = WsdisplaySurface::open(&options.display_path)?;
    let info = surface.info();
    let mut back_buffer = BackBuffer::new(info);
    let proactor =
        Proactor::new(KqueuePort::new().map_err(|err| {
            format!("failed to create NetBSD kqueue proactor for desktop: {err}")
        })?);
    let handle = proactor.handle();
    let frame_period = frame_period(options.fps);
    let cursor_period = rate_period(options.cursor_hz, 60);
    let state = Arc::new(Mutex::new(DesktopState::new(
        info,
        frame_period,
        cursor_period,
    )));
    let mut cursor_presenter = CursorPresenter::new(info);
    let mut input_devices = Vec::new();

    if let Some(path) = options.mouse_path.as_deref() {
        match InputDevice::open(path, WSMOUSEIO_SETVERSION) {
            Ok(device) => {
                register_input(
                    &handle,
                    Arc::clone(&state),
                    device.fd,
                    WSDESKTOP_MOUSE_TOKEN,
                    InputKind::Mouse,
                )?;
                input_devices.push(device);
            }
            Err(err) => state
                .lock()
                .expect("desktop state poisoned")
                .push_log(format!("mouse unavailable: {err}")),
        }
    }

    if let Some(path) = options.keyboard_path.as_deref() {
        match InputDevice::open(path, WSKBDIO_SETVERSION) {
            Ok(device) => {
                register_input(
                    &handle,
                    Arc::clone(&state),
                    device.fd,
                    WSDESKTOP_KEYBOARD_TOKEN,
                    InputKind::Keyboard,
                )?;
                input_devices.push(device);
            }
            Err(err) => state
                .lock()
                .expect("desktop state poisoned")
                .push_log(format!("keyboard unavailable: {err}")),
        }
    }

    {
        let mut state = state.lock().expect("desktop state poisoned");
        state.push_log(format!("display {}", options.display_path));
        state.push_log(format!("input devices {}", input_devices.len()));
        state.request_repaint();
    }

    request_frame(&handle, Arc::clone(&state))?;
    if let Some(max_runtime) = options.max_runtime {
        let stop_handle = handle.clone();
        handle
            .defer_for(max_runtime, CompletionKind::Timer, 0, move |_| {
                let _ = stop_handle.stop();
            })
            .map_err(|err| format!("failed to schedule desktop runtime limit: {err}"))?;
    }

    while handle.is_running() {
        proactor
            .run_once()
            .map_err(|err| format!("desktop proactor failed: {err}"))?;
        let should_render = {
            let mut state = state.lock().expect("desktop state poisoned");
            if state.needs_frame && state.last_render.elapsed() >= state.frame_period {
                state.needs_frame = false;
                state.last_render = Instant::now();
                true
            } else {
                false
            }
        };
        if should_render {
            {
                let mut state = state.lock().expect("desktop state poisoned");
                state.repaint_requested = false;
                state.needs_cursor = false;
                render_desktop(back_buffer.info(), back_buffer.pixels_mut(), &mut state);
            }
            surface
                .present(back_buffer.pixels())
                .map_err(|err| format!("desktop present failed: {err}"))?;
            {
                let mut state = state.lock().expect("desktop state poisoned");
                cursor_presenter.reset();
                cursor_presenter.present(&mut surface, &back_buffer, &state)?;
                state.last_cursor_present = Instant::now();
            }
            if options.continuous {
                state
                    .lock()
                    .expect("desktop state poisoned")
                    .request_repaint();
                request_frame(&handle, Arc::clone(&state))?;
            }
            continue;
        }

        let should_present_cursor = {
            let mut state = state.lock().expect("desktop state poisoned");
            if state.needs_cursor && state.last_cursor_present.elapsed() >= state.cursor_period {
                state.needs_cursor = false;
                state.last_cursor_present = Instant::now();
                true
            } else {
                false
            }
        };
        if should_present_cursor {
            let state = state.lock().expect("desktop state poisoned");
            cursor_presenter.present(&mut surface, &back_buffer, &state)?;
        }
    }

    drop(input_devices);
    Ok(())
}

struct BackBuffer {
    info: WsdisplayInfo,
    pixels: Vec<u8>,
}

impl BackBuffer {
    fn new(info: WsdisplayInfo) -> Self {
        Self {
            info,
            pixels: vec![0; info.visible_len()],
        }
    }

    fn info(&self) -> WsdisplayInfo {
        self.info
    }

    fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    fn pixels_mut(&mut self) -> &mut [u8] {
        &mut self.pixels
    }
}

struct CursorPresenter {
    info: WsdisplayInfo,
    last_rect: Option<DamageRect>,
}

impl CursorPresenter {
    fn new(info: WsdisplayInfo) -> Self {
        Self {
            info,
            last_rect: None,
        }
    }

    fn reset(&mut self) {
        self.last_rect = None;
    }

    fn present(
        &mut self,
        surface: &mut WsdisplaySurface,
        back_buffer: &BackBuffer,
        state: &DesktopState,
    ) -> Result<(), String> {
        let framebuffer = surface.framebuffer_mut();
        if let Some(rect) = self.last_rect {
            restore_rect(self.info, back_buffer.pixels(), framebuffer, rect)?;
        }

        let next_rect = DamageRect::cursor(self.info, state.cursor_x, state.cursor_y);
        if next_rect.is_some() {
            Canvas::new(self.info, framebuffer).cursor(state.cursor_x, state.cursor_y);
        }
        self.last_rect = next_rect;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DamageRect {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

impl DamageRect {
    fn clipped(info: WsdisplayInfo, x: i32, y: i32, width: i32, height: i32) -> Option<Self> {
        let left = x.clamp(0, info.width as i32);
        let top = y.clamp(0, info.height as i32);
        let right = x.saturating_add(width).clamp(0, info.width as i32);
        let bottom = y.saturating_add(height).clamp(0, info.height as i32);
        (right > left && bottom > top).then_some(Self {
            x: left,
            y: top,
            width: right - left,
            height: bottom - top,
        })
    }

    fn cursor(info: WsdisplayInfo, x: i32, y: i32) -> Option<Self> {
        Self::clipped(info, x, y, CURSOR_DAMAGE_WIDTH, CURSOR_DAMAGE_HEIGHT)
    }
}

const CURSOR_DAMAGE_WIDTH: i32 = 20;
const CURSOR_DAMAGE_HEIGHT: i32 = 26;

fn restore_rect(
    info: WsdisplayInfo,
    source: &[u8],
    destination: &mut [u8],
    rect: DamageRect,
) -> Result<(), String> {
    if source.len() != destination.len() {
        return Err(format!(
            "cursor restore length mismatch: source={} destination={}",
            source.len(),
            destination.len()
        ));
    }

    let bytes_per_pixel = info.bytes_per_pixel();
    let row_len = rect.width as usize * bytes_per_pixel;
    for row in rect.y..rect.y + rect.height {
        let start = row as usize * info.stride as usize + rect.x as usize * bytes_per_pixel;
        let end = start + row_len;
        if end > source.len() {
            return Err(format!(
                "cursor restore rectangle exceeds framebuffer: end={} len={}",
                end,
                source.len()
            ));
        }
        destination[start..end].copy_from_slice(&source[start..end]);
    }
    Ok(())
}

fn frame_period(fps: u32) -> Duration {
    rate_period(fps, 60)
}

fn rate_period(hz: u32, max_hz: u32) -> Duration {
    Duration::from_nanos(1_000_000_000 / u64::from(hz.clamp(1, max_hz)))
}

fn request_frame(
    handle: &ProactorHandle<KqueuePort>,
    state: Arc<Mutex<DesktopState>>,
) -> Result<(), String> {
    let delay = {
        let mut state = state.lock().expect("desktop state poisoned");
        if state.frame_pending {
            return Ok(());
        }
        if !state.repaint_requested {
            return Ok(());
        }
        state.frame_pending = true;
        let elapsed = state.last_render.elapsed();
        if elapsed >= state.frame_period {
            Duration::ZERO
        } else {
            state.frame_period - elapsed
        }
    };

    handle
        .defer_for(delay, CompletionKind::Timer, 0, move |_| {
            let mut state = state.lock().expect("desktop state poisoned");
            state.needs_frame = true;
            state.frame_pending = false;
            state.frame_count = state.frame_count.saturating_add(1);
        })
        .map_err(|err| format!("failed to request desktop frame: {err}"))
}

fn request_cursor(
    handle: &ProactorHandle<KqueuePort>,
    state: Arc<Mutex<DesktopState>>,
) -> Result<(), String> {
    let delay = {
        let mut state = state.lock().expect("desktop state poisoned");
        if state.cursor_pending {
            return Ok(());
        }
        state.cursor_pending = true;
        let elapsed = state.last_cursor_present.elapsed();
        if elapsed >= state.cursor_period {
            Duration::ZERO
        } else {
            state.cursor_period - elapsed
        }
    };

    handle
        .defer_for(delay, CompletionKind::Timer, 0, move |_| {
            let mut state = state.lock().expect("desktop state poisoned");
            state.needs_cursor = true;
            state.cursor_pending = false;
        })
        .map_err(|err| format!("failed to request cursor frame: {err}"))
}

fn register_input(
    handle: &ProactorHandle<KqueuePort>,
    state: Arc<Mutex<DesktopState>>,
    fd: RawFd,
    token: u64,
    kind: InputKind,
) -> Result<(), String> {
    let stop_handle = handle.clone();
    let frame_handle = handle.clone();
    let cursor_handle = handle.clone();
    handle
        .register_readable(fd, token, move |_: ReadinessEvent| {
            let mut quit = false;
            let mut needs_scene = false;
            let mut needs_cursor = false;
            let result = drain_wscons_events(fd, |event| {
                let mut state = state.lock().expect("desktop state poisoned");
                let damage = state.apply_event(kind, event);
                if damage.scene {
                    state.request_repaint();
                    needs_scene = true;
                }
                needs_cursor |= damage.cursor;
                quit |= state.quit_requested;
            });
            if let Err(err) = result {
                let mut state = state.lock().expect("desktop state poisoned");
                state.push_log(format!("{kind:?} read failed: {err}"));
                state.request_repaint();
                needs_scene = true;
            }
            if needs_scene {
                let _ = request_frame(&frame_handle, Arc::clone(&state));
            }
            if needs_cursor {
                let _ = request_cursor(&cursor_handle, Arc::clone(&state));
            }
            if quit {
                let _ = stop_handle.stop();
            }
        })
        .map_err(|err| format!("failed to register {kind:?} readiness: {err}"))
}

#[derive(Debug, Clone, Copy)]
enum InputKind {
    Mouse,
    Keyboard,
}

struct InputDevice {
    fd: RawFd,
}

impl InputDevice {
    fn open(path: &str, version_request: libc::c_ulong) -> Result<Self, String> {
        let c_path = CString::new(path)
            .map_err(|_| format!("input path contains an interior NUL: {path:?}"))?;
        let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDONLY | libc::O_NONBLOCK) };
        if fd < 0 {
            return Err(format!("open {path}: {}", io::Error::last_os_error()));
        }

        let mut version = WSEVENT_VERSION as libc::c_int;
        let rc = unsafe { libc::ioctl(fd, version_request, &mut version) };
        if rc < 0 {
            let err = io::Error::last_os_error();
            unsafe {
                libc::close(fd);
            }
            return Err(format!("set event version on {path}: {err}"));
        }

        Ok(Self { fd })
    }
}

impl Drop for InputDevice {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.fd);
        }
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct WsconsEvent {
    event_type: libc::c_uint,
    value: libc::c_int,
    time: libc::timespec,
}

fn drain_wscons_events(fd: RawFd, mut on_event: impl FnMut(WsconsEvent)) -> Result<(), io::Error> {
    loop {
        let mut event = MaybeUninit::<WsconsEvent>::uninit();
        let read_len = unsafe {
            libc::read(
                fd,
                event.as_mut_ptr() as *mut libc::c_void,
                mem::size_of::<WsconsEvent>(),
            )
        };
        if read_len == mem::size_of::<WsconsEvent>() as isize {
            on_event(unsafe { event.assume_init() });
            continue;
        }
        if read_len == 0 {
            return Ok(());
        }
        if read_len < 0 {
            let err = io::Error::last_os_error();
            if matches!(
                err.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) {
                return Ok(());
            }
            return Err(err);
        }
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            format!("partial wscons event read: {read_len} bytes"),
        ));
    }
}

#[derive(Clone, Copy)]
struct WindowModel {
    app: AppKind,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AppKind {
    Terminal,
    Files,
    Monitor,
}

impl AppKind {
    const ALL: [Self; 3] = [Self::Terminal, Self::Files, Self::Monitor];

    fn title(self) -> &'static str {
        match self {
            Self::Terminal => "TERMINAL",
            Self::Files => "FILES",
            Self::Monitor => "MONITOR",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Self::Terminal => ">_",
            Self::Files => "[]",
            Self::Monitor => "##",
        }
    }
}

#[derive(Clone, Copy)]
struct DragState {
    app: AppKind,
    offset_x: i32,
    offset_y: i32,
}

#[derive(Default)]
struct DamageRequest {
    scene: bool,
    cursor: bool,
}

struct DesktopState {
    width: i32,
    height: i32,
    cursor_x: i32,
    cursor_y: i32,
    buttons: u32,
    scroll: i32,
    active_app: AppKind,
    windows: Vec<WindowModel>,
    dragging: Option<DragState>,
    needs_frame: bool,
    frame_pending: bool,
    repaint_requested: bool,
    quit_requested: bool,
    frame_count: u64,
    input_count: u64,
    start: Instant,
    last_render: Instant,
    last_cursor_present: Instant,
    frame_period: Duration,
    cursor_period: Duration,
    needs_cursor: bool,
    cursor_pending: bool,
    logs: Vec<String>,
    last_input: String,
}

impl DesktopState {
    fn new(info: WsdisplayInfo, frame_period: Duration, cursor_period: Duration) -> Self {
        let width = info.width as i32;
        let height = info.height as i32;
        let base_w = (width - 160).clamp(500, 980);
        let base_h = (height - 220).clamp(360, 680);
        Self {
            width,
            height,
            cursor_x: width / 2,
            cursor_y: height / 2,
            buttons: 0,
            scroll: 0,
            active_app: AppKind::Terminal,
            windows: vec![
                WindowModel {
                    app: AppKind::Terminal,
                    x: 112,
                    y: 96,
                    width: base_w,
                    height: base_h,
                },
                WindowModel {
                    app: AppKind::Files,
                    x: 160,
                    y: 132,
                    width: base_w - 80,
                    height: base_h - 40,
                },
                WindowModel {
                    app: AppKind::Monitor,
                    x: 208,
                    y: 168,
                    width: base_w - 120,
                    height: base_h - 80,
                },
            ],
            dragging: None,
            needs_frame: false,
            frame_pending: false,
            repaint_requested: true,
            quit_requested: false,
            frame_count: 0,
            input_count: 0,
            start: Instant::now(),
            last_render: Instant::now() - frame_period,
            last_cursor_present: Instant::now() - cursor_period,
            frame_period,
            cursor_period,
            needs_cursor: false,
            cursor_pending: false,
            logs: Vec::new(),
            last_input: "READY".to_string(),
        }
    }

    fn push_log(&mut self, message: impl Into<String>) {
        self.logs.push(message.into());
        if self.logs.len() > 6 {
            self.logs.remove(0);
        }
    }

    fn request_repaint(&mut self) {
        self.repaint_requested = true;
    }

    fn apply_event(&mut self, kind: InputKind, event: WsconsEvent) -> DamageRequest {
        self.input_count = self.input_count.saturating_add(1);
        let mut damage = DamageRequest::default();
        match event.event_type {
            WSCONS_EVENT_MOUSE_DELTA_X => {
                let before = self.cursor_x;
                self.cursor_x = (self.cursor_x + event.value).clamp(0, self.width - 1);
                damage.cursor |= before != self.cursor_x;
                damage.scene |= self.update_drag();
            }
            WSCONS_EVENT_MOUSE_DELTA_Y => {
                let before = self.cursor_y;
                self.cursor_y = (self.cursor_y + event.value).clamp(0, self.height - 1);
                damage.cursor |= before != self.cursor_y;
                damage.scene |= self.update_drag();
            }
            WSCONS_EVENT_MOUSE_ABSOLUTE_X => {
                let before = self.cursor_x;
                self.cursor_x = event.value.clamp(0, self.width - 1);
                damage.cursor |= before != self.cursor_x;
                damage.scene |= self.update_drag();
            }
            WSCONS_EVENT_MOUSE_ABSOLUTE_Y => {
                let before = self.cursor_y;
                self.cursor_y = event.value.clamp(0, self.height - 1);
                damage.cursor |= before != self.cursor_y;
                damage.scene |= self.update_drag();
            }
            WSCONS_EVENT_MOUSE_DOWN => {
                if (0..32).contains(&event.value) {
                    self.buttons |= 1 << event.value;
                }
                if event.value == 0 {
                    damage.scene |= self.handle_pointer_down();
                }
                damage.scene = true;
                damage.cursor = true;
            }
            WSCONS_EVENT_MOUSE_UP => {
                if (0..32).contains(&event.value) {
                    self.buttons &= !(1 << event.value);
                }
                if event.value == 0 {
                    self.dragging = None;
                }
                damage.scene = true;
                damage.cursor = true;
            }
            WSCONS_EVENT_MOUSE_DELTA_Z | WSCONS_EVENT_HSCROLL | WSCONS_EVENT_VSCROLL => {
                self.scroll = self.scroll.saturating_add(event.value);
                damage.scene = true;
            }
            WSCONS_EVENT_KEY_DOWN => {
                let before_cursor = (self.cursor_x, self.cursor_y);
                let before_active = self.active_app;
                let before_quit = self.quit_requested;
                self.handle_key_code(event.value);
                damage.cursor |= before_cursor != (self.cursor_x, self.cursor_y);
                damage.scene |=
                    before_active != self.active_app || before_quit != self.quit_requested;
            }
            WSCONS_EVENT_ASCII => {
                let before_active = self.active_app;
                let before_quit = self.quit_requested;
                self.handle_ascii(event.value);
                damage.scene |=
                    before_active != self.active_app || before_quit != self.quit_requested;
            }
            WSCONS_EVENT_KEY_UP | WSCONS_EVENT_ALL_KEYS_UP => {}
            _ => {}
        }
        self.last_input = format!("{kind:?} type={} value={}", event.event_type, event.value);
        damage
    }

    fn handle_pointer_down(&mut self) -> bool {
        if self.cursor_y >= self.height - 42 && self.cursor_x >= self.width - 104 {
            self.quit_requested = true;
            return true;
        }

        if self.cursor_x < 84 && self.cursor_y >= 48 {
            let slot = ((self.cursor_y - 56) / 88) as usize;
            if let Some(app) = AppKind::ALL.get(slot).copied() {
                self.active_app = app;
                self.push_log(format!("activated {}", app.title()));
                return true;
            }
        }

        for index in (0..self.windows.len()).rev() {
            let window = self.windows[index];
            if point_in_rect(
                self.cursor_x,
                self.cursor_y,
                window.x,
                window.y,
                window.width,
                34,
            ) {
                self.active_app = window.app;
                self.dragging = Some(DragState {
                    app: window.app,
                    offset_x: self.cursor_x - window.x,
                    offset_y: self.cursor_y - window.y,
                });
                return true;
            }
            if point_in_rect(
                self.cursor_x,
                self.cursor_y,
                window.x,
                window.y,
                window.width,
                window.height,
            ) {
                self.active_app = window.app;
                return true;
            }
        }
        false
    }

    fn update_drag(&mut self) -> bool {
        let Some(drag) = self.dragging else {
            return false;
        };
        if self.buttons & 1 == 0 {
            self.dragging = None;
            return true;
        }
        if let Some(window) = self
            .windows
            .iter_mut()
            .find(|window| window.app == drag.app)
        {
            let next_x = (self.cursor_x - drag.offset_x).clamp(88, self.width - 96);
            let next_y = (self.cursor_y - drag.offset_y).clamp(48, self.height - 96);
            let moved = window.x != next_x || window.y != next_y;
            window.x = next_x;
            window.y = next_y;
            return moved;
        }
        false
    }

    fn handle_ascii(&mut self, value: i32) {
        match value {
            9 => self.cycle_app(),
            10 | 13 | 32 => self.cycle_app(),
            27 | 81 | 113 => self.quit_requested = true,
            _ => {}
        }
    }

    fn handle_key_code(&mut self, value: i32) {
        match value {
            1 | 16 => self.quit_requested = true,
            15 | 28 => self.cycle_app(),
            72 | 200 => self.cursor_y = (self.cursor_y - 20).clamp(0, self.height - 1),
            80 | 208 => self.cursor_y = (self.cursor_y + 20).clamp(0, self.height - 1),
            75 | 203 => self.cursor_x = (self.cursor_x - 20).clamp(0, self.width - 1),
            77 | 205 => self.cursor_x = (self.cursor_x + 20).clamp(0, self.width - 1),
            _ => {}
        }
    }

    fn cycle_app(&mut self) {
        let index = AppKind::ALL
            .iter()
            .position(|app| *app == self.active_app)
            .unwrap_or(0);
        self.active_app = AppKind::ALL[(index + 1) % AppKind::ALL.len()];
        self.push_log(format!("activated {}", self.active_app.title()));
    }

    fn active_window(&self) -> Option<WindowModel> {
        self.windows
            .iter()
            .find(|window| window.app == self.active_app)
            .copied()
    }
}

fn render_desktop(info: WsdisplayInfo, framebuffer: &mut [u8], state: &mut DesktopState) {
    let mut canvas = Canvas::new(info, framebuffer);
    canvas.clear((28, 36, 44));
    canvas.fill_rect(0, 0, state.width, state.height, (30, 42, 54));
    canvas.fill_rect(0, 0, state.width, 38, (18, 22, 28));
    canvas.fill_rect(0, state.height - 42, state.width, 42, (20, 24, 30));
    canvas.fill_rect(0, 38, 84, state.height - 80, (24, 31, 38));
    canvas.stroke_rect(84, 38, state.width - 84, state.height - 80, (53, 65, 74));

    canvas.text(16, 12, "LOADNGO NETBSD", 2, (230, 236, 225));
    canvas.text(310, 12, "KQUEUE PROACTOR", 2, (125, 213, 184));
    let uptime = state.start.elapsed().as_secs();
    canvas.text(
        state.width - 260,
        12,
        &format!("UP {}S", uptime),
        2,
        (224, 191, 92),
    );

    draw_launcher(&mut canvas, state);
    draw_wallpaper(&mut canvas, state);

    for app in AppKind::ALL {
        if app != state.active_app {
            if let Some(window) = state.windows.iter().find(|window| window.app == app) {
                draw_window(&mut canvas, state, *window, false);
            }
        }
    }
    if let Some(window) = state.active_window() {
        draw_window(&mut canvas, state, window, true);
    }

    draw_bottom_bar(&mut canvas, state);
}

fn draw_launcher(canvas: &mut Canvas<'_>, state: &DesktopState) {
    for (index, app) in AppKind::ALL.iter().copied().enumerate() {
        let y = 56 + index as i32 * 88;
        let active = app == state.active_app;
        let fill = if active { (58, 83, 85) } else { (31, 40, 49) };
        let stroke = if active {
            (125, 213, 184)
        } else {
            (64, 76, 86)
        };
        canvas.fill_rect(12, y, 60, 68, fill);
        canvas.stroke_rect(12, y, 60, 68, stroke);
        canvas.text(25, y + 12, app.icon(), 3, (230, 236, 225));
        canvas.text(18, y + 48, app.title(), 1, (210, 218, 213));
    }
}

fn draw_wallpaper(canvas: &mut Canvas<'_>, state: &DesktopState) {
    let origin_x = 112;
    let origin_y = state.height - 185;
    for i in 0..5 {
        let x = origin_x + i * 72;
        let h = 50 + (i % 3) * 24;
        let color = match i % 3 {
            0 => (64, 112, 128),
            1 => (94, 128, 96),
            _ => (146, 112, 76),
        };
        canvas.fill_rect(x, origin_y - h, 44, h, color);
        canvas.stroke_rect(x, origin_y - h, 44, h, (186, 199, 188));
    }
    canvas.text(
        origin_x,
        origin_y + 18,
        "WS DISPLAY SHELL",
        2,
        (105, 126, 136),
    );
}

fn draw_window(canvas: &mut Canvas<'_>, state: &DesktopState, window: WindowModel, active: bool) {
    let title_color = if active { (38, 92, 96) } else { (42, 48, 54) };
    let border = if active {
        (125, 213, 184)
    } else {
        (82, 92, 98)
    };
    canvas.fill_rect(
        window.x + 8,
        window.y + 10,
        window.width,
        window.height,
        (13, 16, 20),
    );
    canvas.fill_rect(
        window.x,
        window.y,
        window.width,
        window.height,
        (232, 234, 226),
    );
    canvas.stroke_rect(window.x, window.y, window.width, window.height, border);
    canvas.fill_rect(window.x, window.y, window.width, 34, title_color);
    canvas.text(
        window.x + 12,
        window.y + 10,
        window.app.title(),
        2,
        (244, 246, 238),
    );
    canvas.fill_rect(
        window.x + window.width - 58,
        window.y + 8,
        42,
        18,
        (124, 60, 54),
    );
    canvas.text(
        window.x + window.width - 48,
        window.y + 13,
        "X",
        1,
        (255, 235, 220),
    );
    canvas.fill_rect(
        window.x + 1,
        window.y + 35,
        window.width - 2,
        window.height - 36,
        (238, 240, 232),
    );

    match window.app {
        AppKind::Terminal => draw_terminal(canvas, state, window),
        AppKind::Files => draw_files(canvas, window),
        AppKind::Monitor => draw_monitor(canvas, state, window),
    }
}

fn draw_terminal(canvas: &mut Canvas<'_>, state: &DesktopState, window: WindowModel) {
    let x = window.x + 18;
    let mut y = window.y + 54;
    canvas.fill_rect(
        x - 8,
        y - 8,
        window.width - 36,
        window.height - 68,
        (18, 24, 25),
    );
    for line in [
        "$ LOADNGO DESKTOP",
        "WSDISPLAY FRAMEBUFFER ONLINE",
        "INPUT THROUGH WSCONS EVENTS",
        "CLICK ICONS OR DRAG TITLE BAR",
        "PRESS Q OR CLICK QUIT TO EXIT",
    ] {
        canvas.text(x, y, line, 2, (130, 229, 184));
        y += 28;
    }
    y += 8;
    for log in state.logs.iter().rev().take(4) {
        canvas.text(x, y, log, 1, (210, 218, 213));
        y += 16;
    }
}

fn draw_files(canvas: &mut Canvas<'_>, window: WindowModel) {
    let x = window.x + 18;
    let mut y = window.y + 56;
    for entry in [
        "HOST-DESKTOP/",
        "PROACTOR/",
        "RENDERER/",
        "UI-CORE/",
        "NETBSD_DISPLAY_BRINGUP.MD",
    ] {
        canvas.fill_rect(x, y - 6, window.width - 38, 24, (222, 226, 216));
        canvas.stroke_rect(x, y - 6, window.width - 38, 24, (190, 198, 188));
        canvas.text(x + 10, y, entry, 2, (54, 63, 68));
        y += 34;
    }
}

fn draw_monitor(canvas: &mut Canvas<'_>, state: &DesktopState, window: WindowModel) {
    let x = window.x + 18;
    let mut y = window.y + 58;
    for line in [
        format!("FRAMES {}", state.frame_count),
        format!("INPUT {}", state.input_count),
        format!("CURSOR {},{}", state.cursor_x, state.cursor_y),
        format!("BUTTONS {:X}", state.buttons),
        format!("FRAME {}MS", state.frame_period.as_millis()),
        format!("LAST {}", state.last_input),
    ] {
        canvas.text(x, y, &line, 2, (48, 57, 62));
        y += 30;
    }
}

fn draw_bottom_bar(canvas: &mut Canvas<'_>, state: &DesktopState) {
    let y = state.height - 31;
    canvas.text(16, y, "TAB/ENTER CYCLE  Q QUIT", 2, (213, 219, 213));
    canvas.text(
        420,
        y,
        &format!("CURSOR {},{}", state.cursor_x, state.cursor_y),
        2,
        (224, 191, 92),
    );
    canvas.fill_rect(state.width - 104, state.height - 34, 88, 26, (124, 60, 54));
    canvas.stroke_rect(
        state.width - 104,
        state.height - 34,
        88,
        26,
        (224, 158, 120),
    );
    canvas.text(
        state.width - 82,
        state.height - 27,
        "QUIT",
        2,
        (255, 236, 224),
    );
}

struct Canvas<'a> {
    info: WsdisplayInfo,
    framebuffer: &'a mut [u8],
}

impl<'a> Canvas<'a> {
    fn new(info: WsdisplayInfo, framebuffer: &'a mut [u8]) -> Self {
        Self { info, framebuffer }
    }

    fn clear(&mut self, color: (u8, u8, u8)) {
        self.fill_rect(0, 0, self.info.width as i32, self.info.height as i32, color);
    }

    fn fill_rect(&mut self, x: i32, y: i32, width: i32, height: i32, color: (u8, u8, u8)) {
        let left = x.clamp(0, self.info.width as i32);
        let top = y.clamp(0, self.info.height as i32);
        let right = (x + width).clamp(0, self.info.width as i32);
        let bottom = (y + height).clamp(0, self.info.height as i32);
        if right <= left || bottom <= top {
            return;
        }
        let pixel = self.info.pack_rgb(color.0, color.1, color.2).to_ne_bytes();
        let bytes_per_pixel = self.info.bytes_per_pixel();
        for row in top..bottom {
            let mut offset =
                row as usize * self.info.stride as usize + left as usize * bytes_per_pixel;
            for _ in left..right {
                match bytes_per_pixel {
                    1 => self.framebuffer[offset] = pixel[0],
                    2 => self.framebuffer[offset..offset + 2].copy_from_slice(&pixel[..2]),
                    3 => self.framebuffer[offset..offset + 3].copy_from_slice(&pixel[..3]),
                    _ => self.framebuffer[offset..offset + 4].copy_from_slice(&pixel),
                }
                offset += bytes_per_pixel;
            }
        }
    }

    fn stroke_rect(&mut self, x: i32, y: i32, width: i32, height: i32, color: (u8, u8, u8)) {
        self.fill_rect(x, y, width, 1, color);
        self.fill_rect(x, y + height - 1, width, 1, color);
        self.fill_rect(x, y, 1, height, color);
        self.fill_rect(x + width - 1, y, 1, height, color);
    }

    fn text(&mut self, x: i32, y: i32, text: &str, scale: i32, color: (u8, u8, u8)) {
        let mut cursor = x;
        for ch in text.chars() {
            if ch == ' ' {
                cursor += 4 * scale;
            } else {
                self.glyph(cursor, y, ch, scale, color);
                cursor += 6 * scale;
            }
        }
    }

    fn glyph(&mut self, x: i32, y: i32, ch: char, scale: i32, color: (u8, u8, u8)) {
        let rows = glyph_rows(ch);
        for (row, bits) in rows.iter().enumerate() {
            for col in 0..5usize {
                if bits & (1 << (4 - col)) != 0 {
                    self.fill_rect(
                        x + col as i32 * scale,
                        y + row as i32 * scale,
                        scale,
                        scale,
                        color,
                    );
                }
            }
        }
    }

    fn cursor(&mut self, x: i32, y: i32) {
        for row in 0..18 {
            for col in 0..=row.min(10) {
                self.fill_rect(x + col, y + row, 1, 1, (248, 250, 240));
            }
        }
        for row in 0..18 {
            self.fill_rect(x, y + row, 1, 1, (12, 15, 18));
            self.fill_rect(x + row.min(10), y + row, 1, 1, (12, 15, 18));
        }
        self.fill_rect(x + 7, y + 14, 5, 9, (12, 15, 18));
        self.fill_rect(x + 8, y + 15, 3, 7, (248, 250, 240));
    }
}

fn glyph_rows(ch: char) -> [u8; 7] {
    match ch.to_ascii_uppercase() {
        'A' => [0x0e, 0x11, 0x11, 0x1f, 0x11, 0x11, 0x11],
        'B' => [0x1e, 0x11, 0x11, 0x1e, 0x11, 0x11, 0x1e],
        'C' => [0x0e, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0e],
        'D' => [0x1e, 0x11, 0x11, 0x11, 0x11, 0x11, 0x1e],
        'E' => [0x1f, 0x10, 0x10, 0x1e, 0x10, 0x10, 0x1f],
        'F' => [0x1f, 0x10, 0x10, 0x1e, 0x10, 0x10, 0x10],
        'G' => [0x0e, 0x11, 0x10, 0x17, 0x11, 0x11, 0x0f],
        'H' => [0x11, 0x11, 0x11, 0x1f, 0x11, 0x11, 0x11],
        'I' => [0x1f, 0x04, 0x04, 0x04, 0x04, 0x04, 0x1f],
        'J' => [0x1f, 0x02, 0x02, 0x02, 0x12, 0x12, 0x0c],
        'K' => [0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11],
        'L' => [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1f],
        'M' => [0x11, 0x1b, 0x15, 0x15, 0x11, 0x11, 0x11],
        'N' => [0x11, 0x19, 0x15, 0x13, 0x11, 0x11, 0x11],
        'O' => [0x0e, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0e],
        'P' => [0x1e, 0x11, 0x11, 0x1e, 0x10, 0x10, 0x10],
        'Q' => [0x0e, 0x11, 0x11, 0x11, 0x15, 0x12, 0x0d],
        'R' => [0x1e, 0x11, 0x11, 0x1e, 0x14, 0x12, 0x11],
        'S' => [0x0f, 0x10, 0x10, 0x0e, 0x01, 0x01, 0x1e],
        'T' => [0x1f, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        'U' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0e],
        'V' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x0a, 0x04],
        'W' => [0x11, 0x11, 0x11, 0x15, 0x15, 0x15, 0x0a],
        'X' => [0x11, 0x11, 0x0a, 0x04, 0x0a, 0x11, 0x11],
        'Y' => [0x11, 0x11, 0x0a, 0x04, 0x04, 0x04, 0x04],
        'Z' => [0x1f, 0x01, 0x02, 0x04, 0x08, 0x10, 0x1f],
        '0' => [0x0e, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0e],
        '1' => [0x04, 0x0c, 0x04, 0x04, 0x04, 0x04, 0x0e],
        '2' => [0x0e, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1f],
        '3' => [0x1e, 0x01, 0x01, 0x0e, 0x01, 0x01, 0x1e],
        '4' => [0x02, 0x06, 0x0a, 0x12, 0x1f, 0x02, 0x02],
        '5' => [0x1f, 0x10, 0x10, 0x1e, 0x01, 0x01, 0x1e],
        '6' => [0x0e, 0x10, 0x10, 0x1e, 0x11, 0x11, 0x0e],
        '7' => [0x1f, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0e, 0x11, 0x11, 0x0e, 0x11, 0x11, 0x0e],
        '9' => [0x0e, 0x11, 0x11, 0x0f, 0x01, 0x01, 0x0e],
        ':' => [0x00, 0x04, 0x04, 0x00, 0x04, 0x04, 0x00],
        '.' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x0c, 0x0c],
        ',' => [0x00, 0x00, 0x00, 0x00, 0x0c, 0x04, 0x08],
        '-' => [0x00, 0x00, 0x00, 0x1f, 0x00, 0x00, 0x00],
        '_' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x1f],
        '/' => [0x01, 0x01, 0x02, 0x04, 0x08, 0x10, 0x10],
        '\\' => [0x10, 0x10, 0x08, 0x04, 0x02, 0x01, 0x01],
        '>' => [0x10, 0x08, 0x04, 0x02, 0x04, 0x08, 0x10],
        '<' => [0x01, 0x02, 0x04, 0x08, 0x04, 0x02, 0x01],
        '[' => [0x0e, 0x08, 0x08, 0x08, 0x08, 0x08, 0x0e],
        ']' => [0x0e, 0x02, 0x02, 0x02, 0x02, 0x02, 0x0e],
        '#' => [0x0a, 0x1f, 0x0a, 0x0a, 0x1f, 0x0a, 0x00],
        '$' => [0x04, 0x0f, 0x14, 0x0e, 0x05, 0x1e, 0x04],
        '!' => [0x04, 0x04, 0x04, 0x04, 0x04, 0x00, 0x04],
        '?' => [0x0e, 0x11, 0x01, 0x02, 0x04, 0x00, 0x04],
        _ => [0x1f, 0x11, 0x05, 0x02, 0x05, 0x11, 0x1f],
    }
}

fn point_in_rect(px: i32, py: i32, x: i32, y: i32, width: i32, height: i32) -> bool {
    px >= x && px < x + width && py >= y && py < y + height
}

const fn iow_const(group: u8, number: u8, len: usize) -> libc::c_ulong {
    (IOC_IN | (((len & IOCPARM_MASK) as u64) << 16) | ((group as u64) << 8) | number as u64)
        as libc::c_ulong
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wscons_event_shape_matches_netbsd_header() {
        assert_eq!(mem::size_of::<WsconsEvent>(), 24);
        assert_eq!(WSKBDIO_SETVERSION, 0x8004_5719);
        assert_eq!(WSMOUSEIO_SETVERSION, 0x8004_5729);
    }

    #[test]
    fn desktop_state_keeps_cursor_inside_surface() {
        let info = WsdisplayInfo {
            width: 100,
            height: 80,
            stride: 400,
            bits_per_pixel: 32,
            fb_size: 32000,
            fb_offset: 0,
            pixel_type: 0,
            red_offset: 16,
            red_size: 8,
            green_offset: 8,
            green_size: 8,
            blue_offset: 0,
            blue_size: 8,
            alpha_offset: 0,
            alpha_size: 0,
        };
        let mut state =
            DesktopState::new(info, Duration::from_millis(50), Duration::from_millis(16));
        let damage = state.apply_event(
            InputKind::Mouse,
            WsconsEvent {
                event_type: WSCONS_EVENT_MOUSE_DELTA_X,
                value: 1000,
                time: libc::timespec {
                    tv_sec: 0,
                    tv_nsec: 0,
                },
            },
        );
        assert_eq!(state.cursor_x, 99);
        assert!(damage.cursor);
        assert!(!damage.scene);
    }

    #[test]
    fn back_buffer_uses_visible_framebuffer_length() {
        let info = WsdisplayInfo {
            width: 10,
            height: 4,
            stride: 64,
            bits_per_pixel: 32,
            fb_size: 256,
            fb_offset: 0,
            pixel_type: 0,
            red_offset: 16,
            red_size: 8,
            green_offset: 8,
            green_size: 8,
            blue_offset: 0,
            blue_size: 8,
            alpha_offset: 0,
            alpha_size: 0,
        };
        let buffer = BackBuffer::new(info);
        assert_eq!(buffer.pixels().len(), 256);
    }

    #[test]
    fn cursor_damage_rect_clips_to_surface() {
        let info = WsdisplayInfo {
            width: 10,
            height: 8,
            stride: 40,
            bits_per_pixel: 32,
            fb_size: 320,
            fb_offset: 0,
            pixel_type: 0,
            red_offset: 16,
            red_size: 8,
            green_offset: 8,
            green_size: 8,
            blue_offset: 0,
            blue_size: 8,
            alpha_offset: 0,
            alpha_size: 0,
        };

        assert_eq!(
            DamageRect::clipped(info, -3, -2, 20, 26),
            Some(DamageRect {
                x: 0,
                y: 0,
                width: 10,
                height: 8,
            })
        );
        assert_eq!(DamageRect::clipped(info, 10, 8, 20, 26), None);
    }
}
