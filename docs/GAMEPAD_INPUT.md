# Gamepad/Controller Input

## Status and purpose

Status: **real macOS backend shipped 2026-09-07**, built against an actual DualSense (PS5 controller) connected via USB-C to a Mac Mini — the first real backend for this subsystem. **Linux `evdev` backend shipped 2026-09-09** (`host-desktop/src/linux_gamepad.rs`), built against the same model of controller plugged into dolores, a Raspberry Pi 5 — see "Linux backend" below. Written 2026-09-05, revised same-day after a type scaffold committed earlier in the session was reverted on review, then implemented for real two days later once a physical controller was actually available to build against. This doc resolves the open questions [DESKTOP_PLATFORM_ROADMAP.md](DESKTOP_PLATFORM_ROADMAP.md)'s "Finding 3" raised on 2026-08-30 (crate placement, per-platform backend strategy, hand-roll vs. crate dependency, raw-event vs. normalized shape) and supersedes that finding as the living design doc for this subsystem going forward. It also carries forward the one concrete design consequence from [INPUT_PHILOSOPHY.md](INPUT_PHILOSOPHY.md): prefer continuous/analog representations over booleans wherever a signal is naturally continuous.

Before the 2026-09-05 design pass, the only joystick-adjacent code anywhere in `loadngo` was `touch/src/joystick.rs`'s `VirtualJoystick` — an on-screen *virtual* stick for touch UI, not a physical controller input path. As of 2026-09-07, `host-core::{GamepadButton, GamepadStick, GamepadTrigger, GamepadSnapshot}` and a real `GCController`-polling backend in `host-desktop/src/macos.rs` exist and are wired into `sng-roguelite`'s gameplay input, title screen, reward draft, and terminal restart/next-seed controls (`sng-roguelite/crates/game-app/src/lib.rs`). Linux followed on 2026-09-09; Windows is still design-only — see "Platform priority and phasing" below, unchanged in substance by this revision beyond macOS now being done rather than merely first in line.

**Same-day revision note**: the first version of this doc committed a `host-core/src/gamepad.rs` type scaffold (`GamepadSnapshot`, `GamepadButton`, deadzone helpers, a `HostFrame.gamepads` field) alongside the design writeup. On review this was reverted — it had no backend and no caller anywhere in the workspace, which is exactly the kind of speculative build-ahead-of-need this doc itself argues against elsewhere (see the `FormFactor` discussion below). The type shapes below are kept as an illustrative sketch for whoever eventually builds the first real backend, not as committed code, and two real design gaps the scaffold glossed over — where gamepad state should live, and how input-source transitions work — are corrected and addressed for the first time in this revision.

## Decision: crate placement

If and when this is built, it belongs in `host-core` (`loadngo-host-core`), not a new crate — gamepad state is a cross-platform contract exactly like touch and keyboard state, the same category `host-core` already exists to hold. A separate crate would just be an indirection with no platform-specific code inside it yet to isolate.

## Decision: hand-rolled per-platform backends

No `gilrs`, `sdl2`, or other cross-platform gamepad crate dependency. Consistent with how `loadngo` already handles audio (`host-desktop/src/audio_mixer.rs`), windowing (per-OS modules in `host-desktop`), and async I/O (`loadngo-proactor`'s per-OS `IoPort` implementations): own platform integration directly rather than depend on a black-box crate. `objc2-game-controller` (added 2026-09-07 for the macOS backend) doesn't cut against this: it's a raw generated binding to Apple's `GameController` framework, the same category as the `objc2-foundation`/`objc2-ui-kit` bindings `host-desktop` already depends on for `ios.rs`, not a cross-platform gamepad abstraction like `gilrs` — it normalizes nothing across OSes, it just makes the Objective-C API callable from Rust.

- **macOS: done (2026-09-07).** The `GameController` framework via `objc2-game-controller`, polled once per frame from `host-desktop/src/macos.rs`'s `capture_frame()` — `GamepadTracker::poll()` calls `GCController::controllers()` and reads each pad's `GCExtendedGamepad`, rather than registering `GCControllerDidConnectNotification`/`GCControllerDidDisconnectNotification` delegates. Poll-based was chosen because it drops straight into the existing per-frame `capture_frame()` model with no separate callback-to-frame synchronization to maintain, at the cost of connect/disconnect being detected on the next poll rather than instantly. `GCExtendedGamepad` already normalizes DualShock/DualSense/Xbox face-button layouts to one positional `buttonA`/`buttonB`/`buttonX`/`buttonY` shape, which maps directly onto `GamepadButton::South/East/West/North` with no per-brand mapping table needed on this platform. Verified against a real DualSense connected via USB-C to a Mac Mini. **Real bug found and fixed the same day this playtest surfaced it**: `GCControllerDirectionPad`'s `yAxis` reports the joystick's native up-is-positive convention, but every other y coordinate `InputSnapshot` exposes is y-down (`event_point_in_view`'s AppKit mouse-y flip is the existing precedent) — both thumbsticks were inverted (up moved down, down moved up) until `read_extended_gamepad` started negating `yAxis().value()` before storing it in `GamepadStick::raw`.
- **Windows**: `XInput` first — it covers Xbox controllers and most third-party pads with the least integration cost — with `DirectInput` as a later fallback for older or non-XInput devices. Still design-only.
- **Linux**: `evdev`/`udev` directly, no `gilrs` dependency, consistent with the project's existing preference for owning raw platform integration (mirrors how `host-desktop/src/netbsd_wsdesktop.rs`/`netbsd_wsdisplay.rs` already talk to `wsdisplay`/`wsmouse` device nodes directly rather than through an abstraction crate). **Done 2026-09-09**, `evdev` without `udev` — see below.

## Decision: normalized cross-pad shape, at the same level as mouse/keyboard/touch

A future `GamepadSnapshot` should normalize face buttons, sticks, and triggers uniformly across Xbox, PlayStation, and Switch Pro controller layouts, consistent with every other `loadngo` input surface (`UiEvent`, touch) already normalizing raw platform input rather than exposing a raw per-model event stream. Buttons should be named by position (`South`/`East`/`West`/`North`), not by any one brand's printed label, so the same value means the same physical button no matter which pad is plugged in.

Where it lives matters and the first version of this doc got it wrong: it proposed `gamepads: Vec<GamepadSnapshot>` as a field on `HostFrame`, a sibling to `input: InputSnapshot`, reasoned as "gamepads are inherently multi-device, `InputSnapshot` is single-seat." That reasoning doesn't hold up — `InputSnapshot.touches` is already `[Option<TouchPoint>; 8]`, handling up to eight simultaneous inputs *inside* `InputSnapshot`. Multiplicity isn't a reason to split a modality out. Mouse, keyboard, and touch are all read through the one `InputSnapshot` a game already consumes every frame; gamepad is a peer input modality, not an ancillary extra, and belongs in the same place: a `gamepads: Vec<GamepadSnapshot>` field *on* `InputSnapshot`, alongside `touches`, so `capture_frame()`'s `HostFrame.input` remains the single normalized surface for everything a player did this frame, regardless of device.

## Shape (implemented in `host-core`, 2026-09-07)

```rust
pub enum GamepadButton {
    South, East, West, North,       // positional face buttons, not brand names
    LeftShoulder, RightShoulder,
    LeftStick, RightStick,          // stick clicks (L3/R3)
    DPadUp, DPadDown, DPadLeft, DPadRight,
    Start, Select, Guide,           // Guide = Xbox/PS/Steam system button
}

pub struct GamepadStick { pub raw: PointF }     // -1.0..=1.0 per axis, pre-deadzone
pub struct GamepadTrigger { pub raw: f32 }      // 0.0..=1.0, pre-deadzone

pub struct GamepadSnapshot {
    pub id: u32,
    pub connected: bool,
    pub left_stick: GamepadStick,
    pub right_stick: GamepadStick,
    pub left_trigger: GamepadTrigger,
    pub right_trigger: GamepadTrigger,
    pub buttons_down: Vec<GamepadButton>,
    pub buttons_pressed: Vec<GamepadButton>,
}

// on InputSnapshot, alongside `touches`:
// pub gamepads: Vec<GamepadSnapshot>,
```

This is real, in `host-core/src/lib.rs` — not the illustrative sketch the pre-implementation revision of this doc carried. `GamepadSnapshot::cleared(id)` and `InputSnapshot::primary_gamepad()` were added alongside it (the latter because `sng-roguelite` needed a "the" gamepad for a single-player game with no player-assignment scheme — see the non-goal below, still unaddressed). `id` on the macOS backend is assigned from `GCController` pointer identity in `host-desktop/src/macos.rs`'s `GamepadTracker`, first-seen order, not a real device-unique identifier (`objc2-game-controller`'s `GCDevice` binding doesn't expose one) — stable for one connection, not guaranteed stable across a disconnect/reconnect of the same physical pad.

Shape reasoning, now backend-tested rather than merely argued for:

- **Triggers are analog only** — never duplicated as a boolean crossing an internal threshold. This is the one concrete place [INPUT_PHILOSOPHY.md](INPUT_PHILOSOPHY.md)'s "prefer continuous over boolean" consequence lands in code.
- **Deadzone is a caller-side helper** (`GamepadStick::with_deadzone`), not baked into raw storage, mirroring `touch/src/joystick.rs`'s `VirtualJoystick`: the same radial, magnitude-based deadzone shape (zero within the deadzone, unscaled pass-through above it, clamped to unit magnitude beyond `1.0`), with the deadzone value supplied by the caller rather than hardcoded. `sng-roguelite` calls it with `0.15`, matching its virtual-stick deadzone exactly (`game-app/src/lib.rs`'s `GAMEPAD_STICK_DEADZONE`).
- **`buttons_down` (held) vs. `buttons_pressed` (this-frame edge)** mirrors the existing `keys_down`/`key_events` split on `InputSnapshot`. On the macOS backend, `buttons_pressed` is computed by diffing each poll's `buttons_down` against the previous poll's, since `GCController` polling has no separate edge-event stream of its own to forward.
- **`Vec<GamepadSnapshot>`, not a fixed-size array like `touches`** — the number of realistic simultaneously-connected pads is small but not as fixed a bound as 8 touches on one screen, and there's still no established convention for what that bound would be.

## Open problem: input-source transitions

Two distinct problems, one decided and now implemented, one genuinely open:

**Stale state on disconnect (decided, and implemented on macOS 2026-09-07).** A gamepad going offline mid-hold — dead batteries, Bluetooth dropout, physically unplugged — while a button is pressed or a stick is deflected must not leave that state stuck. This is not hypothetical: `host-desktop/src/ios.rs`'s `capture_frame` carries a real fix, with a comment explaining exactly this class of bug for touch — a thumbstick that kept firing at its last dragged position after the finger lifted, because the previous code decayed `pending_input` *before* snapshotting it, so an `Ended` touch phase was wiped to `None` before any caller ever observed it. `GamepadTracker::poll()` in `host-desktop/src/macos.rs` carries the equivalent discipline in the other direction: a pad that drops out of `GCController::controllers()` since the last poll gets exactly one `GamepadSnapshot::cleared(id)` — every button/stick/trigger forced to neutral, `connected: false` — on the poll where its disappearance is first observed, then is dropped from tracking entirely (no more snapshots for that `id` after that one frame). Any future Windows/Linux backend needs the same discipline.

**Which input method is "active" — resolved and implemented 2026-09-07.** Games want to know which input method to show UI prompts for — "Press A" vs. "Click" vs. "Press Space" — and `FormFactor` (`touch/src/form_factor.rs`) can't answer it: its one-way latch (`Desktop → MobileTouch`, never demoted) fits touch, which a device either never or permanently proves capable of, but doesn't fit a desktop player who legitimately alternates between a mouse and a controller within one sitting. `touch/src/input_method.rs`'s `InputMethod` (`KeyboardMouse` | `Gamepad`) is the separate, bidirectional type this called for: `InputMethod::update` re-evaluates every frame using "whichever source produced real input most recently wins" — exactly the model this doc predicted — checking `keys_down`/`key_events`/mouse activity against `gamepads`' held buttons/deflected sticks/pressed triggers, and leaving the value unchanged on a quiet frame or a genuinely simultaneous one (both sources active at once) to avoid flicker. Only meaningful when `FormFactor` is `Desktop`.

This was motivated by a real bug, not spec work: `sng-roguelite`'s title screen, reward-cache prompt, reward-draft hint, and gameplay HUD all kept showing "Press Space"/keyboard vocabulary to a player who had been playing entirely on a DualSense for several minutes — exactly the failure mode this section predicted before any backend existed. `sng-roguelite`'s `TouchControls` now owns an `InputMethod` alongside its `FormFactor`, updated the same way, and every `FormFactor::Desktop` prompt branch became a `(FormFactor, InputMethod)` match instead.

## Menu navigation moved into shared `loadngo` UI (2026-09-07)

The first pass wired gamepad menu handling per screen inside
`sng-roguelite` — a focus index and bespoke match arms on each screen.
Playtesting made the cost obvious: only the d-pad selected reward cards
(the left stick did nothing), the run-summary screen showed no focus
indicator at all, and two of its four buttons were unreachable by gamepad.

That capability now lives in `loadngo` and is documented in
[WIDGET_FRAMEWORK.md](WIDGET_FRAMEWORK.md)'s "Focus Navigation" section:
`ui_core::FocusRing` decides which widget holds focus and moves it
spatially, and `loadngo_touch::NavRepeat` turns a d-pad or thumbstick into
repeat-timed `NavDirection` steps. Neither required changing any existing
widget — focus, `Key::Enter` activation, and slider `input_consumed` were
already part of the widget contract.

`sng-roguelite` consumes it on the reward draft, run summary, achievements,
and sound-settings screens. It still paints its own visuals (see that doc's
theming gap), reading `focused` to draw its existing highlight treatment, so
adopting navigation changed no existing appearance.

## Linux backend (2026-09-09)

`host-desktop/src/linux_gamepad.rs`, polled once per frame from
`advance_frame_clock` the same way macOS polls from its frame publish. The
decisions worth keeping:

**evdev, not joydev.** Both device families are present for any pad
(`/dev/input/event*` and `/dev/input/js*`). joydev hands out pre-normalized
axes for free but reports *opaque button indices* whose meaning varies by
driver, which would mean shipping and maintaining a per-controller quirk
table to answer "which button is South?". evdev reports the kernel's
semantic codes — `BTN_SOUTH`, `ABS_RX`, `ABS_HAT0Y` — which are already
position-named in exactly the way `GamepadButton` is, so the mapping is a
flat lookup with nothing device-specific in it. The price is one
`EVIOCGABS` ioctl per axis to learn its range, which is the only `unsafe`
in the module; everything else is ordinary file and sysfs reads.

**A pad is not a device node.** A DualSense alone publishes four
`/dev/input/event*` nodes: the gamepad, its motion sensors, its touchpad,
and a headset jack. A scan that opened every node would report a touchpad
as a second gamepad. Discovery instead reads
`/sys/class/input/eventN/device/capabilities/key` and tests for `BTN_SOUTH`
— the bit that actually separates a pad from its siblings. That file is a
list of 64-bit hex words printed **most significant first**, so the words
are reversed before indexing; a unit test pins that against the real mask a
DualSense publishes (`7fdb000000000000 0 0 0 0`), because getting the word
order backwards silently finds nothing rather than failing loudly.

**No y-flip here, deliberately.** `GamepadStick` documents up as *negative*
y. macOS must negate because GameController reports up as `+1`; evdev
already reports up as the lower value on `ABS_Y`/`ABS_RY`. Adding a
negation "to match macOS" would invert every stick on Linux — the
symmetry to preserve is the contract, not the code.

**Hotplug by polling, no udev.** evdev has no hotplug notification of its
own, and taking a `udev` dependency to learn about a directory that can be
listed in microseconds is not a trade worth making. `/dev/input` is
rescanned once a second; a device that stops answering is dropped after
exactly one `GamepadSnapshot::cleared(id)`, which is the same
stale-state-on-disconnect discipline macOS follows above.

**Triggers are not published twice.** `BTN_TL2`/`BTN_TR2` — the digital
shadow of the analog triggers — are deliberately left unmapped, since
`ABS_Z`/`ABS_RZ` already carry the continuous signal, per
[INPUT_PHILOSOPHY.md](INPUT_PHILOSOPHY.md).

**Access.** `/dev/input/event*` is `root:input` mode `0660`, so the user
running the game must be in the `input` group. No elevation, no udev rule
is shipped; a pad simply does not appear for a user outside that group.

### A press edge belongs to the read, not to the frame

**Found the hard way on 2026-09-09, and worth stating as a rule.** Every
transient on `InputSnapshot` — mouse wheel and click edges, `key_events`,
`typed_text` — is consumed by `capture_frame()` on both backends. Read a
frame twice and the second read sees no keystrokes, by design: an edge
belongs to the *read*, not to the published frame.

The first Linux gamepad backend polled in `advance_frame_clock` instead, so
`buttons_pressed` was the one transient that survived being read. That is
not a defensible variation, it is an inconsistency, and it cost real time:

`sng-roguelite` opens its achievements screen with South and also closes it
with South, on a loop that `continue`s to the next screen *without awaiting
a frame*. On macOS the second `capture_frame()` had already consumed the
edge, so the screen stayed open and nobody knew there was a latent bug. On
Linux the same edge opened and closed the screen forever inside one
`run_until_stalled` — the window froze at ~70% CPU with keyboard and mouse
dead too, since the event loop never got back out. Not a flicker; a hard
live-lock, found by the first person to play it with a pad.

Both ends are now fixed, and both fixes were worth making:

- **Linux polls in `capture_frame`**, where macOS already did. Beyond the
  consistency, polling per read is what makes an edge impossible to *miss*:
  polling per published frame loses a press whenever a caller skips a frame
  it never read.
- **`sng-roguelite` arms South only after seeing it released**, the same
  release-before-arm discipline `FocusRing` and the reward draft already
  use. Any screen that can be both entered and left by the same button
  needs this, on every platform — the host consuming the edge makes the
  live-lock impossible, but it does not make "one press did two things"
  correct.

The general rule for a new backend: **poll the pad as part of building the
frame the caller asked for, and consume press edges there** — the same
place, and at the same moment, as every other transient.

### Verifying a backend: `gamepad_harness`

`host-desktop/src/bin/gamepad_harness.rs` shows every connected pad's live
state — sticks as dots in their travel circles with raw and deadzone-shaped
numbers, triggers as bars, one chip per button lighting green while held
and amber on the press edge, plus a short log of press edges (a one-frame
edge is otherwise impossible to read). It uses only the public host API, so
it runs on any backend that fills `InputSnapshot::gamepads`.

This exists because gamepad state is the one input modality with no visible
trace on screen by default: an inverted axis, a mismapped button, or a
backend returning nothing at all are indistinguishable from "the game
ignored me". The unit tests can pin the arithmetic and the constants; only
hardware can confirm the pad in your hands is understood.

Its notes panel draws each line as its own single-line label rather than as
one `TextBlockModel`. That is a workaround, not a preference: on the Linux
backend today a multi-line text block drops its leading lines, reproducible
in the untouched `text_input_harness`, whose "Purpose" paragraph is missing
on Linux while rendering fine elsewhere. A harness whose own instructions
render wrong is worse than no harness, so it avoids the path that is
currently broken. Revert it to a text block once that defect is fixed.

## Platform priority and phasing

**Tier 1 — desktop (macOS, Windows, Linux), highest priority.** The three platforms named above, each with its own named API. **macOS done (2026-09-07)**, **Linux done (2026-09-09)**; Windows still design-only.

**Tier 2 — mobile with a physical controller (clip-on or wireless).** `sng-roguelite`, `sng-rusty`, and `sng-zhoenus` already ship Android builds today (some also iOS), so this tier rides app/runtime infrastructure that already exists, unlike the access-gated tiers below. A physical controller reaches a phone or tablet two ways: a clip-on adapter that clamps directly onto the device (Razer Kishi, Backbone One, GameSir X2/X3, 8BitDo, PowerA MOGA, and similar), or a standalone pad (an Xbox or PlayStation controller, for instance) paired over Bluetooth or USB independently of any clip. Both arrive at the OS the same way, as a standard controller recognized by Android's `InputDevice` gamepad APIs or iOS's `GameController` framework — the same two backends the now-superseded Finding 3 already named as mobile's counterpart to desktop, just never phased in until now. No change to the normalized shape above would be needed; a future `GamepadSnapshot` fits a clip-on or paired pad the same way it fits a desktop one.

Worth naming even though it doesn't change today's design: a clip-on adapter shifts the phone or tablet from being the primary input surface (touch) to primarily a display-and-compute unit, with the controller doing the actual input — the inverse of what `touch/src/form_factor.rs`'s `FormFactor::MobileTouch` currently assumes for every mobile session. This is the same transition question raised above (which input method is "active"), just on mobile instead of desktop — not resolved here either.

**Tier 3 — handheld PCs (Steam Deck, ASUS ROG Ally, MSI Claw, Lenovo Legion Go, and similar).** These should ride Tier 1's desktop backends essentially for free, since their built-in controls already present as a standard gamepad to the OS: Steam Deck's as a standard `evdev` device on Linux (SteamOS), and the Windows handhelds' specifically implement XInput compatibility for their built-in controls for exactly this reason. No dedicated backend work is anticipated for this tier beyond what Tier 1 already builds — it's a consequence of Tier 1 existing, not a separate undertaking. Each device family also has real extras beyond a standard gamepad's surface: Steam Deck's gyro, trackpads, and back-grip buttons (L4/L5/R4/R5); the Windows handhelds' own macro/paddle buttons and quick-access-menu buttons. All of these are explicitly deferred — the normalized shape above only covers sticks, triggers, face buttons, shoulders, d-pad, and the three system buttons.

**Tier 4 — consoles (Xbox, PlayStation).** Honestly blocked, not merely deprioritized: shipping a backend for either platform requires a console dev kit and platform NDA access this project doesn't have. Not schedulable until that access exists. The normalized shape above is deliberately generic enough (positional buttons, standard sticks/triggers) to receive that data if such access is ever obtained — this doc doesn't need to change shape later for that reason, only to gain a backend.

**Tier 5 — hybrid devices (Switch).** Same framing as consoles: blocked on Nintendo dev-kit access, not schedulable, shape already ready to receive it whenever that access might exist.

## Explicitly not decided yet / non-goals

- No backend implementation for Windows yet — still future work. macOS is done (2026-09-07), Linux 2026-09-09.
- No `FormFactor::Gamepad` variant. `touch/src/form_factor.rs`'s own doc comment states the rule directly: "grow this enum... only when a second real form factor actually exists to support... don't pre-build variants for platforms that aren't implemented yet."
- ~~How "which input method is active" should work, for adaptive UI prompts~~ — resolved 2026-09-07, see above (`InputMethod`).
- No connect/disconnect event type beyond a plain `connected: bool` compared across frames — the one exception being the stale-state-clearing requirement above, which is decided.
- No rumble/haptics output path — input-only, whenever this is built.
- No gyro, motion, or trackpad-as-pointer support (Steam Deck, DualSense) — named as deferred extensions above.
- No support for handheld-PC-specific extras beyond a standard gamepad (Steam Deck's back-grip buttons; the Windows handhelds' macro/paddle/quick-access-menu buttons) — named as deferred extensions in Tier 3 above.
- No decision on how multiple simultaneous pads map to players — a game-layer concern; a per-device `id` would give games a stable slot to build player assignment on top of, but this doc doesn't propose an assignment scheme.
- No decision on whether `FormFactor` should ever grow a controller-primary mobile variant, distinct from today's touch-assumed `MobileTouch` — raised by mobile clip-on/paired controllers (Tier 2 above), not resolved.

## Related docs

- [DESKTOP_PLATFORM_ROADMAP.md](DESKTOP_PLATFORM_ROADMAP.md) — Finding 3, superseded by this doc.
- [ARCHITECTURE.md](ARCHITECTURE.md) — layer model and the existing `## Input model` section this extends.
- [INPUT_PHILOSOPHY.md](INPUT_PHILOSOPHY.md) — the analog-over-boolean design consequence referenced above.
- [AUDIO.md](AUDIO.md) — `AudioMixer`'s precedent for "normalize once, cfg-split construction per platform capability," the same shape this doc's backend strategy would follow.
- [PROACTOR_ENGINE_ADOPTION.md](PROACTOR_ENGINE_ADOPTION.md) — documents `HostProactor<P: CompletionPort>`, a generic wrapper extracted only *after* being hand-rolled once per host; cited as why this doc doesn't propose a generic backend wrapper for gamepad polling.
