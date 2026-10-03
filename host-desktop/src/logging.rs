//! Console and bounded persistent logging for every hosted application.
use loadngo_proactor::{LogFlush, PersistentLog, PersistentLogConfig, PersistentLogStatus};
use std::{fmt, path::PathBuf, sync::Mutex};

static LOGGER: Mutex<Option<PersistentLog>> = Mutex::new(None);

/// Called by platform launch after its proactor exists. The stable window
/// class is used on every OS, not just Linux, to isolate each app's logs.
pub(crate) fn initialize(window: &loadngo_host_core::WindowDescriptor) {
    let app_id = window.linux_wm_class.unwrap_or("loadngo");
    let result = (|| {
        if app_id.is_empty()
            || app_id.len() > 128
            || !app_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b".-_".contains(&byte))
        {
            return Err("invalid logging app identity".to_string());
        }
        let directory = PathBuf::from(crate::app_data_dir(app_id)?).join("logs");
        let config = PersistentLogConfig::new(directory.clone());
        let mut logger = LOGGER.lock().unwrap();
        if logger.is_none() {
            *logger = Some(crate::create_persistent_log(config)?);
        }
        Ok(directory)
    })();
    match result {
        Ok(directory) => console_record(
            false,
            format_args!(
                "[loadngo] persistent logs: {} (8 MiB maximum, 14-day retention)",
                directory.display()
            ),
        ),
        Err(error) => std::eprintln!("[loadngo] persistent logging unavailable: {error}"),
    }
}

pub(crate) fn persist(error: bool, message: fmt::Arguments<'_>) {
    if let Some(logger) = LOGGER.lock().unwrap().as_ref() {
        logger.record(if error { "error" } else { "info" }, message);
    }
}

pub(crate) fn console_record(error: bool, message: fmt::Arguments<'_>) {
    #[cfg(target_os = "android")]
    {
        // Android's native log function records to the persistent sink too.
        if error {
            crate::android_log_error(&message.to_string());
        } else {
            crate::android_log_info(&message.to_string());
        }
    }
    #[cfg(not(target_os = "android"))]
    {
        persist(error, message);
        if error {
            std::eprintln!("{message}");
        } else {
            std::println!("{message}");
        }
    }
}

/// Mirror an application report to the OS console and bounded local logs.
pub fn log_info(message: fmt::Arguments<'_>) {
    console_record(false, message);
}
pub fn log_error(message: fmt::Arguments<'_>) {
    console_record(true, message);
}

/// Await after an important report or before an application returns. The
/// existing host proactor delivers the flush completion and wakes the future.
pub async fn flush_logs() -> Result<(), String> {
    let flush: Option<LogFlush> = LOGGER.lock().unwrap().as_ref().map(PersistentLog::flush);
    if let Some(flush) = flush {
        flush.await
    } else {
        Ok(())
    }
}

pub fn persistent_log_status() -> Option<PersistentLogStatus> {
    LOGGER.lock().unwrap().as_ref().map(PersistentLog::status)
}

pub(crate) fn shutdown() {
    // Release the global mutex before joining: no console producer waits on
    // disk while holding this lock during teardown.
    let logger = LOGGER.lock().unwrap().take();
    drop(logger);
}
