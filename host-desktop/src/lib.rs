mod audio;
pub use audio::*;

#[cfg(target_os = "android")]
mod android;
#[cfg(target_os = "android")]
pub use android::*;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(all(not(target_os = "macos"), not(target_os = "android"), not(target_os = "linux")))]
mod fallback;
#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "linux")]
pub use linux::*;
#[cfg(all(not(target_os = "macos"), not(target_os = "android"), not(target_os = "linux")))]
pub use fallback::*;
#[cfg(target_os = "macos")]
pub use macos::*;
