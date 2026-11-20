//! Opening a web address, a file or a folder with whatever the system uses
//! for it: the default browser for an address, the default app for a file,
//! the file manager for a folder. What a double-click would do.
//!
//! Blocks until the system has taken the request (it starts the system's
//! opener and waits for it to hand off), so call it off the frame thread: an
//! offloaded job.
//!
//! An iPhone opens addresses only: a web page, or another app by a scheme
//! that app has declared (`sng-mahjong://`). It has no opener for a file or
//! a folder.

/// Opens `target`, an address such as `https://example.org/` or a path.
/// Platforms without an opener say so in the error.
pub fn open_in_system(target: &str) -> Result<(), String> {
    if target.trim().is_empty() {
        return Err("nothing to open".to_string());
    }
    imp::open(target)
}

/// Runs the system's opener program and reports why it refused, if it did.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn run_opener(program: &str, target: &str) -> Result<(), String> {
    let output = std::process::Command::new(program)
        .arg(target)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|error| format!("could not run {program}: {error}"))?;
    if output.status.success() {
        return Ok(());
    }
    let said = String::from_utf8_lossy(&output.stderr);
    let said = said.trim();
    if said.is_empty() {
        Err(format!("{program} could not open {target}"))
    } else {
        Err(said.to_string())
    }
}

#[cfg(target_os = "macos")]
mod imp {
    pub fn open(target: &str) -> Result<(), String> {
        super::run_opener("/usr/bin/open", target)
    }
}

#[cfg(target_os = "linux")]
mod imp {
    pub fn open(target: &str) -> Result<(), String> {
        super::run_opener("xdg-open", target)
    }
}

#[cfg(target_os = "windows")]
mod imp {
    use std::os::windows::ffi::OsStrExt;

    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    pub fn open(target: &str) -> Result<(), String> {
        let mut wide: Vec<u16> = std::ffi::OsStr::new(target).encode_wide().collect();
        wide.push(0);
        // SAFETY: both strings are NUL-terminated and outlive the call; the
        // shell copies what it keeps.
        let result = unsafe {
            ShellExecuteW(
                HWND::default(),
                w!("open"),
                PCWSTR(wide.as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            )
        };
        // The shell reports success as a value above 32.
        let code = result.0 as isize;
        if code > 32 {
            Ok(())
        } else {
            Err(format!("the shell could not open {target} (code {code})"))
        }
    }
}

/// Whether `target` begins with a scheme (`https:`, `sng-mahjong:`), as an
/// address does and a path does not.
#[cfg(any(target_os = "ios", test))]
fn has_scheme(target: &str) -> bool {
    let Some((scheme, _)) = target.split_once(':') else {
        return false;
    };
    let mut letters = scheme.chars();
    letters
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic())
        && letters.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

#[cfg(target_os = "ios")]
mod imp {
    use std::ffi::c_void;
    use std::sync::mpsc;

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, Bool};
    use objc2::{class, msg_send};
    use objc2_foundation::NSString;

    #[repr(C)]
    struct DispatchQueue {
        _opaque: [u8; 0],
    }

    extern "C" {
        /// The main thread's queue (`dispatch_get_main_queue()` is a macro
        /// for its address).
        static _dispatch_main_q: DispatchQueue;
        fn dispatch_async_f(
            queue: *const DispatchQueue,
            context: *mut c_void,
            work: extern "C" fn(*mut c_void),
        );
    }

    struct Request {
        target: String,
        answer: mpsc::Sender<Result<(), String>>,
    }

    pub fn open(target: &str) -> Result<(), String> {
        if !super::has_scheme(target) {
            return Err(format!(
                "{target} is not an address, and an iPhone opens addresses only"
            ));
        }
        let (answer, answered) = mpsc::channel();
        let request = Box::new(Request {
            target: target.to_string(),
            answer,
        });
        // UIApplication belongs to the main thread; this is an offloaded job.
        // SAFETY: the queue is the process's main queue, and `on_main` takes
        // the box back exactly once.
        unsafe {
            dispatch_async_f(
                &raw const _dispatch_main_q,
                Box::into_raw(request).cast(),
                on_main,
            );
        }
        // No time limit: the first time one app opens another, iOS asks the
        // user whether it may, and answers here only once they have. If
        // UIKit lets go of the request without answering, the sender goes
        // with it and this stops waiting.
        answered
            .recv()
            .unwrap_or_else(|_| Err("iOS let the request go without an answer".to_string()))
    }

    extern "C" fn on_main(context: *mut c_void) {
        // SAFETY: `context` is the box `open` leaked for this one call.
        let request = unsafe { Box::from_raw(context.cast::<Request>()) };
        let Request { target, answer } = *request;
        let text = NSString::from_str(&target);
        // SAFETY: plain UIKit and Foundation calls on the main thread, with
        // objects that live until the calls return; UIKit copies the block.
        unsafe {
            let url: Option<Retained<AnyObject>> = msg_send![class!(NSURL), URLWithString: &*text];
            let app: Option<Retained<AnyObject>> =
                msg_send![class!(UIApplication), sharedApplication];
            let (Some(url), Some(app)) = (url, app) else {
                let _ = answer.send(Err(format!("{target} is not an address iOS understands")));
                return;
            };
            let options: Retained<AnyObject> = msg_send![class!(NSDictionary), dictionary];
            let done = RcBlock::new(move |opened: Bool| {
                let _ = answer.send(if opened.as_bool() {
                    Ok(())
                } else {
                    Err("no app on this iPhone answers to it, or it was declined".to_string())
                });
            });
            let _: () = msg_send![
                &*app,
                openURL: &*url,
                options: &*options,
                completionHandler: &*done
            ];
        }
    }
}

#[cfg(not(any(
    target_os = "macos",
    target_os = "linux",
    target_os = "windows",
    target_os = "ios"
)))]
mod imp {
    pub fn open(_target: &str) -> Result<(), String> {
        Err("this platform has no opener".to_string())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn nothing_is_not_opened() {
        assert!(super::open_in_system("  ").is_err());
    }

    #[test]
    fn an_address_has_a_scheme_and_a_path_has_none() {
        for address in ["https://example.org/", "sng-mahjong://", "mailto:a@b.c"] {
            assert!(super::has_scheme(address), "{address}");
        }
        for path in [
            "/Users/me/notes.txt",
            "notes.txt",
            "~/src",
            "://x",
            "1st:x",
            "",
        ] {
            assert!(!super::has_scheme(path), "{path}");
        }
    }
}
