# Gamepad/Controller Input

## Status and purpose

Status: **design doc only, no code.** Written 2026-09-05, revised same-day after a type scaffold committed earlier in the session was reverted on review. This doc resolves the open questions [DESKTOP_PLATFORM_ROADMAP.md](DESKTOP_PLATFORM_ROADMAP.md)'s "Finding 3" raised on 2026-08-30 (crate placement, per-platform backend strategy, hand-roll vs. crate dependency, raw-event vs. normalized shape) and supersedes that finding as the living design doc for this subsystem going forward. It also carries forward the one concrete design consequence from [INPUT_PHILOSOPHY.md](INPUT_PHILOSOPHY.md): prefer continuous/analog representations over booleans wherever a signal is naturally continuous.

Before this, the only joystick-adjacent code anywhere in `loadngo` was `touch/src/joystick.rs`'s `VirtualJoystick` — an on-screen *virtual* stick for touch UI, not a physical controller input path. There is still no physical gamepad/controller code, and no gamepad crate dependency, anywhere in the workspace, and this doc does not add any.

**Same-day revision note**: the first version of this doc committed a `host-core/src/gamepad.rs` type scaffold (`GamepadSnapshot`, `GamepadButton`, deadzone helpers, a `HostFrame.gamepads` field) alongside the design writeup. On review this was reverted — it had no backend and no caller anywhere in the workspace, which is exactly the kind of speculative build-ahead-of-need this doc itself argues against elsewhere (see the `FormFactor` discussion below). The type shapes below are kept as an illustrative sketch for whoever eventually builds the first real backend, not as committed code, and two real design gaps the scaffold glossed over — where gamepad state should live, and how input-source transitions work — are corrected and addressed for the first time in this revision.

## Decision: crate placement

If and when this is built, it belongs in `host-core` (`loadngo-host-core`), not a new crate — gamepad state is a cross-platform contract exactly like touch and keyboard state, the same category `host-core` already exists to hold. A separate crate would just be an indirection with no platform-specific code inside it yet to isolate.

## Decision: hand-rolled per-platform backends

No `gilrs`, `sdl2`, or other cross-platform gamepad crate dependency. Consistent with how `loadngo` already handles audio (`host-desktop/src/audio_mixer.rs`), windowing (per-OS modules in `host-desktop`), and async I/O (`loadngo-proactor`'s per-OS `IoPort` implementations): own platform integration directly rather than depend on a black-box crate. Named targets, none implemented:

- **macOS**: the `GameController` framework, via the same `objc2`-based integration pattern `host-desktop/src/macos.rs` already uses for AppKit.
- **Windows**: `XInput` first — it covers Xbox controllers and most third-party pads with the least integration cost — with `DirectInput` as a later fallback for older or non-XInput devices.
- **Linux**: `evdev`/`udev` directly, no `gilrs` dependency, consistent with the project's existing preference for owning raw platform integration (mirrors how `host-desktop/src/netbsd_wsdesktop.rs`/`netbsd_wsdisplay.rs` already talk to `wsdisplay`/`wsmouse` device nodes directly rather than through an abstraction crate).

## Decision: normalized cross-pad shape, at the same level as mouse/keyboard/touch

A future `GamepadSnapshot` should normalize face buttons, sticks, and triggers uniformly across Xbox, PlayStation, and Switch Pro controller layouts, consistent with every other `loadngo` input surface (`UiEvent`, touch) already normalizing raw platform input rather than exposing a raw per-model event stream. Buttons should be named by position (`South`/`East`/`West`/`North`), not by any one brand's printed label, so the same value means the same physical button no matter which pad is plugged in.

Where it lives matters and the first version of this doc got it wrong: it proposed `gamepads: Vec<GamepadSnapshot>` as a field on `HostFrame`, a sibling to `input: InputSnapshot`, reasoned as "gamepads are inherently multi-device, `InputSnapshot` is single-seat." That reasoning doesn't hold up — `InputSnapshot.touches` is already `[Option<TouchPoint>; 8]`, handling up to eight simultaneous inputs *inside* `InputSnapshot`. Multiplicity isn't a reason to split a modality out. Mouse, keyboard, and touch are all read through the one `InputSnapshot` a game already consumes every frame; gamepad is a peer input modality, not an ancillary extra, and belongs in the same place: a `gamepads: Vec<GamepadSnapshot>` field *on* `InputSnapshot`, alongside `touches`, so `capture_frame()`'s `HostFrame.input` remains the single normalized surface for everything a player did this frame, regardless of device.

## Candidate shape (illustrative, not committed)

A sketch of what this might look like, for whoever builds the first real backend — not code that exists in the workspace:

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

Shape reasoning worth keeping even though none of this is built yet:

- **Triggers are analog only** — never duplicated as a boolean crossing an internal threshold. This is the one concrete place [INPUT_PHILOSOPHY.md](INPUT_PHILOSOPHY.md)'s "prefer continuous over boolean" consequence would land in code, whenever this is built.
- **Deadzone would be a caller-side helper**, not baked into raw storage, mirroring `touch/src/joystick.rs`'s `VirtualJoystick`: the same radial, magnitude-based deadzone shape (zero within the deadzone, unscaled pass-through above it, clamped to unit magnitude beyond `1.0`), with the deadzone value supplied by the caller rather than hardcoded.
- **`buttons_down` (held) vs. `buttons_pressed` (this-frame edge)** would mirror the existing `keys_down`/`key_events` split on `InputSnapshot` — the same continuous-hold-vs-one-shot-edge need.
- **`Vec<GamepadSnapshot>`, not a fixed-size array like `touches`** — the number of realistic simultaneously-connected pads is small but not as fixed a bound as 8 touches on one screen, and there's no established convention yet for what that bound would be.

## Open problem: input-source transitions

Not addressed at all in the first version of this doc, and a real gap: a game doesn't just need per-frame input state, it needs to handle a player *changing which device they're using*, mid-session. Two distinct problems, one decided, one genuinely open:

**Stale state on disconnect (decided: a hard requirement whenever a backend exists).** A gamepad going offline mid-hold — dead batteries, Bluetooth dropout, physically unplugged — while a button is pressed or a stick is deflected must not leave that state stuck. This is not hypothetical: `host-desktop/src/ios.rs`'s `capture_frame` carries a real fix, with a comment explaining exactly this class of bug for touch — a thumbstick that kept firing at its last dragged position after the finger lifted, because the previous code decayed `pending_input` *before* snapshotting it, so an `Ended` touch phase was wiped to `None` before any caller ever observed it. Any gamepad backend needs the equivalent discipline in the other direction: when `connected` transitions from `true` to `false`, `buttons_down`, `buttons_pressed`, and every stick/trigger value must be force-cleared to neutral in that same transition — mirroring `InputSnapshot::clear_keyboard_state()`'s existing behavior on keyboard focus loss — not left to whatever value they last held. This is a correctness requirement on the eventual backend contract, not new capability, so it's safe to decide now even without code: whoever writes the first backend needs to know this going in.

**Which input method is "active" (genuinely open, not decided here).** Games want to know which input method to show UI prompts for — "Press A" vs. "Click" vs. "Press Space" — and today that's `FormFactor`'s job (`touch/src/form_factor.rs`), which exists specifically because `sng-roguelite`'s restart prompt once shipped keyboard-only on a touch device. But `FormFactor::update` is a **one-way latch**: `Desktop → MobileTouch` promotion only, never demoted, because a device that has ever shown a real touch can be assumed touch-capable for the rest of the session. Gamepad-vs-mouse/keyboard on desktop has the opposite transition shape: a player legitimately alternates hands between a controller and a mouse within one sitting, and a design that only ever promotes toward "gamepad" and never back would show gamepad-style prompts to someone who just picked the mouse back up. This needs its own transition model — most likely "whichever source produced the most recent input event wins," rather than `FormFactor`'s latch — but that's a real, separate design question this doc is deliberately not answering yet. Solving it now, without a backend to test it against, would repeat the exact mistake this revision just corrected.

## Platform priority and phasing

**Tier 1 — desktop (macOS, Windows, Linux), highest priority.** The three platforms named above, each with its own named API. This is where real backend work should start once it starts.

**Tier 2 — mobile with a physical controller (clip-on or wireless).** `sng-roguelite`, `sng-rusty`, and `sng-zhoenus` already ship Android builds today (some also iOS), so this tier rides app/runtime infrastructure that already exists, unlike the access-gated tiers below. A physical controller reaches a phone or tablet two ways: a clip-on adapter that clamps directly onto the device (Razer Kishi, Backbone One, GameSir X2/X3, 8BitDo, PowerA MOGA, and similar), or a standalone pad (an Xbox or PlayStation controller, for instance) paired over Bluetooth or USB independently of any clip. Both arrive at the OS the same way, as a standard controller recognized by Android's `InputDevice` gamepad APIs or iOS's `GameController` framework — the same two backends the now-superseded Finding 3 already named as mobile's counterpart to desktop, just never phased in until now. No change to the normalized shape above would be needed; a future `GamepadSnapshot` fits a clip-on or paired pad the same way it fits a desktop one.

Worth naming even though it doesn't change today's design: a clip-on adapter shifts the phone or tablet from being the primary input surface (touch) to primarily a display-and-compute unit, with the controller doing the actual input — the inverse of what `touch/src/form_factor.rs`'s `FormFactor::MobileTouch` currently assumes for every mobile session. This is the same transition question raised above (which input method is "active"), just on mobile instead of desktop — not resolved here either.

**Tier 3 — handheld PCs (Steam Deck, ASUS ROG Ally, MSI Claw, Lenovo Legion Go, and similar).** These should ride Tier 1's desktop backends essentially for free, since their built-in controls already present as a standard gamepad to the OS: Steam Deck's as a standard `evdev` device on Linux (SteamOS), and the Windows handhelds' specifically implement XInput compatibility for their built-in controls for exactly this reason. No dedicated backend work is anticipated for this tier beyond what Tier 1 already builds — it's a consequence of Tier 1 existing, not a separate undertaking. Each device family also has real extras beyond a standard gamepad's surface: Steam Deck's gyro, trackpads, and back-grip buttons (L4/L5/R4/R5); the Windows handhelds' own macro/paddle buttons and quick-access-menu buttons. All of these are explicitly deferred — the normalized shape above only covers sticks, triggers, face buttons, shoulders, d-pad, and the three system buttons.

**Tier 4 — consoles (Xbox, PlayStation).** Honestly blocked, not merely deprioritized: shipping a backend for either platform requires a console dev kit and platform NDA access this project doesn't have. Not schedulable until that access exists. The normalized shape above is deliberately generic enough (positional buttons, standard sticks/triggers) to receive that data if such access is ever obtained — this doc doesn't need to change shape later for that reason, only to gain a backend.

**Tier 5 — hybrid devices (Switch).** Same framing as consoles: blocked on Nintendo dev-kit access, not schedulable, shape already ready to receive it whenever that access might exist.

## Explicitly not decided yet / non-goals

- No backend implementation for any platform — macOS, Windows, and Linux backends are all future work, not started.
- No code exists for this subsystem at all as of this revision — see the same-day revision note above.
- No `FormFactor::Gamepad` variant. `touch/src/form_factor.rs`'s own doc comment states the rule directly: "grow this enum... only when a second real form factor actually exists to support... don't pre-build variants for platforms that aren't implemented yet."
- How "which input method is active" should work, for adaptive UI prompts — see Open problem above. Genuinely unresolved, not just undecided in detail.
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
