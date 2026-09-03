mod audio;
pub use audio::*;
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
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "netbsd")]
pub mod netbsd_wsdesktop;
#[cfg(target_os = "netbsd")]
pub mod netbsd_wsdisplay;
#[cfg(any(target_os = "macos", target_os = "linux"))]
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
