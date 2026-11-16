//! Starting, watching and stopping what the launcher runs.
//!
//! Each start gets a thread of its own, which blocks on the program's output
//! or on its exit; nothing here polls. What happens is queued as [`Event`]s
//! and the app is woken to collect them with [`Supervisor::take_events`].
//!
//! Two ways of running, by the entry's [`Kind`]:
//!
//! - A build step and a service are **owned**: their output comes down a
//!   pipe (shown line by line), and they are stopped when the launcher
//!   quits, since a program writing into a pipe nobody reads would fail.
//! - A program with its own window is **left running**: its output goes to
//!   a log file, so it outlives the launcher untroubled.
//!
//! Each runs in a process group of its own, so stopping one stops what it
//! started too (`npm` and the `node` under it).

use std::collections::{HashMap, VecDeque};
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Child, ExitStatus, Stdio};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use crate::catalog::{self, Entry, Kind};

/// Lines of output kept for a failure's report.
const TAIL_LINES: usize = 40;
/// A line of output longer than this is cut.
const MAX_LINE: usize = 400;
/// Output lines waiting for the app beyond this are dropped: a frame collects
/// them all, so only a program printing faster than frames run gets here.
const MAX_QUEUED_LINES: usize = 4096;
/// How long a stopped program has to exit before it is killed.
const STOP_PATIENCE: Duration = Duration::from_secs(3);
/// A signal to a group misses a process that one of its members is in the
/// middle of starting, and that process then holds the group's output open.
/// So the request is made once more, this long after the first.
const ASK_AGAIN: Duration = Duration::from_millis(250);

/// Something that happened to a start, named by the number it was given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub run: u64,
    pub what: What,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum What {
    /// The build step has started.
    Building,
    /// The program has started.
    Started,
    /// A line of output from a build step or a service.
    Line(String),
    /// A service's page can be opened.
    Ready,
    Ended(Ending),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ending {
    /// It finished by itself, successfully.
    Finished,
    /// It was stopped from the launcher.
    Stopped,
    /// It could not start, or it ended with a failure. `tail` is the last of
    /// what it printed.
    Failed { why: String, tail: Vec<String> },
}

/// Where a bare program name is looked up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchPath {
    /// The launcher's own `PATH`.
    Inherited,
    /// The `PATH` of the user's login shell, asked once. An app started from
    /// the Dock or a desktop menu has only the system's bare `PATH`, without
    /// `cargo`, `npm` and the rest; this finds what a terminal would.
    UserShell,
}

/// A started program's process group (Unix) or process (Windows).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Group(u32);

impl Group {
    fn of(child: &Child) -> Self {
        Self(child.id())
    }

    /// Asks everything in the group to exit.
    #[cfg(unix)]
    fn terminate(self) {
        // SAFETY: kill takes numbers only. A group that has already gone
        // gives ESRCH, which is fine.
        unsafe {
            libc::kill(-(self.0 as libc::pid_t), libc::SIGTERM);
        }
    }

    /// Ends everything in the group at once.
    #[cfg(unix)]
    fn kill(self) {
        // SAFETY: as in `terminate`.
        unsafe {
            libc::kill(-(self.0 as libc::pid_t), libc::SIGKILL);
        }
    }

    /// Windows has no signal a windowless program must honour, so both ask
    /// `taskkill` to end the process and everything it started.
    #[cfg(windows)]
    fn terminate(self) {
        self.kill();
    }

    #[cfg(windows)]
    fn kill(self) {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &self.0.to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .status();
    }

    #[cfg(not(any(unix, windows)))]
    fn terminate(self) {}

    #[cfg(not(any(unix, windows)))]
    fn kill(self) {}
}

/// Whether the launcher's exit stops a running program.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hold {
    /// Its output comes down a pipe: it is stopped with the launcher.
    Owned,
    /// Its output goes to a log file: it is left running.
    LeftRunning,
}

#[derive(Default)]
struct RunState {
    /// What is running for this start right now.
    running: Option<(Group, Hold)>,
    /// Stop was asked: start nothing further.
    cancelled: bool,
    /// The start's thread has finished.
    done: bool,
}

#[derive(Default)]
struct Run {
    state: Mutex<RunState>,
    ended: Condvar,
}

impl Run {
    fn lock(&self) -> MutexGuard<'_, RunState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn cancelled(&self) -> bool {
        self.lock().cancelled
    }

    /// Records what was just started. If stop was asked meanwhile, it is
    /// stopped here.
    fn adopt(&self, group: Group, hold: Hold) {
        let mut state = self.lock();
        if state.cancelled {
            group.terminate();
        }
        state.running = Some((group, hold));
        if hold == Hold::Owned {
            owned_at_exit::remember(group);
        }
    }

    /// What was running has ended.
    fn release(&self) {
        if let Some((group, _)) = self.lock().running.take() {
            owned_at_exit::forget(group);
        }
    }

    /// Asks it to stop, and to start nothing further.
    fn cancel(&self) {
        let mut state = self.lock();
        state.cancelled = true;
        if let Some((group, _)) = state.running {
            group.terminate();
        }
    }

    /// Ends outright what is still running.
    fn kill(&self) {
        if let Some((group, _)) = self.lock().running {
            group.kill();
        }
    }

    /// Whether the launcher's exit leaves this start alone: only a program
    /// that is up, writing to its log.
    fn left_running(&self) -> bool {
        matches!(self.lock().running, Some((_, Hold::LeftRunning)))
    }

    /// Waits until the start's thread has finished, or `deadline`.
    fn wait_done(&self, deadline: Instant) -> bool {
        let mut state = self.lock();
        while !state.done {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            state = self
                .ended
                .wait_timeout(state, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        true
    }
}

struct Inner {
    events: Mutex<VecDeque<Event>>,
    wake: Box<dyn Fn() + Send + Sync>,
    runs: Mutex<HashMap<u64, Arc<Run>>>,
    /// Where the log files of programs left running go.
    logs: PathBuf,
    search: SearchPath,
    /// How long a stopped program has to exit before it is killed.
    patience: Duration,
}

impl Inner {
    /// Queues an event; wakes the app if it had nothing waiting, so a burst
    /// of output costs one wake.
    fn send(&self, run: u64, what: What) {
        let was_empty = {
            let mut events = self.events.lock().unwrap_or_else(PoisonError::into_inner);
            if matches!(what, What::Line(_)) && events.len() >= MAX_QUEUED_LINES {
                return;
            }
            let was_empty = events.is_empty();
            events.push_back(Event { run, what });
            was_empty
        };
        if was_empty {
            (self.wake)();
        }
    }

    fn runs(&self) -> MutexGuard<'_, HashMap<u64, Arc<Run>>> {
        self.runs.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Runs the catalog's entries.
pub struct Supervisor {
    inner: Arc<Inner>,
}

impl Supervisor {
    /// `logs` is the folder for the log files of programs left running.
    /// `wake` is called, from another thread, when events are waiting.
    pub fn new(logs: PathBuf, search: SearchPath, wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self::with_patience(logs, search, STOP_PATIENCE, wake)
    }

    /// [`Supervisor::new`] with another wait before a stopped program is
    /// killed.
    fn with_patience(
        logs: PathBuf,
        search: SearchPath,
        patience: Duration,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        owned_at_exit::install();
        Self {
            inner: Arc::new(Inner {
                events: Mutex::new(VecDeque::new()),
                wake: Box::new(wake),
                runs: Mutex::new(HashMap::new()),
                logs,
                search,
                patience,
            }),
        }
    }

    /// Starts `entry` (one with a `run`) as start number `run`. Everything
    /// that follows arrives as events with that number, the last of them
    /// [`What::Ended`].
    pub fn start(&self, run: u64, entry: &Entry) {
        let record = Arc::new(Run::default());
        self.inner.runs().insert(run, Arc::clone(&record));
        let inner = Arc::clone(&self.inner);
        let entry = entry.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("launcher-run-{run}"))
            .spawn(move || {
                let ending = run_entry(&inner, run, &entry, &record);
                record.release();
                inner.runs().remove(&run);
                {
                    let mut state = record.lock();
                    state.done = true;
                }
                record.ended.notify_all();
                inner.send(run, What::Ended(ending));
            });
        if let Err(error) = spawned {
            self.inner.runs().remove(&run);
            self.inner.send(
                run,
                What::Ended(Ending::Failed {
                    why: format!("could not start a thread for it: {error}"),
                    tail: Vec::new(),
                }),
            );
        }
    }

    /// Stops start number `run`: asks it to exit, and ends it outright if
    /// it has not within a few seconds. Does nothing once it has ended.
    pub fn stop(&self, run: u64) {
        let Some(record) = self.inner.runs().get(&run).cloned() else {
            return;
        };
        record.cancel();
        let patience = self.inner.patience;
        // Waiting on a program that takes its time is not the frame
        // thread's to do.
        let _ = std::thread::Builder::new()
            .name(format!("launcher-stop-{run}"))
            .spawn(move || {
                let asked = Instant::now();
                if record.wait_done(asked + ASK_AGAIN.min(patience)) {
                    return;
                }
                record.cancel();
                if !record.wait_done(asked + patience) {
                    record.kill();
                }
            });
    }

    /// The events since the last call, oldest first.
    pub fn take_events(&self) -> Vec<Event> {
        self.inner
            .events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .drain(..)
            .collect()
    }

    /// For the launcher's exit: stops every build step and service, waits
    /// up to `patience` for them and ends outright what is left. Programs
    /// that are up in their own windows are left running.
    pub fn shutdown(&self, patience: Duration) {
        let stopping: Vec<Arc<Run>> = self
            .inner
            .runs()
            .values()
            .filter(|run| !run.left_running())
            .cloned()
            .collect();
        let asked = Instant::now();
        for run in &stopping {
            run.cancel();
        }
        for run in &stopping {
            if !run.wait_done(asked + ASK_AGAIN.min(patience)) {
                run.cancel();
            }
        }
        for run in &stopping {
            if !run.wait_done(asked + patience) {
                run.kill();
            }
        }
    }
}

/// The last lines a start printed.
struct Tail(VecDeque<String>);

impl Tail {
    fn new() -> Self {
        Self(VecDeque::with_capacity(TAIL_LINES))
    }

    fn push(&mut self, line: &str) {
        if self.0.len() == TAIL_LINES {
            self.0.pop_front();
        }
        self.0.push_back(line.to_string());
    }

    fn take(&mut self) -> Vec<String> {
        self.0.drain(..).collect()
    }
}

enum Finish {
    Exited(ExitStatus),
    NoStart(String),
}

/// One start, from its build step to the end of its program.
fn run_entry(inner: &Inner, id: u64, entry: &Entry, run: &Run) -> Ending {
    let mut tail = Tail::new();
    if let Some(build) = &entry.build {
        inner.send(id, What::Building);
        let finish = run_piped(inner, id, run, build, None, &mut tail);
        if run.cancelled() {
            return Ending::Stopped;
        }
        match finish {
            Finish::Exited(status) if status.success() => {}
            Finish::Exited(status) => {
                return Ending::Failed {
                    why: format!("The build failed ({status})."),
                    tail: tail.take(),
                }
            }
            Finish::NoStart(error) => {
                return Ending::Failed {
                    why: format!("The build could not start: {error}"),
                    tail: tail.take(),
                }
            }
        }
        run.release();
        tail.take();
    }
    let Some(command) = &entry.run else {
        return Ending::Finished;
    };
    let finish = match entry.kind() {
        Kind::Service => run_piped(
            inner,
            id,
            run,
            command,
            Some(entry.ready.as_deref()),
            &mut tail,
        ),
        Kind::Program | Kind::Link => run_logged(inner, id, run, &entry.name, command, &mut tail),
    };
    if run.cancelled() {
        return Ending::Stopped;
    }
    match finish {
        Finish::Exited(status) if status.success() => Ending::Finished,
        Finish::Exited(status) => Ending::Failed {
            why: format!("It ended with a failure ({status})."),
            tail: tail.take(),
        },
        Finish::NoStart(error) => Ending::Failed {
            why: format!("It could not start: {error}"),
            tail: tail.take(),
        },
    }
}

/// Runs `command` with its output down a pipe, sending each line, until it
/// and everything it started have closed the pipe. `service` is `None` for a
/// build step; for a service it holds the text that means the page is ready
/// (`Some(None)`: ready as soon as it has started).
fn run_piped(
    inner: &Inner,
    id: u64,
    run: &Run,
    command: &catalog::Command,
    service: Option<Option<&str>>,
    tail: &mut Tail,
) -> Finish {
    let (reader, writer) = match std::io::pipe() {
        Ok(pipe) => pipe,
        Err(error) => return Finish::NoStart(error.to_string()),
    };
    let second = match writer.try_clone() {
        Ok(second) => second,
        Err(error) => return Finish::NoStart(error.to_string()),
    };
    let mut process = configured(command, inner.search);
    process.stdout(writer).stderr(second);
    let mut child = match process.spawn() {
        Ok(child) => child,
        Err(error) => return Finish::NoStart(no_start(command, &error)),
    };
    // The pipe's writing ends held here must close, or its end never comes.
    drop(process);
    run.adopt(Group::of(&child), Hold::Owned);

    let mut waiting_for = None;
    if let Some(ready) = service {
        inner.send(id, What::Started);
        match ready {
            Some(text) => waiting_for = Some(text),
            None => inner.send(id, What::Ready),
        }
    }
    let mut reader = BufReader::new(reader);
    let mut bytes = Vec::with_capacity(256);
    loop {
        bytes.clear();
        match reader.read_until(b'\n', &mut bytes) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let line = clean_line(&bytes);
        if line.is_empty() {
            continue;
        }
        tail.push(&line);
        if waiting_for.is_some_and(|text| line.contains(text)) {
            waiting_for = None;
            inner.send(id, What::Line(line));
            inner.send(id, What::Ready);
        } else {
            inner.send(id, What::Line(line));
        }
    }
    match child.wait() {
        Ok(status) => Finish::Exited(status),
        Err(error) => Finish::NoStart(error.to_string()),
    }
}

/// Runs `command` with its output in a log file, and waits for it to exit.
/// The launcher quitting meanwhile leaves it running.
fn run_logged(
    inner: &Inner,
    id: u64,
    run: &Run,
    name: &str,
    command: &catalog::Command,
    tail: &mut Tail,
) -> Finish {
    let path = log_path(&inner.logs, name);
    let log = std::fs::create_dir_all(&inner.logs)
        .and_then(|()| std::fs::File::create(&path))
        .and_then(|file| Ok((file.try_clone()?, file)));
    let (out, err) = match log {
        Ok(files) => files,
        Err(error) => {
            return Finish::NoStart(format!("no log file at {}: {error}", path.display()))
        }
    };
    let mut process = configured(command, inner.search);
    process.stdout(out).stderr(err);
    let mut child = match process.spawn() {
        Ok(child) => child,
        Err(error) => return Finish::NoStart(no_start(command, &error)),
    };
    drop(process);
    run.adopt(Group::of(&child), Hold::LeftRunning);
    inner.send(id, What::Started);
    match child.wait() {
        Ok(status) => {
            if !status.success() {
                for line in log_tail(&path) {
                    tail.push(&line);
                }
            }
            Finish::Exited(status)
        }
        Err(error) => Finish::NoStart(error.to_string()),
    }
}

fn no_start(command: &catalog::Command, error: &std::io::Error) -> String {
    if error.kind() == std::io::ErrorKind::NotFound {
        match &command.dir {
            Some(dir) if !dir.is_dir() => format!("there is no folder {}", dir.display()),
            _ => format!("there is no program \"{}\"", command.program),
        }
    } else {
        format!("{}: {error}", command.program)
    }
}

/// The log file of the entry called `name`.
pub fn log_path(logs: &Path, name: &str) -> PathBuf {
    let mut slug = String::with_capacity(name.len());
    for ch in name.chars() {
        if ch.is_alphanumeric() {
            slug.extend(ch.to_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    logs.join(format!(
        "{}.log",
        if slug.is_empty() { "entry" } else { slug }
    ))
}

/// The last lines of a log file.
fn log_tail(path: &Path) -> Vec<String> {
    const LAST_BYTES: u64 = 8 * 1024;
    let Ok(mut file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let length = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    if file
        .seek(SeekFrom::Start(length.saturating_sub(LAST_BYTES)))
        .is_err()
    {
        return Vec::new();
    }
    let mut bytes = Vec::new();
    if file.take(LAST_BYTES).read_to_end(&mut bytes).is_err() {
        return Vec::new();
    }
    let mut lines: Vec<String> = bytes
        .split(|byte| *byte == b'\n')
        .map(clean_line)
        .filter(|line| !line.is_empty())
        .collect();
    // The first may be the back half of a line.
    if length > LAST_BYTES && !lines.is_empty() {
        lines.remove(0);
    }
    let keep = lines.len().saturating_sub(TAIL_LINES);
    lines.split_off(keep)
}

/// A line of program output as text to show: no line ending, no terminal
/// colour or cursor codes, what a carriage return overwrote left out, tabs
/// as spaces, and cut at [`MAX_LINE`] characters.
fn clean_line(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim_end_matches(['\n', '\r']);
    let text = text.rsplit('\r').next().unwrap_or(text);
    let mut out = String::with_capacity(text.len().min(MAX_LINE));
    let mut count = 0;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\u{1b}' => match chars.peek() {
                // A control sequence ends at its first letter-like byte.
                Some('[') => {
                    chars.next();
                    for inner in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&inner) {
                            break;
                        }
                    }
                }
                // An operating-system command ends at BEL or ESC \.
                Some(']') => {
                    chars.next();
                    while let Some(inner) = chars.next() {
                        if inner == '\u{7}' {
                            break;
                        }
                        if inner == '\u{1b}' {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {
                    chars.next();
                }
            },
            '\t' => {
                out.push_str("    ");
                count += 4;
            }
            ch if ch.is_control() => {}
            ch => {
                out.push(ch);
                count += 1;
            }
        }
        if count >= MAX_LINE {
            out.push('…');
            break;
        }
    }
    out.truncate(out.trim_end().len());
    out
}

/// `command` ready to start: its folder, its variables, no input, a group
/// of its own, and the search path asked for.
fn configured(command: &catalog::Command, search: SearchPath) -> std::process::Command {
    let mut process = std::process::Command::new(&command.program);
    process.args(&command.args).stdin(Stdio::null());
    if let Some(dir) = &command.dir {
        process.current_dir(dir);
    }
    if search == SearchPath::UserShell && !command.env.iter().any(|(name, _)| name == "PATH") {
        if let Some(path) = user_shell_path() {
            process.env("PATH", path);
        }
    }
    for (name, value) in &command.env {
        process.env(name, value);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        process.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        process.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }
    process
}

/// The `PATH` of the user's login shell, asked once (the first start waits
/// for it, on its own thread). `None` where there is no such shell or it did
/// not answer; programs are then looked up on the launcher's own `PATH`.
fn user_shell_path() -> Option<&'static OsString> {
    static PATH: std::sync::OnceLock<Option<OsString>> = std::sync::OnceLock::new();
    PATH.get_or_init(ask_user_shell_path).as_ref()
}

#[cfg(not(unix))]
fn ask_user_shell_path() -> Option<OsString> {
    // Windows hands a program started from the desktop the user's full PATH.
    None
}

/// Runs the user's shell as a login, interactive shell (the startup files a
/// terminal reads, where version managers add their folders) and reads the
/// `PATH` it ends up with.
#[cfg(unix)]
fn ask_user_shell_path() -> Option<OsString> {
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::process::CommandExt;

    const MARK: &str = "__loadngo_launcher_path__";
    /// A startup file that waits for something gets this long.
    const PATIENCE: Duration = Duration::from_secs(8);

    let shell = std::env::var_os("SHELL")
        .map(PathBuf::from)
        .filter(|path| path.is_file())?;
    let mut process = std::process::Command::new(shell);
    process
        .args(["-l", "-i", "-c"])
        .arg(format!("printf '\\n{MARK}%s\\n' \"$PATH\""))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // A session of its own: with no terminal to take hold of, an interactive
    // shell does not stop itself waiting for one.
    // SAFETY: setsid is async-signal-safe and touches no memory.
    unsafe {
        process.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = process.spawn().ok()?;
    drop(process);
    let group = Group::of(&child);
    let mut stdout = child.stdout.take()?;
    let (done, result) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("launcher-shell-path".to_string())
        .spawn(move || {
            let mut bytes = Vec::new();
            let _ = stdout.read_to_end(&mut bytes);
            let _ = done.send(bytes);
        })
        .ok()?;
    let bytes = match result.recv_timeout(PATIENCE) {
        Ok(bytes) => bytes,
        Err(_) => {
            group.kill();
            let _ = child.wait();
            return None;
        }
    };
    let _ = child.wait();
    let mark = MARK.as_bytes();
    let line = bytes
        .split(|byte| *byte == b'\n')
        .rev()
        .find(|line| line.starts_with(mark))?;
    let path = &line[mark.len()..];
    (!path.is_empty()).then(|| OsString::from_vec(path.to_vec()))
}

/// A last resort for exits that skip [`Supervisor::shutdown`]: the Dock's
/// Quit ends a macOS app from inside the system's own code, and a signal
/// ends any program where it stands. At exit, and on the signals that ask a
/// program to end, every owned group still running is asked to stop, so a
/// server never outlives the launcher that was reading from it.
mod owned_at_exit {
    use super::Group;

    #[cfg(unix)]
    mod imp {
        use std::sync::atomic::{AtomicU32, Ordering};

        use super::Group;

        /// The owned groups running now, 0 for a free place. Fixed and
        /// lock-free, so a signal handler may read it.
        static GROUPS: [AtomicU32; 64] = [const { AtomicU32::new(0) }; 64];

        fn stop_them() {
            for place in &GROUPS {
                let group = place.load(Ordering::Relaxed);
                if group != 0 {
                    Group(group).terminate();
                }
            }
        }

        extern "C" fn at_exit() {
            stop_them();
        }

        /// Only async-signal-safe calls: kill, signal, raise.
        extern "C" fn on_signal(signal: libc::c_int) {
            stop_them();
            // SAFETY: restores the default action and sends the signal
            // again, so the launcher still ends the way it was asked to.
            unsafe {
                libc::signal(signal, libc::SIG_DFL);
                libc::raise(signal);
            }
        }

        pub fn install() {
            static ONCE: std::sync::Once = std::sync::Once::new();
            ONCE.call_once(|| {
                // SAFETY: both register functions that take what the system
                // passes and touch only the atomics above.
                unsafe {
                    libc::atexit(at_exit);
                    for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
                        libc::signal(
                            signal,
                            on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t,
                        );
                    }
                }
            });
        }

        pub fn remember(group: Group) {
            for place in &GROUPS {
                if place
                    .compare_exchange(0, group.0, Ordering::Relaxed, Ordering::Relaxed)
                    .is_ok()
                {
                    return;
                }
            }
            // More owned groups at once than places: this one is stopped
            // only by the ordinary quit.
        }

        pub fn forget(group: Group) {
            for place in &GROUPS {
                if place
                    .compare_exchange(group.0, 0, Ordering::Relaxed, Ordering::Relaxed)
                    .is_ok()
                {
                    return;
                }
            }
        }
    }

    #[cfg(not(unix))]
    mod imp {
        use super::Group;

        pub fn install() {}

        pub fn remember(_group: Group) {}

        pub fn forget(_group: Group) {}
    }

    pub use imp::{forget, install, remember};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_lines_are_cleaned_for_showing() {
        assert_eq!(clean_line(b"plain\n"), "plain");
        assert_eq!(clean_line(b"windows\r\n"), "windows");
        assert_eq!(
            clean_line(b"\x1b[32m  Compiling\x1b[0m ui-core\n"),
            "  Compiling ui-core"
        );
        assert_eq!(clean_line(b"10%\r50%\r100% done\n"), "100% done");
        assert_eq!(clean_line(b"\x1b]0;title\x07shown\n"), "shown");
        assert_eq!(clean_line(b"a\tb\n"), "a    b");
        assert_eq!(clean_line(b"   \n"), "");
        let long = "x".repeat(MAX_LINE + 50);
        let cut = clean_line(long.as_bytes());
        assert_eq!(cut.chars().count(), MAX_LINE + 1);
        assert!(cut.ends_with('…'));
    }

    #[test]
    fn log_files_are_named_after_their_entries() {
        let logs = Path::new("/logs");
        assert_eq!(log_path(logs, "Code Editor"), logs.join("code-editor.log"));
        assert_eq!(
            log_path(logs, "  Night Shift: Part 2! "),
            logs.join("night-shift-part-2.log")
        );
        assert_eq!(log_path(logs, "///"), logs.join("entry.log"));
    }

    #[cfg(unix)]
    mod running {
        use std::sync::mpsc::{channel, Receiver};

        use super::super::*;

        struct Rig {
            supervisor: Supervisor,
            woken: Receiver<()>,
            dir: tempfile::TempDir,
            seen: Vec<Event>,
        }

        fn sh(script: &str) -> catalog::Command {
            catalog::Command {
                program: "/bin/sh".to_string(),
                args: vec!["-c".to_string(), script.to_string()],
                dir: None,
                env: Vec::new(),
            }
        }

        fn entry(name: &str) -> Entry {
            Entry {
                name: name.to_string(),
                group: String::new(),
                about: String::new(),
                build: None,
                run: None,
                open: None,
                ready: None,
            }
        }

        impl Rig {
            fn new() -> Self {
                Self::with_patience(STOP_PATIENCE)
            }

            fn with_patience(patience: Duration) -> Self {
                let dir = tempfile::tempdir().unwrap();
                let (wake, woken) = channel();
                let supervisor = Supervisor::with_patience(
                    dir.path().join("logs"),
                    SearchPath::Inherited,
                    patience,
                    move || {
                        let _ = wake.send(());
                    },
                );
                Self {
                    supervisor,
                    woken,
                    dir,
                    seen: Vec::new(),
                }
            }

            /// Collects events until one matches, and returns it.
            fn until(&mut self, wanted: impl Fn(&What) -> bool) -> What {
                loop {
                    if let Some(found) = self.seen.iter().position(|event| wanted(&event.what)) {
                        return self.seen.remove(found).what;
                    }
                    self.woken
                        .recv_timeout(Duration::from_secs(20))
                        .expect("an event within 20 s");
                    self.seen.extend(self.supervisor.take_events());
                }
            }

            fn ending(&mut self) -> Ending {
                match self.until(|what| matches!(what, What::Ended(_))) {
                    What::Ended(ending) => ending,
                    _ => unreachable!(),
                }
            }
        }

        /// Whether the process is running. One that has exited but has not
        /// been collected yet (an orphan waits for the system to do that)
        /// still answers signal 0, so on Linux its state is read as well.
        fn alive(pid: i32) -> bool {
            // SAFETY: signal 0 only asks whether the process exists.
            if unsafe { libc::kill(pid, 0) } != 0 {
                return false;
            }
            !exited_uncollected(pid)
        }

        #[cfg(target_os = "linux")]
        fn exited_uncollected(pid: i32) -> bool {
            match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
                // "pid (name) state ...": the state follows the last ')'.
                Ok(stat) => matches!(
                    stat.rsplit(')')
                        .next()
                        .and_then(|rest| rest.trim_start().chars().next()),
                    Some('Z' | 'X')
                ),
                // Gone between the two looks.
                Err(_) => true,
            }
        }

        #[cfg(not(target_os = "linux"))]
        fn exited_uncollected(_pid: i32) -> bool {
            false
        }

        /// Waits for a process to have ended. The end of a group's output
        /// says its processes have let go of it, which is a moment before
        /// the system has them down as ended.
        fn ended(pid: i32) -> bool {
            let deadline = Instant::now() + Duration::from_secs(10);
            while alive(pid) {
                if Instant::now() >= deadline {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            true
        }

        #[test]
        fn a_failed_build_reports_what_it_printed_and_starts_nothing() {
            let mut rig = Rig::new();
            let marker = rig.dir.path().join("ran");
            let mut item = entry("Tool");
            item.build = Some(sh(
                "echo compiling; echo 'error: no such thing' >&2; exit 3",
            ));
            item.run = Some(sh(&format!("touch '{}'", marker.display())));
            rig.supervisor.start(1, &item);
            rig.until(|what| *what == What::Building);
            let Ending::Failed { why, tail } = rig.ending() else {
                panic!("the build fails");
            };
            assert!(why.starts_with("The build failed"), "{why}");
            assert_eq!(tail, ["compiling", "error: no such thing"]);
            assert!(!marker.exists());
        }

        #[test]
        fn a_program_runs_after_its_build_with_its_output_in_a_log() {
            let mut rig = Rig::new();
            let mut item = entry("Hello There");
            item.build = Some(sh("echo built"));
            item.run = Some(sh("echo hello from the program"));
            rig.supervisor.start(7, &item);
            rig.until(|what| *what == What::Line("built".to_string()));
            rig.until(|what| *what == What::Started);
            assert_eq!(rig.ending(), Ending::Finished);
            let log = std::fs::read_to_string(rig.dir.path().join("logs/hello-there.log")).unwrap();
            assert_eq!(log, "hello from the program\n");
        }

        #[test]
        fn a_program_that_fails_reports_the_end_of_its_log() {
            let mut rig = Rig::new();
            let mut item = entry("Crasher");
            item.run = Some(sh(
                "echo starting; echo 'panicked at main.rs' >&2; exit 101",
            ));
            rig.supervisor.start(1, &item);
            let Ending::Failed { why, tail } = rig.ending() else {
                panic!("it fails");
            };
            assert!(why.contains("101"), "{why}");
            assert_eq!(tail, ["starting", "panicked at main.rs"]);
        }

        #[test]
        fn a_missing_program_or_folder_is_named() {
            let mut rig = Rig::new();
            let mut item = entry("Nothing");
            item.run = Some(catalog::Command {
                program: "/no/such/program".to_string(),
                args: Vec::new(),
                dir: None,
                env: Vec::new(),
            });
            rig.supervisor.start(1, &item);
            let Ending::Failed { why, .. } = rig.ending() else {
                panic!("it cannot start");
            };
            assert_eq!(
                why,
                "It could not start: there is no program \"/no/such/program\""
            );

            let mut item = entry("Nowhere");
            let mut command = sh("true");
            command.dir = Some(PathBuf::from("/no/such/folder"));
            item.run = Some(command);
            rig.supervisor.start(2, &item);
            let Ending::Failed { why, .. } = rig.ending() else {
                panic!("it cannot start");
            };
            assert_eq!(
                why,
                "It could not start: there is no folder /no/such/folder"
            );
        }

        #[test]
        fn a_service_is_ready_at_its_text_and_stop_ends_all_it_started() {
            let mut rig = Rig::new();
            let pids = rig.dir.path().join("pids");
            let mut item = entry("Server");
            // The shell starts a child and waits on it, as npm does node.
            item.run = Some(sh(&format!(
                "echo warming up; sleep 300 & echo $! > '{}'; echo 'Local: http://localhost:1/'; wait",
                pids.display()
            )));
            item.open = Some("http://localhost:1/".to_string());
            item.ready = Some("Local:".to_string());
            rig.supervisor.start(3, &item);
            rig.until(|what| *what == What::Started);
            rig.until(|what| *what == What::Line("warming up".to_string()));
            rig.until(|what| *what == What::Ready);
            let child: i32 = std::fs::read_to_string(&pids)
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            assert!(alive(child));
            rig.supervisor.stop(3);
            assert_eq!(rig.ending(), Ending::Stopped);
            // The pipe closed, so the child the shell started has gone too.
            assert!(ended(child));
            // Stopping what has ended does nothing.
            rig.supervisor.stop(3);
        }

        #[test]
        fn a_service_with_no_text_to_wait_for_is_ready_at_once() {
            let mut rig = Rig::new();
            let mut item = entry("Quick");
            item.run = Some(sh("sleep 300"));
            item.open = Some("http://localhost:1/".to_string());
            rig.supervisor.start(4, &item);
            rig.until(|what| *what == What::Ready);
            rig.supervisor.stop(4);
            assert_eq!(rig.ending(), Ending::Stopped);
        }

        #[test]
        fn what_missed_the_first_request_to_stop_is_asked_again() {
            let mut rig = Rig::new();
            let mut item = entry("Straggler");
            // Stands in for a process that was being started as the request
            // arrived: the first request is taken and survived, the second
            // ends it.
            item.run = Some(sh(
                "trap 'trap - TERM' TERM; echo up; while :; do sleep 300 & wait; done",
            ));
            item.open = Some("http://localhost:1/".to_string());
            item.ready = Some("up".to_string());
            rig.supervisor.start(8, &item);
            rig.until(|what| *what == What::Ready);
            let asked = Instant::now();
            rig.supervisor.stop(8);
            assert_eq!(rig.ending(), Ending::Stopped);
            // By the second request, not by being killed at the end of the wait.
            assert!(asked.elapsed() < STOP_PATIENCE - Duration::from_secs(1));
        }

        #[test]
        fn what_will_not_stop_is_ended_outright() {
            let mut rig = Rig::with_patience(Duration::from_millis(600));
            let pids = rig.dir.path().join("pid");
            let mut item = entry("Stubborn");
            // Ignored signals stay ignored in what the shell starts.
            item.run = Some(sh(&format!(
                "trap '' TERM; echo $$ > '{}'; echo up; while :; do sleep 300; done",
                pids.display()
            )));
            item.open = Some("http://localhost:1/".to_string());
            item.ready = Some("up".to_string());
            rig.supervisor.start(9, &item);
            rig.until(|what| *what == What::Ready);
            let shell: i32 = std::fs::read_to_string(&pids)
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            rig.supervisor.stop(9);
            assert_eq!(rig.ending(), Ending::Stopped);
            assert!(ended(shell));
        }

        #[test]
        fn stopping_a_build_starts_nothing_after_it() {
            let mut rig = Rig::new();
            let marker = rig.dir.path().join("ran");
            let mut item = entry("Slow");
            item.build = Some(sh("echo building; sleep 300"));
            item.run = Some(sh(&format!("touch '{}'", marker.display())));
            rig.supervisor.start(5, &item);
            rig.until(|what| *what == What::Line("building".to_string()));
            rig.supervisor.stop(5);
            assert_eq!(rig.ending(), Ending::Stopped);
            assert!(!marker.exists());
        }

        #[test]
        fn quitting_stops_services_and_leaves_programs_running() {
            let mut rig = Rig::new();
            let pid_file = rig.dir.path().join("program-pid");
            let mut program = entry("Window");
            program.run = Some(sh(&format!(
                "echo $$ > '{}'; exec sleep 300",
                pid_file.display()
            )));
            let mut service = entry("Server");
            service.run = Some(sh("echo up; exec sleep 300"));
            service.open = Some("http://localhost:1/".to_string());
            service.ready = Some("up".to_string());
            rig.supervisor.start(1, &program);
            rig.supervisor.start(2, &service);
            rig.until(|what| *what == What::Ready);
            rig.until(|what| *what == What::Started);
            rig.until(|what| *what == What::Started);
            rig.supervisor.shutdown(Duration::from_secs(10));
            let ended = rig.until(|what| matches!(what, What::Ended(_)));
            assert_eq!(ended, What::Ended(Ending::Stopped));
            // The program writes its pid as its first act; Started can come
            // before that.
            let deadline = Instant::now() + Duration::from_secs(20);
            let pid: i32 = loop {
                let written = std::fs::read_to_string(&pid_file).unwrap_or_default();
                if let Ok(pid) = written.trim().parse() {
                    break pid;
                }
                assert!(Instant::now() < deadline, "the program wrote its pid");
                std::thread::sleep(Duration::from_millis(20));
            };
            assert!(alive(pid), "the program is left running");
            rig.supervisor.stop(1);
            assert_eq!(rig.ending(), Ending::Stopped);
        }
    }
}
