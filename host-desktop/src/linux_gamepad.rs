//! Linux gamepad input, read from evdev (`/dev/input/event*`).
//!
//! evdev rather than the older joydev (`/dev/input/js*`) because evdev
//! reports *semantic* codes — `BTN_SOUTH`, `ABS_RX`, `ABS_HAT0Y` — which map
//! straight onto [`GamepadButton`]'s position-named variants. joydev reports
//! opaque button indices whose meaning varies per driver, which would mean
//! shipping a per-controller quirk table to answer "which button is South?".
//! The one thing joydev gives for free is pre-normalized axes; evdev needs an
//! `EVIOCGABS` ioctl per axis to learn its range, which is the only `unsafe`
//! in this module.
//!
//! Mirrors `macos.rs`'s `GamepadTracker` in shape: poll once per frame,
//! return one [`GamepadSnapshot`] per connected pad, plus exactly one
//! `cleared()` snapshot on the frame a pad disappears.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use loadngo_host_core::{GamepadButton, GamepadSnapshot, GamepadStick, GamepadTrigger, PointF};

const EV_KEY: u16 = 0x01;
const EV_ABS: u16 = 0x03;

// Button codes from the kernel's `input-event-codes.h`. The names are the
// kernel's own position-based aliases (`BTN_SOUTH` == `BTN_A`), which is the
// same convention `GamepadButton` documents — so a driver that labels its
// buttons by physical position needs no translation table here, and one that
// gets it wrong is a driver bug rather than something to special-case.
const BTN_SOUTH: u16 = 0x130;
const BTN_EAST: u16 = 0x131;
const BTN_NORTH: u16 = 0x133;
const BTN_WEST: u16 = 0x134;
const BTN_TL: u16 = 0x136;
const BTN_TR: u16 = 0x137;
const BTN_SELECT: u16 = 0x13a;
const BTN_START: u16 = 0x13b;
const BTN_MODE: u16 = 0x13c;
const BTN_THUMBL: u16 = 0x13d;
const BTN_THUMBR: u16 = 0x13e;
// Some pads report the d-pad as four buttons instead of a hat axis.
const BTN_DPAD_UP: u16 = 0x220;
const BTN_DPAD_DOWN: u16 = 0x221;
const BTN_DPAD_LEFT: u16 = 0x222;
const BTN_DPAD_RIGHT: u16 = 0x223;

const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const ABS_Z: u16 = 0x02;
const ABS_RX: u16 = 0x03;
const ABS_RY: u16 = 0x04;
const ABS_RZ: u16 = 0x05;
const ABS_HAT0X: u16 = 0x10;
const ABS_HAT0Y: u16 = 0x11;

/// How often to look for newly plugged-in pads. evdev has no hotplug
/// notification without udev, and a directory scan is cheap next to a frame,
/// so poll for it rather than take a udev dependency.
const RESCAN_INTERVAL: Duration = Duration::from_millis(1000);

/// `struct input_absinfo`'s six `__s32` fields.
const ABSINFO_BYTES: usize = 24;

fn button_for_code(code: u16) -> Option<GamepadButton> {
    Some(match code {
        BTN_SOUTH => GamepadButton::South,
        BTN_EAST => GamepadButton::East,
        BTN_NORTH => GamepadButton::North,
        BTN_WEST => GamepadButton::West,
        BTN_TL => GamepadButton::LeftShoulder,
        BTN_TR => GamepadButton::RightShoulder,
        BTN_SELECT => GamepadButton::Select,
        BTN_START => GamepadButton::Start,
        BTN_MODE => GamepadButton::Guide,
        BTN_THUMBL => GamepadButton::LeftStick,
        BTN_THUMBR => GamepadButton::RightStick,
        BTN_DPAD_UP => GamepadButton::DPadUp,
        BTN_DPAD_DOWN => GamepadButton::DPadDown,
        BTN_DPAD_LEFT => GamepadButton::DPadLeft,
        BTN_DPAD_RIGHT => GamepadButton::DPadRight,
        // `BTN_TL2`/`BTN_TR2` are deliberately unmapped: they are the digital
        // shadow of the analog triggers, which are already reported
        // continuously through `ABS_Z`/`ABS_RZ`. Publishing both would
        // duplicate one physical control as two signals, against
        // `INPUT_PHILOSOPHY.md`'s "prefer continuous over boolean".
        _ => return None,
    })
}

/// One axis's range, as reported by the driver.
#[derive(Debug, Clone, Copy)]
struct AbsRange {
    minimum: i32,
    maximum: i32,
}

impl AbsRange {
    /// Maps a raw reading onto `-1.0..=1.0`.
    ///
    /// No y-flip anywhere in this module: evdev already reports "up" as the
    /// *lower* value on `ABS_Y`/`ABS_RY`, which is exactly the y-down
    /// convention `GamepadStick` documents. macOS needs a negation here
    /// because GameController reports up as `+1`; Linux does not, and adding
    /// one "for symmetry" would invert every stick.
    fn to_signed(self, value: i32) -> f32 {
        let span = self.maximum - self.minimum;
        if span <= 0 {
            return 0.0;
        }
        let normalized = f64::from(value - self.minimum) / f64::from(span);
        let signed = (normalized * 2.0 - 1.0) as f32;
        signed.clamp(-1.0, 1.0)
    }

    /// Maps a raw reading onto `0.0..=1.0`, for triggers that rest at zero.
    fn to_unsigned(self, value: i32) -> f32 {
        let span = self.maximum - self.minimum;
        if span <= 0 {
            return 0.0;
        }
        let unsigned = (f64::from(value - self.minimum) / f64::from(span)) as f32;
        unsigned.clamp(0.0, 1.0)
    }
}

/// Asks the driver for one axis's range.
///
/// The only ioctl in this module. Everything else — the event stream itself,
/// device discovery — is plain file and sysfs reads, because evdev's wire
/// format is stable and self-describing. Ranges are not available any other
/// way: sysfs publishes which axes exist, never their bounds.
fn read_abs_range(file: &File, code: u16) -> Option<AbsRange> {
    let mut fields = [0i32; 6];
    // _IOR('E', 0x40 + code, struct input_absinfo): direction 2 (read) in the
    // top two bits, then size, then type, then number.
    let request = (2u64 << 30)
        | ((ABSINFO_BYTES as u64) << 16)
        | (u64::from(b'E') << 8)
        | u64::from(0x40 + code);
    // SAFETY: `fields` is a live, correctly sized and aligned `[i32; 6]`,
    // matching `struct input_absinfo`'s six `__s32` fields exactly, and the
    // fd is owned by `file` for the duration of the call. The kernel writes
    // at most that many bytes for this request.
    let result = unsafe { libc::ioctl(file.as_raw_fd(), request as _, fields.as_mut_ptr()) };
    if result < 0 {
        return None;
    }
    Some(AbsRange {
        minimum: fields[1],
        maximum: fields[2],
    })
}

/// Reads one of sysfs's capability bitmasks and tests a single bit.
///
/// The file is a space-separated list of 64-bit hex words, **most
/// significant first**, so the words are reversed before indexing.
fn capability_bit_set(path: &Path, bit: usize) -> bool {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return false;
    };
    let mut words: Vec<u64> = contents
        .split_whitespace()
        .map(|word| u64::from_str_radix(word, 16).unwrap_or(0))
        .collect();
    words.reverse();
    let Some(word) = words.get(bit / 64) else {
        return false;
    };
    (word >> (bit % 64)) & 1 == 1
}

/// Whether an `/dev/input/eventN` node is a gamepad.
///
/// Tests for `BTN_SOUTH`, which is what separates a pad from the *other*
/// nodes a modern controller publishes. A DualSense alone exposes four:
/// the gamepad, its motion sensors, its touchpad, and a headset jack. A scan
/// that opened every node would happily treat the touchpad as a second pad.
fn is_gamepad(event_name: &str) -> bool {
    let capabilities = PathBuf::from("/sys/class/input")
        .join(event_name)
        .join("device/capabilities/key");
    capability_bit_set(&capabilities, BTN_SOUTH as usize)
}

/// One pad's accumulated state, and the whole decode of evdev's event
/// stream into it.
///
/// Deliberately owns no file descriptor. Everything about *interpreting* a
/// pad — which code is which button, how an axis normalizes, when a press
/// edge appears and expires — is decided here, so it can be driven by
/// synthetic `input_event` bytes in a test rather than only by a controller
/// somebody has to be holding.
#[derive(Default)]
struct PadState {
    ranges: HashMap<u16, AbsRange>,
    left_stick: PointF,
    right_stick: PointF,
    left_trigger: f32,
    right_trigger: f32,
    held: Vec<GamepadButton>,
    pressed: Vec<GamepadButton>,
}

struct GamepadDevice {
    id: u32,
    path: PathBuf,
    file: File,
    state: PadState,
}

impl GamepadDevice {
    fn open(id: u32, path: PathBuf) -> Option<Self> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&path)
            .ok()?;
        let mut ranges = HashMap::new();
        for code in [ABS_X, ABS_Y, ABS_Z, ABS_RX, ABS_RY, ABS_RZ] {
            if let Some(range) = read_abs_range(&file, code) {
                ranges.insert(code, range);
            }
        }
        Some(Self {
            id,
            path,
            file,
            state: PadState {
                ranges,
                ..PadState::default()
            },
        })
    }

    /// Drains everything the kernel has queued. Returns `false` when the
    /// device has gone away and should be dropped.
    fn drain(&mut self, event_bytes: usize, value_offset: usize) -> bool {
        let mut buffer = [0u8; 512];
        loop {
            match self.file.read(&mut buffer) {
                Ok(0) => return true,
                Ok(read) => self
                    .state
                    .apply_encoded(&buffer[..read], event_bytes, value_offset),
                Err(err) if err.kind() == ErrorKind::WouldBlock => return true,
                Err(err) if err.kind() == ErrorKind::Interrupted => {}
                // ENODEV on unplug, and anything else is equally unrecoverable
                // for a device node that has stopped answering.
                Err(_) => return false,
            }
        }
    }
}

impl PadState {
    fn hold(&mut self, button: GamepadButton) {
        if !self.held.contains(&button) {
            self.held.push(button);
        }
        if !self.pressed.contains(&button) {
            self.pressed.push(button);
        }
    }

    fn release(&mut self, button: GamepadButton) {
        self.held.retain(|held| *held != button);
    }

    /// Translates a hat axis change into d-pad press/release edges, so a pad
    /// reporting its d-pad as an axis produces the same button events as one
    /// reporting it as four keys.
    fn apply_hat(&mut self, negative: GamepadButton, positive: GamepadButton, value: i32) {
        for (button, active) in [(negative, value < 0), (positive, value > 0)] {
            if active {
                self.hold(button);
            } else {
                self.release(button);
            }
        }
    }

    fn apply_event(&mut self, kind: u16, code: u16, value: i32) {
        match kind {
            EV_KEY => {
                let Some(button) = button_for_code(code) else {
                    return;
                };
                // value 2 is autorepeat, which a gamepad should not synthesize
                // into a fresh press edge.
                match value {
                    0 => self.release(button),
                    1 => self.hold(button),
                    _ => {}
                }
            }
            EV_ABS => match code {
                ABS_X | ABS_Y | ABS_RX | ABS_RY => {
                    let Some(range) = self.ranges.get(&code).copied() else {
                        return;
                    };
                    let signed = range.to_signed(value);
                    match code {
                        ABS_X => self.left_stick.x = signed,
                        ABS_Y => self.left_stick.y = signed,
                        ABS_RX => self.right_stick.x = signed,
                        _ => self.right_stick.y = signed,
                    }
                }
                ABS_Z | ABS_RZ => {
                    let Some(range) = self.ranges.get(&code).copied() else {
                        return;
                    };
                    let unsigned = range.to_unsigned(value);
                    if code == ABS_Z {
                        self.left_trigger = unsigned;
                    } else {
                        self.right_trigger = unsigned;
                    }
                }
                ABS_HAT0X => {
                    self.apply_hat(GamepadButton::DPadLeft, GamepadButton::DPadRight, value);
                }
                ABS_HAT0Y => {
                    self.apply_hat(GamepadButton::DPadUp, GamepadButton::DPadDown, value);
                }
                _ => {}
            },
            _ => {}
        }
    }

    /// Decodes a buffer of `input_event` records and applies each one.
    ///
    /// The records are byte-sliced rather than transmuted: the layout is
    /// `struct timeval` followed by `__u16 type`, `__u16 code`, `__s32
    /// value`, and `value_offset` is where that trailing 8-byte tail
    /// begins. A trailing partial record cannot happen — the kernel writes
    /// whole events — and `chunks_exact` drops one if it ever did.
    fn apply_encoded(&mut self, buffer: &[u8], event_bytes: usize, value_offset: usize) {
        for chunk in buffer.chunks_exact(event_bytes) {
            let kind = u16::from_ne_bytes([chunk[value_offset], chunk[value_offset + 1]]);
            let code = u16::from_ne_bytes([chunk[value_offset + 2], chunk[value_offset + 3]]);
            let value = i32::from_ne_bytes([
                chunk[value_offset + 4],
                chunk[value_offset + 5],
                chunk[value_offset + 6],
                chunk[value_offset + 7],
            ]);
            self.apply_event(kind, code, value);
        }
    }

    fn snapshot(&self, id: u32) -> GamepadSnapshot {
        GamepadSnapshot {
            id,
            connected: true,
            left_stick: GamepadStick {
                raw: self.left_stick,
            },
            right_stick: GamepadStick {
                raw: self.right_stick,
            },
            left_trigger: GamepadTrigger {
                raw: self.left_trigger,
            },
            right_trigger: GamepadTrigger {
                raw: self.right_trigger,
            },
            buttons_down: self.held.clone(),
            buttons_pressed: self.pressed.clone(),
        }
    }
}

pub(crate) struct GamepadTracker {
    devices: Vec<GamepadDevice>,
    next_id: u32,
    last_scan: Option<Instant>,
    /// Sized from `struct timeval`, which is two `long`s — 16 bytes on a
    /// 64-bit target and 8 on a 32-bit one. Computed rather than assumed so
    /// the same code is correct on armv7 as on aarch64.
    event_bytes: usize,
}

impl GamepadTracker {
    pub(crate) fn new() -> Self {
        Self {
            devices: Vec::new(),
            next_id: 0,
            last_scan: None,
            event_bytes: std::mem::size_of::<libc::timeval>() + 8,
        }
    }

    fn scan(&mut self) {
        let Ok(entries) = std::fs::read_dir("/dev/input") else {
            return;
        };
        let mut found: Vec<(String, PathBuf)> = entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let name = entry.file_name().into_string().ok()?;
                name.starts_with("event").then(|| (name, entry.path()))
            })
            .collect();
        // Stable order, so ids are assigned predictably rather than in
        // whatever order the directory happens to enumerate.
        found.sort_by(|left, right| left.0.cmp(&right.0));

        for (name, path) in found {
            if self.devices.iter().any(|device| device.path == path) {
                continue;
            }
            if !is_gamepad(&name) {
                continue;
            }
            if let Some(device) = GamepadDevice::open(self.next_id, path) {
                self.next_id = self.next_id.wrapping_add(1);
                self.devices.push(device);
            }
        }
    }

    /// One frame's worth of gamepad state, newly connected pads included and
    /// newly departed ones reported exactly once as `cleared()`.
    pub(crate) fn poll(&mut self) -> Vec<GamepadSnapshot> {
        let now = Instant::now();
        if self
            .last_scan
            .is_none_or(|last| now.duration_since(last) >= RESCAN_INTERVAL)
        {
            self.last_scan = Some(now);
            self.scan();
        }

        let event_bytes = self.event_bytes;
        let value_offset = event_bytes - 8;
        let mut departed = Vec::new();
        for device in &mut self.devices {
            // Press edges expire on the poll that follows the one which
            // reported them. Since `capture_frame` is the only caller, that
            // makes an edge belong to exactly one read of the input — the
            // same lifetime `key_events` has.
            device.state.pressed.clear();
            if !device.drain(event_bytes, value_offset) {
                departed.push(device.id);
            }
        }

        let mut snapshots: Vec<GamepadSnapshot> = self
            .devices
            .iter()
            .filter(|device| !departed.contains(&device.id))
            .map(|device| device.state.snapshot(device.id))
            .collect();
        self.devices.retain(|device| !departed.contains(&device.id));
        snapshots.extend(departed.into_iter().map(GamepadSnapshot::cleared));
        snapshots
    }
}

#[cfg(test)]
mod tests {
    use super::{
        button_for_code, capability_bit_set, AbsRange, PadState, ABS_HAT0X, ABS_HAT0Y, ABS_RZ,
        ABS_Y, BTN_SOUTH, BTN_TR, EV_ABS, EV_KEY,
    };
    use loadngo_host_core::GamepadButton;

    /// Byte layout of one `input_event` on this target, matching what
    /// `GamepadTracker` computes at runtime.
    fn event_bytes() -> usize {
        std::mem::size_of::<libc::timeval>() + 8
    }

    /// Encodes events exactly as the kernel writes them, so the tests below
    /// exercise the real byte-slicing and not a shortcut around it.
    fn encode(events: &[(u16, u16, i32)]) -> Vec<u8> {
        let size = event_bytes();
        let mut bytes = Vec::with_capacity(size * events.len());
        for (kind, code, value) in events {
            // The timeval is ignored by the decoder; fill it with a value
            // that is not zero so a decoder reading the wrong offset would
            // produce obvious nonsense rather than a plausible zero.
            bytes.extend(std::iter::repeat_n(0xa5, size - 8));
            bytes.extend_from_slice(&kind.to_ne_bytes());
            bytes.extend_from_slice(&code.to_ne_bytes());
            bytes.extend_from_slice(&value.to_ne_bytes());
        }
        bytes
    }

    /// A pad whose axes report unsigned bytes, like a DualSense.
    fn byte_ranged_pad() -> PadState {
        let mut pad = PadState::default();
        for code in [0x00, 0x01, 0x02, 0x03, 0x04, 0x05] {
            pad.ranges.insert(
                code,
                AbsRange {
                    minimum: 0,
                    maximum: 255,
                },
            );
        }
        pad
    }

    fn feed(pad: &mut PadState, events: &[(u16, u16, i32)]) {
        let size = event_bytes();
        pad.apply_encoded(&encode(events), size, size - 8);
    }

    #[test]
    fn a_real_event_record_decodes_into_a_button_press_that_expires_after_one_poll() {
        let mut pad = byte_ranged_pad();
        feed(&mut pad, &[(EV_KEY, BTN_SOUTH, 1)]);
        let snapshot = pad.snapshot(0);
        assert_eq!(snapshot.buttons_down, vec![GamepadButton::South]);
        assert_eq!(snapshot.buttons_pressed, vec![GamepadButton::South]);

        // What `poll` does between reads: the edge goes, the hold stays.
        pad.pressed.clear();
        let snapshot = pad.snapshot(0);
        assert_eq!(snapshot.buttons_down, vec![GamepadButton::South]);
        assert!(snapshot.buttons_pressed.is_empty());

        feed(&mut pad, &[(EV_KEY, BTN_SOUTH, 0)]);
        assert!(pad.snapshot(0).buttons_down.is_empty());
    }

    #[test]
    fn autorepeat_does_not_manufacture_a_second_press_edge() {
        let mut pad = byte_ranged_pad();
        feed(&mut pad, &[(EV_KEY, BTN_TR, 1)]);
        pad.pressed.clear();
        // value 2 is the kernel's autorepeat.
        feed(&mut pad, &[(EV_KEY, BTN_TR, 2)]);
        let snapshot = pad.snapshot(0);
        assert_eq!(snapshot.buttons_down, vec![GamepadButton::RightShoulder]);
        assert!(snapshot.buttons_pressed.is_empty());
    }

    #[test]
    fn pushing_the_stick_up_reports_negative_y() {
        // The contract `GamepadStick` documents, and the one thing a
        // backend can get exactly backwards while looking fine. evdev
        // reports up as the *low* end of ABS_Y, so no negation is correct.
        let mut pad = byte_ranged_pad();
        feed(&mut pad, &[(EV_ABS, ABS_Y, 0)]);
        assert!(pad.snapshot(0).left_stick.raw.y < -0.9);

        feed(&mut pad, &[(EV_ABS, ABS_Y, 255)]);
        assert!(pad.snapshot(0).left_stick.raw.y > 0.9);
    }

    #[test]
    fn a_hat_axis_produces_the_same_dpad_buttons_as_a_pad_with_dpad_keys() {
        let mut pad = byte_ranged_pad();
        feed(&mut pad, &[(EV_ABS, ABS_HAT0Y, -1)]);
        assert_eq!(pad.snapshot(0).buttons_down, vec![GamepadButton::DPadUp]);

        // Returning to centre releases it without leaving the opposite
        // direction stuck on.
        feed(&mut pad, &[(EV_ABS, ABS_HAT0Y, 0)]);
        assert!(pad.snapshot(0).buttons_down.is_empty());

        feed(&mut pad, &[(EV_ABS, ABS_HAT0X, 1)]);
        assert_eq!(pad.snapshot(0).buttons_down, vec![GamepadButton::DPadRight]);
    }

    #[test]
    fn a_trigger_sweeps_continuously_and_is_never_reported_as_a_button() {
        let mut pad = byte_ranged_pad();
        feed(&mut pad, &[(EV_ABS, ABS_RZ, 128)]);
        let snapshot = pad.snapshot(0);
        assert!((snapshot.right_trigger.raw - 0.5).abs() < 0.01);
        assert!(snapshot.buttons_down.is_empty());
    }

    #[test]
    fn several_events_in_one_read_are_all_applied() {
        // The kernel delivers a burst per read; decoding only the first
        // would look like a laggy pad rather than a broken one.
        let mut pad = byte_ranged_pad();
        feed(
            &mut pad,
            &[
                (EV_KEY, BTN_SOUTH, 1),
                (EV_ABS, ABS_Y, 0),
                (EV_ABS, ABS_RZ, 255),
                (0x00, 0x00, 0), // EV_SYN, which carries no state
            ],
        );
        let snapshot = pad.snapshot(0);
        assert_eq!(snapshot.buttons_pressed, vec![GamepadButton::South]);
        assert!(snapshot.left_stick.raw.y < -0.9);
        assert!((snapshot.right_trigger.raw - 1.0).abs() < 0.01);
    }

    #[test]
    fn sticks_normalize_to_signed_and_triggers_to_unsigned() {
        // A DualSense reports its sticks and triggers as unsigned bytes.
        let byte = AbsRange {
            minimum: 0,
            maximum: 255,
        };
        assert!((byte.to_signed(0) + 1.0).abs() < 0.01);
        assert!(byte.to_signed(128).abs() < 0.01);
        assert!((byte.to_signed(255) - 1.0).abs() < 0.01);
        assert!(byte.to_unsigned(0).abs() < 0.01);
        assert!((byte.to_unsigned(255) - 1.0).abs() < 0.01);

        // An Xbox pad reports signed 16-bit sticks instead; the same mapping
        // has to hold without a per-driver special case.
        let signed = AbsRange {
            minimum: -32768,
            maximum: 32767,
        };
        assert!((signed.to_signed(-32768) + 1.0).abs() < 0.01);
        assert!(signed.to_signed(0).abs() < 0.01);
        assert!((signed.to_signed(32767) - 1.0).abs() < 0.01);
    }

    #[test]
    fn a_degenerate_range_reads_as_centred_rather_than_dividing_by_zero() {
        let broken = AbsRange {
            minimum: 7,
            maximum: 7,
        };
        assert!(broken.to_signed(7).abs() < f32::EPSILON);
        assert!(broken.to_unsigned(7).abs() < f32::EPSILON);
    }

    #[test]
    fn triggers_are_not_also_published_as_buttons() {
        // BTN_TL2/BTN_TR2 are the digital shadow of ABS_Z/ABS_RZ.
        assert!(button_for_code(0x138).is_none());
        assert!(button_for_code(0x139).is_none());
        assert_eq!(button_for_code(0x130), Some(GamepadButton::South));
        assert_eq!(button_for_code(0x134), Some(GamepadButton::West));
    }

    #[test]
    fn capability_words_are_read_most_significant_first() {
        // The real bitmask a DualSense publishes for `capabilities/key`:
        // five words, most significant first, with the gamepad buttons in
        // the highest one. BTN_SOUTH is bit 304, i.e. bit 48 of word 4.
        let directory = std::env::temp_dir().join("loadngo-gamepad-caps-test");
        std::fs::create_dir_all(&directory).expect("temp dir");
        let path = directory.join("key");
        std::fs::write(&path, "7fdb000000000000 0 0 0 0\n").expect("write caps");
        assert!(capability_bit_set(&path, BTN_SOUTH as usize));
        // BTN_C (0x132) is genuinely absent from that mask.
        assert!(!capability_bit_set(&path, 0x132));
        // A node with no key capabilities at all — the DualSense's headset
        // jack publishes exactly this — must not look like a gamepad.
        std::fs::write(&path, "0\n").expect("write caps");
        assert!(!capability_bit_set(&path, BTN_SOUTH as usize));
        let _ = std::fs::remove_file(&path);
    }
}
