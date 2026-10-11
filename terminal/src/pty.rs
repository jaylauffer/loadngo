//! A program (usually the user's shell) on a pseudo-terminal.
//!
//! Two threads serve each one: a reader, blocked in `read` on the
//! terminal's master side, and a writer for input, so a large paste never
//! blocks the caller. Output collects in a buffer the caller swaps out
//! ([`Pty::drain_into`]); past [`MAX_PENDING`] bytes the reader waits for
//! the caller to catch up, so a program writing without end cannot grow
//! memory. Nothing polls: `wake` is called when output arrives into an
//! empty buffer and when the program's side closes.

use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, PoisonError};

/// Output held for the caller before the reader stops reading.
pub const MAX_PENDING: usize = 4 * 1024 * 1024;

/// What to run.
#[derive(Debug, Clone)]
pub struct PtyConfig {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub cols: u16,
    pub rows: u16,
    /// Set in the program's environment, beside `TERM=xterm-256color` and
    /// `COLORTERM=truecolor`.
    pub env: Vec<(String, String)>,
}

impl PtyConfig {
    /// The user's login shell (`$SHELL`, else `/bin/sh`) in `cwd`.
    pub fn login_shell(cwd: PathBuf, cols: u16, rows: u16) -> Self {
        let program = std::env::var_os("SHELL")
            .map(PathBuf::from)
            .filter(|path| path.is_file())
            .unwrap_or_else(|| PathBuf::from("/bin/sh"));
        Self {
            program,
            args: vec!["-l".to_string()],
            cwd,
            cols,
            rows,
            env: Vec::new(),
        }
    }
}

#[derive(Default)]
struct Inbox {
    output: Vec<u8>,
    /// The program's side closed (it and everything it started exited).
    closed: bool,
}

#[derive(Default)]
struct Shared {
    inbox: Mutex<Inbox>,
    drained: Condvar,
}

pub struct Pty {
    #[cfg(unix)]
    unix: unix::UnixPty,
    shared: Arc<Shared>,
    outgoing: std::sync::mpsc::Sender<Vec<u8>>,
}

impl Pty {
    /// Starts `config.program` on a new pseudo-terminal. `wake` is called
    /// from the reader thread when output is waiting and when it ends.
    pub fn spawn(config: &PtyConfig, wake: impl Fn() + Send + Sync + 'static) -> io::Result<Self> {
        Self::spawn_platform(config, wake)
    }

    /// Moves the output that arrived since the last call into `into`
    /// (which should be empty; its allocation is reused for the next
    /// output). Returns whether the program's side has closed.
    pub fn drain_into(&self, into: &mut Vec<u8>) -> bool {
        let mut inbox = self
            .shared
            .inbox
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        into.clear();
        std::mem::swap(&mut inbox.output, into);
        self.shared.drained.notify_all();
        inbox.closed && inbox.output.is_empty()
    }

    /// Sends input to the program.
    pub fn write(&self, bytes: Vec<u8>) {
        if !bytes.is_empty() {
            let _ = self.outgoing.send(bytes);
        }
    }

    /// Tells the program the terminal's new size (it gets `SIGWINCH`).
    pub fn resize(&self, cols: u16, rows: u16) {
        #[cfg(unix)]
        self.unix.resize(cols, rows);
        #[cfg(not(unix))]
        let _ = (cols, rows);
    }

    /// The program's exit status, once it has exited.
    pub fn exit_status(&mut self) -> Option<std::process::ExitStatus> {
        #[cfg(unix)]
        return self.unix.child.as_mut()?.try_wait().ok().flatten();
        #[cfg(not(unix))]
        None
    }

    #[cfg(unix)]
    fn reader(
        mut source: impl io::Read + Send + 'static,
        shared: Arc<Shared>,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> io::Result<()> {
        std::thread::Builder::new()
            .name("pty-reader".to_string())
            .spawn(move || {
                let mut buffer = vec![0u8; 64 * 1024];
                loop {
                    let read = match source.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(read) => read,
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                        // EIO: the program's side closed.
                        Err(_) => break,
                    };
                    let mut inbox = shared.inbox.lock().unwrap_or_else(PoisonError::into_inner);
                    while inbox.output.len() >= MAX_PENDING {
                        inbox = shared
                            .drained
                            .wait(inbox)
                            .unwrap_or_else(PoisonError::into_inner);
                    }
                    let was_empty = inbox.output.is_empty();
                    inbox.output.extend_from_slice(&buffer[..read]);
                    drop(inbox);
                    if was_empty {
                        wake();
                    }
                }
                shared
                    .inbox
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .closed = true;
                wake();
            })
            .map(drop)
    }

    #[cfg(unix)]
    fn writer(
        mut sink: impl io::Write + Send + 'static,
    ) -> io::Result<std::sync::mpsc::Sender<Vec<u8>>> {
        let (outgoing, queued) = std::sync::mpsc::channel::<Vec<u8>>();
        std::thread::Builder::new()
            .name("pty-writer".to_string())
            .spawn(move || {
                for bytes in queued {
                    if sink.write_all(&bytes).and_then(|()| sink.flush()).is_err() {
                        break;
                    }
                }
            })?;
        Ok(outgoing)
    }
}

#[cfg(unix)]
mod unix {
    use std::fs::File;
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command, Stdio};
    use std::sync::Arc;

    use super::{Pty, PtyConfig, Shared};

    pub(super) struct UnixPty {
        /// The master side, kept for resizing.
        pub(super) master: File,
        /// Taken only when dropped.
        pub(super) child: Option<Child>,
    }

    impl UnixPty {
        pub(super) fn resize(&self, cols: u16, rows: u16) {
            let size = libc::winsize {
                ws_row: rows,
                ws_col: cols,
                ws_xpixel: 0,
                ws_ypixel: 0,
            };
            unsafe {
                libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ as _, &size);
            }
        }
    }

    impl Drop for UnixPty {
        fn drop(&mut self) {
            // Hang up on the shell's whole session, as closing a terminal
            // window does, then reap it off the caller's thread.
            let Some(mut child) = self.child.take() else {
                return;
            };
            if let Ok(pid) = i32::try_from(child.id()) {
                unsafe {
                    libc::kill(-pid, libc::SIGHUP);
                }
            }
            if !matches!(child.try_wait(), Ok(Some(_))) {
                let _ = std::thread::Builder::new()
                    .name("pty-reaper".to_string())
                    .spawn(move || {
                        let _ = child.wait();
                    });
            }
        }
    }

    fn cloexec(fd: i32) -> io::Result<()> {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    impl Pty {
        pub(super) fn spawn_platform(
            config: &PtyConfig,
            wake: impl Fn() + Send + Sync + 'static,
        ) -> io::Result<Self> {
            let mut master = -1;
            let mut slave = -1;
            let mut size = libc::winsize {
                ws_row: config.rows,
                ws_col: config.cols,
                ws_xpixel: 0,
                ws_ypixel: 0,
            };
            let opened = unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &mut size,
                )
            };
            if opened != 0 {
                return Err(io::Error::last_os_error());
            }
            let master = unsafe { File::from_raw_fd(master) };
            let slave = unsafe { File::from_raw_fd(slave) };
            cloexec(master.as_raw_fd())?;
            cloexec(slave.as_raw_fd())?;

            let mut command = Command::new(&config.program);
            command
                .args(&config.args)
                .current_dir(&config.cwd)
                .env("TERM", "xterm-256color")
                .env("COLORTERM", "truecolor")
                .envs(config.env.iter().map(|(key, value)| (key, value)))
                .stdin(Stdio::from(slave.try_clone()?))
                .stdout(Stdio::from(slave.try_clone()?))
                .stderr(Stdio::from(slave));
            unsafe {
                command.pre_exec(|| {
                    // A new session, with the terminal as its controlling
                    // terminal, so job control and Ctrl-C work.
                    if libc::setsid() < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    if libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let child = command.spawn()?;
            // The slave copies above were the child's; the parent's closed
            // when `command` is dropped.
            drop(command);

            let shared = Arc::new(Shared::default());
            Pty::reader(master.try_clone()?, Arc::clone(&shared), wake)?;
            let outgoing = Pty::writer(master.try_clone()?)?;
            Ok(Pty {
                unix: UnixPty {
                    master,
                    child: Some(child),
                },
                shared,
                outgoing,
            })
        }
    }
}

#[cfg(not(unix))]
impl Pty {
    fn spawn_platform(
        _config: &PtyConfig,
        _wake: impl Fn() + Send + Sync + 'static,
    ) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "terminals are not available on this platform yet",
        ))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn a_program_runs_on_the_terminal_and_sees_its_size() {
        let (woken, wakes) = mpsc::channel();
        let config = PtyConfig {
            program: PathBuf::from("/bin/sh"),
            args: vec![
                "-c".to_string(),
                "stty size; [ -t 0 ] && echo tty; read line; echo got:$line".to_string(),
            ],
            cwd: std::env::temp_dir(),
            cols: 77,
            rows: 21,
            env: Vec::new(),
        };
        let mut pty = Pty::spawn(&config, move || {
            let _ = woken.send(());
        })
        .unwrap();
        pty.write(b"hello\r".to_vec());
        let mut text = Vec::new();
        let mut chunk = Vec::new();
        loop {
            wakes
                .recv_timeout(Duration::from_secs(10))
                .expect("output within 10 s");
            let closed = pty.drain_into(&mut chunk);
            text.extend_from_slice(&chunk);
            if closed {
                break;
            }
        }
        let text = String::from_utf8_lossy(&text);
        assert!(text.contains("21 77"), "{text:?}");
        assert!(text.contains("tty"), "{text:?}");
        assert!(text.contains("got:hello"), "{text:?}");
        let status = pty.unix.child.as_mut().unwrap().wait().unwrap();
        assert!(status.success());
        assert!(pty.exit_status().is_some_and(|status| status.success()));
    }
}
