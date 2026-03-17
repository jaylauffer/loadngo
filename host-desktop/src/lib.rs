mod audio;
pub use audio::*;

#[cfg(target_os = "android")]
mod android;
#[cfg(target_os = "android")]
pub use android::*;

#[cfg(all(not(target_os = "macos"), not(target_os = "android")))]
mod fallback;
#[cfg(target_os = "macos")]
mod macos;

#[cfg(all(not(target_os = "macos"), not(target_os = "android")))]
pub use fallback::*;
#[cfg(target_os = "macos")]
pub use macos::*;
