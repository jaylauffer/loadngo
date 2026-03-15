//! Task application entrypoint (Rust Win32 port of TaskWindow/TaskMainWnd).

#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod date_banner;
#[cfg(windows)]
mod day_plan;
#[cfg(windows)]
mod day_planner;
#[cfg(windows)]
mod dragdrop;
#[cfg(windows)]
mod project_plan;
#[cfg(windows)]
mod project_planner;
#[cfg(windows)]
mod tabs;
#[cfg(windows)]
mod task_list;
#[cfg(windows)]
mod task_window;
#[cfg(windows)]
mod toolbar;
#[cfg(windows)]
mod winutil;

#[cfg(windows)]
use anyhow::Result;
#[cfg(windows)]
use tracing::Level;
#[cfg(windows)]
use tracing_subscriber::{fmt::writer::MakeWriterExt, EnvFilter};
#[cfg(windows)]
use windows::Win32::{
    Foundation::HWND,
    System::{
        LibraryLoader::GetModuleHandleW,
        Ole::{OleInitialize, OleUninitialize},
    },
    UI::WindowsAndMessaging::{ShowWindow, SW_SHOW},
};

#[cfg(windows)]
use task_window::{
    build_services, create_main_window, init_common_controls, load_all, message_loop,
    register_window_class,
};

#[cfg(windows)]
fn main() -> Result<()> {
    let file_appender = tracing_appender::rolling::never(".", "task.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive(Level::DEBUG.into()))
        .with_writer(non_blocking.and(std::io::stdout))
        .init();

    unsafe {
        // Match legacy startup: OleInitialize instead of CoInitializeEx.
        OleInitialize(None)
            .ok()
            .ok_or_else(|| anyhow::anyhow!("OleInitialize failed"))?;
        init_common_controls();
        let hinstance = GetModuleHandleW(None)?.into();
        let (mut service, mut network) = build_services()?;
        load_all(&mut service, "user_plan");
        network.init()?;
        register_window_class(hinstance)?;
        let hwnd: HWND = create_main_window(hinstance, service, network, "user_plan".to_string())?;
        let _ = ShowWindow(hwnd, SW_SHOW);
        tracing::info!("TaskWindow started");
        message_loop();
        OleUninitialize();
    }
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("task is currently available only on Windows; ui-core is the multiplatform extraction seam.");
}
