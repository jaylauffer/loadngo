mod audio;
pub use audio::*;
/// Shared text-fitting policy. Deliberately *not* `cfg`-gated by platform —
/// every software text rasterizer in this crate must agree on what
/// `RenderTextOverflow` means, and the one time they did not (Android had no
/// implementation at all) the failure was invisible until it reached a
/// device. See the module docs.
mod text_overflow;
pub use text_overflow::{fit_text_to_width, ELLIPSIS};
mod audio_mixer;
pub use audio_mixer::*;

#[cfg(target_os = "android")]
mod android;
#[cfg(target_os = "android")]
pub use android::*;
#[cfg(target_os = "android")]
mod android_jni;

#[cfg(all(
    not(target_os = "macos"),
    not(target_os = "ios"),
    not(target_os = "android"),
    not(target_os = "linux"),
    not(target_os = "windows")
))]
mod fallback;
#[cfg(target_os = "ios")]
mod ios;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
mod linux_gamepad;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "netbsd")]
pub mod netbsd_wsdesktop;
#[cfg(target_os = "netbsd")]
pub mod netbsd_wsdisplay;
#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "linux",
    target_os = "android",
    target_os = "windows"
))]
mod proactor_driver;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(all(
    not(target_os = "macos"),
    not(target_os = "ios"),
    not(target_os = "android"),
    not(target_os = "linux"),
    not(target_os = "windows")
))]
pub use fallback::*;
#[cfg(target_os = "ios")]
pub use ios::*;
#[cfg(target_os = "linux")]
pub use linux::*;
#[cfg(target_os = "macos")]
pub use macos::*;
#[cfg(target_os = "windows")]
pub use windows::*;

/// Reads a debug/config value the same way regardless of platform. Checks
/// the real process environment first (`std::env::var`), which is all any
/// desktop launch (`cargo run`, a CI job) needs. On Android there is no
/// `am start` equivalent of setting a process environment variable for the
/// launched app, so this falls back to the `debug.<name>` system property,
/// settable unprivileged from `adb shell setprop debug.<name> <value>` and
/// readable by any app (unlike most other property namespaces, `debug.*` is
/// not SELinux-restricted to the shell/system UID). iOS has its own working
/// mechanism already (`devicectl ... --environment-variables`), so it only
/// needs the environment-variable path.
///
/// A game should call this instead of `std::env::var` for any debug/test
/// hook it wants to trigger identically from a desktop shell, an Android
/// `adb setprop`, or an iOS `devicectl` launch.
#[must_use]
pub fn debug_config_value(name: &str) -> Option<String> {
    if let Ok(value) = std::env::var(name) {
        return Some(value);
    }
    #[cfg(target_os = "android")]
    {
        let property_name = format!("debug.{}", name.to_ascii_lowercase());
        return android::android_system_property(&property_name);
    }
    #[cfg(not(target_os = "android"))]
    {
        None
    }
}

/// The platform-agnostic concept every game built on `loadngo` uses to pick
/// which `loadngo_localization::Localizer` locale catalog to load: each
/// platform module defines its own `system_locale()` (re-exported above),
/// querying the OS's actual current-user locale rather than anything a game
/// has to configure itself. Returns a bare base-language tag ("en", "de",
/// "ja", ...) — region/script subtags are deliberately dropped, since a
/// locale catalog only exists per base language, not per region, until a
/// game actually needs that distinction. Always returns *something*
/// ("en" if the OS gives nothing usable): a game should never need to
/// handle "no locale" as a case of its own.
pub(crate) fn base_language_tag(raw: &str) -> Option<String> {
    let tag = raw
        .split(|c: char| !c.is_ascii_alphabetic())
        .next()?
        .to_ascii_lowercase();
    (!tag.is_empty()).then_some(tag)
}
