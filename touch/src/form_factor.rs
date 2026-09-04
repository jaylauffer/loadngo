use loadngo_host_core::InputSnapshot;

/// Which device form factor a game session is actually being played on.
///
/// Deliberately small and named rather than a big speculative capability
/// system: today there are exactly two real form factors (desktop
/// keyboard/mouse, and touch), and this is the single fact every screen is
/// expected to consult before choosing platform-specific prompt text or
/// affordances -- instead of re-deriving "is this touch" ad hoc per game, or
/// worse, forgetting to and only ever offering a keyboard affordance (that's
/// exactly how `sng-roguelite`'s run-summary restart prompt and
/// reward-cache interact shipped keyboard-only the first time, before this
/// existed). Originally duplicated per-game (`sng-roguelite`, then
/// `sng-zhoenus`) before moving here so every loadngo game shares one
/// definition instead of drifting. Grow this enum (e.g. a future `Gamepad`
/// variant, or a tablet/portrait form factor) only when a second real form
/// factor actually exists to support, per the data-driven-over-speculative
/// style; don't pre-build variants for platforms that aren't implemented
/// yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormFactor {
    Desktop,
    MobileTouch,
}

impl FormFactor {
    /// What a session starts as, before any input has been observed.
    ///
    /// Android and iOS are touch-only devices from the moment the process
    /// starts -- there is nothing to wait and see. Without this, a session
    /// that ends before the player's first touch (or simply the first
    /// rendered frame) would report `Desktop` and regress right back into a
    /// keyboard-only prompt (e.g. "Press Space to start") on a device with
    /// no keyboard -- exactly what iOS did before this `cfg` covered it,
    /// since winit gives no other reliable a-priori touch-capability signal
    /// at startup. Desktop stays `Desktop` unless a real touch later proves
    /// otherwise (e.g. a touchscreen laptop) -- see [`FormFactor::update`].
    #[must_use]
    pub const fn platform_default() -> Self {
        #[cfg(any(target_os = "android", target_os = "ios"))]
        {
            Self::MobileTouch
        }
        #[cfg(not(any(target_os = "android", target_os = "ios")))]
        {
            Self::Desktop
        }
    }

    /// One-way promotion, never demotion: seeing a real touch this frame
    /// proves the session is actually being played as `MobileTouch`. No-op
    /// on Android/iOS, which already start there via `platform_default`.
    /// Safe to call every frame regardless of whether any touch is present.
    pub fn update(&mut self, input: &InputSnapshot) {
        if input.active_touches().next().is_some() {
            *self = Self::MobileTouch;
        }
    }
}
