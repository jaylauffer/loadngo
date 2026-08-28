mod audio;
pub use audio::*;

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
