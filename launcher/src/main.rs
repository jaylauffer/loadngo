//! The loadngo launcher with nothing built into it: `launcher::host` has the
//! window and the loop.

// A window, not a console program: Windows opens no console beside it.
#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    launcher::host::launch(Vec::new());
}
