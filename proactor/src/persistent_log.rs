//! Bounded persistent logs scheduled by an existing proactor. Directory work,
//! rotation and fsync run on one sleeping file worker, never on the pump.
use crate::{CompletionPort, ProactorHandle};
use std::{
    fmt,
    fs::{self, File, OpenOptions},
    future::Future,
    io::{self, Write as _},
    path::{Path, PathBuf},
    pin::Pin,
    sync::{mpsc, Arc, Mutex},
    task::{Context, Poll, Waker},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone)]
pub struct PersistentLogConfig {
    pub directory: PathBuf,
    /// Includes the active file. At most 64 files are supported.
    pub file_count: usize,
    pub file_bytes: usize,
    pub queue_bytes: usize,
    pub record_bytes: usize,
    pub max_age: Duration,
    pub batch_delay: Duration,
}

impl PersistentLogConfig {
    pub fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            file_count: 8,
            file_bytes: 1024 * 1024,
            queue_bytes: 128 * 1024,
            record_bytes: 16 * 1024,
            max_age: Duration::from_secs(14 * 24 * 3600),
            batch_delay: Duration::from_millis(250),
        }
    }

    fn validate(&self) -> io::Result<()> {
        if !(1..=64).contains(&self.file_count)
            || self.record_bytes < 256
            || self.record_bytes > self.queue_bytes.saturating_sub(128)
            || self.queue_bytes > self.file_bytes
            || self.queue_bytes > 16 * 1024 * 1024
            || self.file_bytes > 64 * 1024 * 1024
            || self.max_age.is_zero()
            || self.batch_delay.is_zero()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid persistent log limits",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default)]
pub struct PersistentLogStatus {
    pub accepted: u64,
    pub persisted: u64,
    pub dropped: u64,
    pub truncated: u64,
    pub error: Option<String>,
}

struct State {
    buffer: Vec<u8>,
    scratch: String,
    scheduled: bool,
    closed: bool,
    in_flight: bool,
    status: PersistentLogStatus,
    waiters: Vec<Waker>,
}

type Schedule = Box<dyn Fn() -> io::Result<()> + Send + Sync>;

pub struct PersistentLog {
    config: PersistentLogConfig,
    state: Arc<Mutex<State>>,
    schedule: Schedule,
    sender: Option<mpsc::SyncSender<()>>,
    worker: Option<thread::JoinHandle<()>>,
}

impl PersistentLog {
    pub fn new<P: CompletionPort>(
        handle: ProactorHandle<P>,
        config: PersistentLogConfig,
    ) -> io::Result<Self> {
        Self::new_with_wake(handle, config, || {})
    }

    /// `wake_host` wakes a native event loop that does not wait directly on
    /// the completion port. It must not advance simulation or demand a frame.
    pub fn new_with_wake<P: CompletionPort>(
        handle: ProactorHandle<P>,
        config: PersistentLogConfig,
        wake_host: impl Fn() + Send + Sync + 'static,
    ) -> io::Result<Self> {
        config.validate()?;
        let wake_host = Arc::new(wake_host);
        let state = Arc::new(Mutex::new(State {
            buffer: Vec::with_capacity(config.queue_bytes),
            scratch: String::with_capacity(config.record_bytes),
            scheduled: false,
            closed: false,
            in_flight: false,
            status: PersistentLogStatus::default(),
            waiters: Vec::new(),
        }));
        let (sender, receiver) = mpsc::sync_channel(1);
        let worker_state = Arc::clone(&state);
        let worker_config = config.clone();
        let worker_handle = handle.clone();
        let worker_sender = sender.clone();
        let worker_wake = Arc::clone(&wake_host);
        let worker = thread::Builder::new()
            .name("loadngo-log-io".into())
            .spawn(move || {
                let mut store = LogStore::new(worker_config.clone());
                let mut buffer = Vec::with_capacity(worker_config.queue_bytes);
                while receiver.recv().is_ok() {
                    let accepted = {
                        let mut state = worker_state.lock().unwrap();
                        if state.in_flight && !state.closed {
                            continue;
                        }
                        std::mem::swap(&mut buffer, &mut state.buffer);
                        state.in_flight = true;
                        state.status.accepted
                    };
                    if buffer.is_empty() {
                        let mut state = worker_state.lock().unwrap();
                        state.in_flight = false;
                        if state.closed {
                            break;
                        }
                        continue;
                    }
                    let result = store.append(&buffer).map_err(|error| error.to_string());
                    buffer.clear();
                    let completed = Arc::clone(&worker_state);
                    let sender = worker_sender.clone();
                    // This posts a completion, not the blocking file operation.
                    if let Err(error) = worker_handle.enqueue_work(move |_| {
                        complete(&completed, accepted, result);
                        let state = completed.lock().unwrap();
                        if !state.closed && !state.buffer.is_empty() && state.status.error.is_none()
                        {
                            let _ = sender.try_send(());
                        }
                    }) {
                        complete(&worker_state, accepted, Err(error.to_string()));
                    }
                    worker_wake();
                    let state = worker_state.lock().unwrap();
                    if state.closed && state.buffer.is_empty() {
                        break;
                    }
                }
            })?;
        let scheduled_state = Arc::clone(&state);
        let scheduled_sender = sender.clone();
        let delay = config.batch_delay;
        let schedule = Box::new(move || {
            let state = Arc::clone(&scheduled_state);
            let sender = scheduled_sender.clone();
            let result = handle
                .defer_for(delay, crate::CompletionKind::Timer, 0, move |_| {
                    state.lock().unwrap().scheduled = false;
                    let _ = sender.try_send(());
                })
                .map(|_| ());
            wake_host();
            result
        });
        Ok(Self {
            config,
            state,
            schedule,
            sender: Some(sender),
            worker: Some(worker),
        })
    }

    /// Formats into reused bounded storage. Returns false when the record is
    /// dropped or logging is disabled. Never waits for disk I/O.
    pub fn record(&self, level: &str, message: fmt::Arguments<'_>) -> bool {
        let mut state = self.state.lock().unwrap();
        if state.closed || state.status.error.is_some() {
            return false;
        }
        state.scratch.clear();
        let mut output = LimitedText {
            text: &mut state.scratch,
            limit: self.config.record_bytes - 16,
            truncated: false,
        };
        let _ = fmt::write(&mut output, message);
        let truncated = output.truncated;
        if truncated {
            state.scratch.push_str(" [truncated]");
            state.status.truncated += 1;
        }
        // Header space is reserved, so even the largest record cannot grow
        // the buffer beyond queue_bytes.
        if state.buffer.len() + state.scratch.len() + 128 > self.config.queue_bytes {
            state.status.dropped += 1;
            return false;
        }
        state.status.accepted += 1;
        let sequence = state.status.accepted;
        let dropped = state.status.dropped;
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let State {
            buffer, scratch, ..
        } = &mut *state;
        // Bound level too: callers cannot grow a header through this field.
        let level = match level {
            "error" => "error",
            "warn" => "warn",
            "debug" => "debug",
            _ => "info",
        };
        let _ = writeln!(
            buffer,
            "unix_ms={timestamp} seq={sequence} level={level} dropped={dropped}"
        );
        buffer.extend_from_slice(scratch.as_bytes());
        if !scratch.ends_with('\n') {
            buffer.push(b'\n');
        }
        let schedule = !state.scheduled;
        state.scheduled = true;
        drop(state);
        if schedule {
            if let Err(error) = (self.schedule)() {
                complete(&self.state, 0, Err(error.to_string()));
                return false;
            }
        }
        true
    }

    pub fn status(&self) -> PersistentLogStatus {
        self.state.lock().unwrap().status.clone()
    }

    /// Flush completion is delivered through the supplied proactor. The host
    /// must keep pumping it while this future is pending.
    pub fn flush(&self) -> LogFlush {
        let target = self.state.lock().unwrap().status.accepted;
        if let Some(sender) = &self.sender {
            let _ = sender.try_send(());
        }
        LogFlush {
            state: Arc::clone(&self.state),
            target,
        }
    }

    /// Drains the bounded queue and joins the file worker. Safe during host
    /// teardown: disk work never waits for the pump to dispatch its completion.
    pub fn close(&mut self) {
        if self.worker.is_none() {
            return;
        }
        self.state.lock().unwrap().closed = true;
        // Drop the scheduler's sender before joining, including any deferred
        // callback clones: explicitly signal shutdown instead of relying on EOF.
        if let Some(sender) = self.sender.take() {
            let _ = sender.send(());
        }
        // The worker checks closed after its final batch; wake with an empty
        // batch too, for a logger that never received a message.
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for PersistentLog {
    fn drop(&mut self) {
        self.close();
    }
}

fn complete(state: &Mutex<State>, accepted: u64, result: Result<(), String>) {
    let mut state = state.lock().unwrap();
    state.in_flight = false;
    match result {
        Ok(()) => state.status.persisted = state.status.persisted.max(accepted),
        Err(error) => {
            if state.status.error.is_none() {
                std::eprintln!("loadngo persistent logging disabled: {error}");
                state.status.dropped +=
                    state.status.accepted.saturating_sub(state.status.persisted);
                state.buffer.clear();
                state.status.error = Some(error);
            }
        }
    }
    let waiters = std::mem::take(&mut state.waiters);
    drop(state);
    for waker in waiters {
        waker.wake();
    }
}

pub struct LogFlush {
    state: Arc<Mutex<State>>,
    target: u64,
}
impl Future for LogFlush {
    type Output = Result<(), String>;
    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state = self.state.lock().unwrap();
        if let Some(error) = &state.status.error {
            return Poll::Ready(Err(error.clone()));
        }
        if state.status.persisted >= self.target {
            return Poll::Ready(Ok(()));
        }
        if !state
            .waiters
            .iter()
            .any(|waker| waker.will_wake(context.waker()))
        {
            if state.waiters.len() >= 32 {
                return Poll::Ready(Err("too many concurrent log flush waiters".into()));
            }
            state.waiters.push(context.waker().clone());
        }
        Poll::Pending
    }
}

struct LimitedText<'a> {
    text: &'a mut String,
    limit: usize,
    truncated: bool,
}
impl fmt::Write for LimitedText<'_> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let mut available = self.limit.saturating_sub(self.text.len()).min(text.len());
        while !text.is_char_boundary(available) {
            available -= 1;
        }
        self.text.push_str(&text[..available]);
        self.truncated |= available < text.len();
        Ok(())
    }
}

struct LogStore {
    config: PersistentLogConfig,
    file: Option<File>,
    lock: Option<File>,
    length: usize,
}
impl LogStore {
    fn new(config: PersistentLogConfig) -> Self {
        Self {
            config,
            file: None,
            lock: None,
            length: 0,
        }
    }
    fn path(&self, index: usize) -> PathBuf {
        self.config.directory.join(format!("log.{index}.txt"))
    }
    fn append(&mut self, bytes: &[u8]) -> io::Result<()> {
        if bytes.len() > self.config.file_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "log batch exceeds file limit",
            ));
        }
        if self.file.is_none() {
            fs::create_dir_all(&self.config.directory)?;
            if self.lock.is_none() {
                let lock = OpenOptions::new()
                    .create(true)
                    .truncate(false)
                    .read(true)
                    .write(true)
                    .open(self.config.directory.join("writer.lock"))?;
                lock.try_lock().map_err(io::Error::other)?;
                self.lock = Some(lock);
            }
            self.prune()?;
            self.length = fs::metadata(self.path(0)).map_or(0, |metadata| {
                usize::try_from(metadata.len()).unwrap_or(usize::MAX)
            });
            self.file = Some(
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(self.path(0))?,
            );
        }
        if bytes.len() > self.config.file_bytes.saturating_sub(self.length) {
            self.file.take();
            self.prune()?;
            remove_if_present(&self.path(self.config.file_count - 1))?;
            for index in (1..self.config.file_count).rev() {
                let previous = self.path(index - 1);
                if previous.exists() {
                    fs::rename(previous, self.path(index))?;
                }
            }
            self.file = Some(
                OpenOptions::new()
                    .create(true)
                    .truncate(true)
                    .write(true)
                    .open(self.path(0))?,
            );
            self.length = 0;
        }
        let file = self.file.as_mut().unwrap();
        file.write_all(bytes)?;
        file.sync_data()?;
        self.length += bytes.len();
        Ok(())
    }
    fn prune(&self) -> io::Result<()> {
        let now = SystemTime::now();
        for index in 0..64 {
            let path = self.path(index);
            let metadata = match fs::metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            let expired = metadata
                .modified()
                .ok()
                .and_then(|modified| now.duration_since(modified).ok())
                .is_some_and(|age| age > self.config.max_age);
            if index >= self.config.file_count
                || metadata.len() > self.config.file_bytes as u64
                || expired
            {
                remove_if_present(&path)?;
            }
        }
        Ok(())
    }
}
fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChannelPort, Proactor};
    use std::sync::atomic::{AtomicU64, Ordering};

    struct TestDirectory(PathBuf);
    impl TestDirectory {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let name = format!(
                "loadngo-log-test-{}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            );
            let path = std::env::temp_dir().join(name);
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn small_config(path: PathBuf) -> PersistentLogConfig {
        PersistentLogConfig {
            directory: path,
            file_count: 3,
            file_bytes: 1024,
            queue_bytes: 512,
            record_bytes: 256,
            max_age: Duration::from_secs(3600),
            batch_delay: Duration::from_millis(2),
        }
    }

    #[test]
    fn flush_is_acknowledged_only_by_the_existing_proactor_and_shutdown_drains() {
        let directory = TestDirectory::new();
        let proactor = Proactor::new(ChannelPort::new());
        let mut logger =
            PersistentLog::new(proactor.handle(), small_config(directory.0.clone())).unwrap();
        assert!(logger.record("info", format_args!("a full playtest report\nsecond line")));
        let mut flush = logger.flush();
        logger.close();
        let mut context = Context::from_waker(Waker::noop());
        assert!(Pin::new(&mut flush).poll(&mut context).is_pending());
        assert_eq!(logger.status().persisted, 0);
        assert!(fs::read_to_string(directory.0.join("log.0.txt"))
            .unwrap()
            .contains("a full playtest report\nsecond line"));
        for _ in 0..16 {
            proactor.run_ready().unwrap();
        }
        assert!(matches!(
            Pin::new(&mut flush).poll(&mut context),
            Poll::Ready(Ok(()))
        ));
        assert_eq!(logger.status().persisted, 1);
        assert!(!logger.record("info", format_args!("closed")));
    }

    #[test]
    fn proactor_deadline_flushes_without_application_polling_or_a_timer_thread() {
        let directory = TestDirectory::new();
        let proactor = Proactor::new(ChannelPort::new());
        let logger =
            PersistentLog::new(proactor.handle(), small_config(directory.0.clone())).unwrap();
        logger.record("info", format_args!("deadline batch"));
        let expired = Arc::new(Mutex::new(false));
        let timeout = Arc::clone(&expired);
        proactor
            .handle()
            .defer_for(
                Duration::from_secs(2),
                crate::CompletionKind::Timer,
                0,
                move |_| *timeout.lock().unwrap() = true,
            )
            .unwrap();
        while logger.status().persisted == 0 {
            proactor.run_once().unwrap();
            assert!(!*expired.lock().unwrap(), "scheduled log never persisted");
        }
        assert!(fs::read_to_string(directory.0.join("log.0.txt"))
            .unwrap()
            .contains("deadline batch"));
    }

    #[test]
    fn shutdown_drains_records_queued_while_the_previous_batch_completes() {
        let directory = TestDirectory::new();
        let proactor = Proactor::new(ChannelPort::new());
        let (arrived, arrival) = mpsc::sync_channel(1);
        let (resume, resumed) = mpsc::sync_channel(1);
        let resumed = Mutex::new(resumed);
        let paused = std::sync::atomic::AtomicBool::new(false);
        let mut logger = PersistentLog::new_with_wake(
            proactor.handle(),
            small_config(directory.0.clone()),
            move || {
                if thread::current().name() == Some("loadngo-log-io")
                    && !paused.swap(true, Ordering::SeqCst)
                {
                    arrived.send(()).unwrap();
                    resumed.lock().unwrap().recv().unwrap();
                }
            },
        )
        .unwrap();
        logger.record("info", format_args!("first batch"));
        let _flush = logger.flush();
        arrival.recv_timeout(Duration::from_secs(2)).unwrap();
        logger.record("info", format_args!("queued during completion"));
        // Reproduce close's flag transition before letting the worker resume.
        logger.state.lock().unwrap().closed = true;
        resume.send(()).unwrap();
        logger.close();
        let content = fs::read_to_string(directory.0.join("log.0.txt")).unwrap();
        assert!(content.contains("first batch"));
        assert!(content.contains("queued during completion"));
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn native_completion_port_flushes_and_wakes_the_host() {
        let directory = TestDirectory::new();
        #[cfg(target_os = "macos")]
        let port = crate::KqueuePort::new().unwrap();
        #[cfg(target_os = "linux")]
        let port = crate::EpollPort::new().unwrap();
        let proactor = Proactor::new(port);
        let wakes = Arc::new(AtomicU64::new(0));
        let observed = Arc::clone(&wakes);
        let mut logger = PersistentLog::new_with_wake(
            proactor.handle(),
            small_config(directory.0.clone()),
            move || {
                observed.fetch_add(1, Ordering::SeqCst);
            },
        )
        .unwrap();
        logger.record("info", format_args!("native completion"));
        let expired = Arc::new(Mutex::new(false));
        let timeout = Arc::clone(&expired);
        proactor
            .handle()
            .defer_for(
                Duration::from_secs(2),
                crate::CompletionKind::Timer,
                0,
                move |_| *timeout.lock().unwrap() = true,
            )
            .unwrap();
        while logger.status().persisted == 0 {
            proactor.run_once().unwrap();
            assert!(!*expired.lock().unwrap(), "native completion timed out");
        }
        logger.close();
        assert!(wakes.load(Ordering::SeqCst) >= 2);
    }

    #[test]
    fn rotation_size_age_and_reduced_retention_are_enforced() {
        let directory = TestDirectory::new();
        let config = small_config(directory.0.clone());
        fs::write(directory.0.join("log.0.txt"), vec![b'x'; 1500]).unwrap();
        fs::write(directory.0.join("log.2.txt"), b"expired").unwrap();
        File::options()
            .write(true)
            .open(directory.0.join("log.2.txt"))
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(UNIX_EPOCH))
            .unwrap();
        fs::write(directory.0.join("log.60.txt"), b"old higher quota").unwrap();
        fs::write(directory.0.join("keep-me.txt"), b"unrelated").unwrap();
        let mut store = LogStore::new(config.clone());
        for index in 0..20 {
            let mut record = format!("record {index:02}\n").into_bytes();
            record.resize(600, b'.');
            store.append(&record).unwrap();
        }
        let mut total = 0;
        for index in 0..64 {
            if let Ok(metadata) = fs::metadata(store.path(index)) {
                assert!(index < config.file_count);
                assert!(metadata.len() <= config.file_bytes as u64);
                total += metadata.len();
            }
        }
        assert!(total <= (config.file_bytes * config.file_count) as u64);
        assert!(fs::read_to_string(store.path(0))
            .unwrap()
            .starts_with("record 19"));
        assert_eq!(
            fs::read(directory.0.join("keep-me.txt")).unwrap(),
            b"unrelated"
        );
    }

    #[test]
    fn queue_and_unicode_records_are_bounded_and_losses_are_counted() {
        let directory = TestDirectory::new();
        let proactor = Proactor::new(ChannelPort::new());
        let mut config = small_config(directory.0.clone());
        config.batch_delay = Duration::from_secs(10);
        let mut logger = PersistentLog::new(proactor.handle(), config.clone()).unwrap();
        for _ in 0..20 {
            logger.record("info", format_args!("{}", "猫".repeat(500)));
        }
        assert!(logger.status().dropped > 0);
        assert!(logger.status().truncated > 0);
        assert!(logger.state.lock().unwrap().buffer.len() <= config.queue_bytes);
        assert!(logger.state.lock().unwrap().scratch.len() <= config.record_bytes);
        logger.close();
        let content = fs::read_to_string(directory.0.join("log.0.txt")).unwrap();
        assert!(content.contains("[truncated]"));
    }

    #[test]
    fn disk_failure_is_reported_and_logging_stops_without_breaking_the_pump() {
        let directory = TestDirectory::new();
        let path = directory.0.join("not-a-directory");
        fs::write(&path, b"occupied").unwrap();
        let proactor = Proactor::new(ChannelPort::new());
        let mut logger = PersistentLog::new(proactor.handle(), small_config(path)).unwrap();
        logger.record("info", format_args!("cannot persist"));
        let mut flush = logger.flush();
        logger.close();
        for _ in 0..16 {
            proactor.run_ready().unwrap();
        }
        assert!(matches!(
            Pin::new(&mut flush).poll(&mut Context::from_waker(Waker::noop())),
            Poll::Ready(Err(_))
        ));
        assert!(logger.status().error.is_some());
        assert!(!logger.record("info", format_args!("no retry storm")));
    }

    #[test]
    fn two_process_writers_cannot_rotate_the_same_log_directory() {
        let directory = TestDirectory::new();
        let config = small_config(directory.0.clone());
        let mut first = LogStore::new(config.clone());
        first.append(b"first").unwrap();
        let mut second = LogStore::new(config);
        assert!(second.append(b"second").is_err());
        assert_eq!(fs::read(first.path(0)).unwrap(), b"first");
        drop(first);
        second.append(b"second").unwrap();
        assert_eq!(fs::read(second.path(0)).unwrap(), b"firstsecond");
    }
}
