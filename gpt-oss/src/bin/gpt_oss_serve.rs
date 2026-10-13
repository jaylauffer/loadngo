//! `gpt_oss_serve`: holds gpt-oss on this machine and answers calls from local programs
//! (the Zhoenus talking head first) on a Unix socket, one JSON line per call
//! (`loadngo_gpt_oss::call`). The model is named by its BLAKE3 hash, restored from the
//! Archive CAS once and hashed again at every launch (`data::model_store`). Run with
//! `--help` for the options.
//!
//! The socket's directory is made 0700, and a connection from another user is closed:
//! calls from this machine carry no token (`docs/LOADNGO_NODE.md`). Connections are
//! accepted, read and answered on the proactor; the model runs on one worker thread,
//! one call at a time, with a bounded queue. Idle, the server waits in the kernel.

#[cfg(unix)]
fn main() {
    serve::main();
}

#[cfg(not(unix))]
fn main() {
    eprintln!("gpt_oss_serve: needs Unix-domain sockets, which this build does not have yet");
    std::process::exit(2);
}

#[cfg(unix)]
mod serve {
    use std::collections::HashMap;
    use std::fs;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    use data::archive_cas::ArchiveObject;
    use data::cli::{read_args, ArgDoc, Usage};
    use data::model_store::{self, ModelLocation, ModelStore};
    use loadngo_gpt_oss::call::{self, Reply, Status, MAX_CALL_BYTES};
    use loadngo_gpt_oss::chat::{Conversation, Message, Reasoning};
    use loadngo_gpt_oss::engine::Engine;
    use loadngo_gpt_oss::model::Model;
    use loadngo_gpt_oss::tokenizer::Tokenizer;
    use loadngo_proactor::{
        new_platform_proactor, AcceptResult, IoBuf, IoResult, PlatformPort, ProactorHandle,
    };
    use loadngo_weights::gguf;

    const USAGE: Usage = Usage {
        bin: "gpt_oss_serve",
        invocation: "cargo run --release -p loadngo-gpt-oss --bin gpt_oss_serve --",
        about: "holds gpt-oss on this machine and answers calls from local programs on a Unix \
                socket, one JSON line per call (loadngo_gpt_oss::call)",
        args: &[
            ArgDoc::optional(
                "--socket",
                "PATH",
                "where to listen (default ~/.loadngo/node/gpt-oss.sock; its directory is made 0700)",
            ),
            ArgDoc::optional(
                "--cas-root",
                "PATH",
                "Archive CAS root to restore the model from (default: any attached root holding it)",
            ),
            ArgDoc::optional(
                "--model-hash",
                "HEX",
                "the model's BLAKE3 hash, with --model-bytes (default gpt-oss-20b)",
            ),
            ArgDoc::optional("--model-bytes", "N", "the model's size in bytes"),
            ArgDoc::optional("--context", "N", "positions per call (default 4096)"),
            ArgDoc::optional("--max-tokens", "N", "tokens per answer at most (default 1024)"),
            ArgDoc::optional(
                "--queue",
                "N",
                "calls answered or waiting at once; more get status busy (default 4)",
            ),
            ArgDoc::optional("--reasoning", "R", "low, medium or high (default low)"),
            ArgDoc::switch("--cpu", "the CPU reference path instead of the GPU (about 2 tokens/s)"),
            ArgDoc::switch(
                "--dry-run",
                "say where the model would come from and where it would listen; load nothing",
            ),
        ],
        examples: &[
            "cargo run --release -p loadngo-gpt-oss --bin gpt_oss_serve",
            "cargo run --release -p loadngo-gpt-oss --bin gpt_oss_serve -- --dry-run",
        ],
        notes: &[
            "A call: {\"call_id\": 7, \"capability\": \"chat\", \"deadline_ms\": 30000, \"body\": \
             {\"instructions\": \"...\", \"message\": \"...\"}}; the reply: {\"call_id\": 7, \
             \"status\": \"ok\", \"body\": {\"text\": \"...\"}}. Status is ok, expired, busy or error.",
            "Prints a line starting gpt_oss_serve_listening once it answers calls. SIGTERM or \
             SIGINT stop it and remove the socket.",
            "The model is restored from the Archive CAS on first use and hashed at every launch; a \
             cached copy that does not match is refused.",
        ],
    };

    struct Options {
        socket: PathBuf,
        store: ModelStore,
        context: usize,
        max_tokens: usize,
        queue: usize,
        reasoning: Reasoning,
        cpu: bool,
        dry_run: bool,
    }

    fn fail(message: &str) -> ! {
        eprintln!("gpt_oss_serve: {message}; {}", USAGE.hint());
        std::process::exit(2);
    }

    fn options() -> Options {
        let args = read_args(&USAGE, false);
        let mut store = ModelStore::new(model_store::gpt_oss_20b());
        let mut socket = default_socket();
        let (mut hash, mut bytes) = (None, None);
        let (mut context, mut max_tokens, mut queue) = (4096, 1024, 4);
        let (mut reasoning, mut cpu, mut dry_run) = (Reasoning::Low, false, false);
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            let mut value = || {
                args.next()
                    .unwrap_or_else(|| fail(&format!("{arg} needs a value")))
            };
            let number = |text: String, what: &str| -> usize {
                text.parse()
                    .ok()
                    .filter(|n| *n > 0)
                    .unwrap_or_else(|| fail(&format!("{what} must be a positive number")))
            };
            match arg.as_str() {
                "--socket" => socket = PathBuf::from(value()),
                "--cas-root" => store.cas_root = Some(PathBuf::from(value())),
                "--model-hash" => hash = Some(value()),
                "--model-bytes" => bytes = Some(value()),
                "--context" => context = number(value(), "--context"),
                "--max-tokens" => max_tokens = number(value(), "--max-tokens"),
                "--queue" => queue = number(value(), "--queue"),
                "--reasoning" => {
                    reasoning = Reasoning::parse(&value())
                        .unwrap_or_else(|| fail("--reasoning is low, medium or high"));
                }
                "--cpu" => cpu = true,
                "--dry-run" => dry_run = true,
                other => fail(&format!("unknown option {other}")),
            }
        }
        match (hash, bytes) {
            (None, None) => {}
            (Some(hash), Some(bytes)) => {
                store.model = ArchiveObject {
                    hash: hash
                        .parse()
                        .unwrap_or_else(|_| fail("--model-hash is not a BLAKE3 hash")),
                    size: bytes
                        .parse()
                        .unwrap_or_else(|_| fail("--model-bytes is not a number")),
                };
            }
            _ => fail("--model-hash and --model-bytes go together"),
        }
        Options {
            socket,
            store,
            context,
            max_tokens,
            queue,
            reasoning,
            cpu,
            dry_run,
        }
    }

    /// `~/.loadngo/node/gpt-oss.sock`.
    fn default_socket() -> PathBuf {
        std::env::var_os("HOME")
            .map_or_else(|| PathBuf::from("."), PathBuf::from)
            .join(".loadngo/node/gpt-oss.sock")
    }

    /// One call waiting for the model.
    struct Job {
        call_id: u64,
        chat: call::Chat,
        deadline: Instant,
        connection: u64,
    }

    /// Calls answered or waiting, for the queue limit.
    static PENDING: AtomicUsize = AtomicUsize::new(0);

    pub fn main() {
        let o = options();
        if o.dry_run {
            let from = match o.store.locate() {
                Ok(ModelLocation::Cached(path)) => format!("cached: {}", path.display()),
                Ok(ModelLocation::InCas(root)) => format!("cas: {}", root.display()),
                Err(e) => format!("not found: {e:#}"),
            };
            println!("model {} ({from})", o.store.model.hash);
            println!("socket {}", o.socket.display());
            return;
        }
        if let Err(e) = run(&o) {
            eprintln!("gpt_oss_serve: {e}");
            std::process::exit(1);
        }
    }

    fn run(o: &Options) -> Result<(), String> {
        prepare_socket_dir(&o.socket)?;
        // The model loads on the worker, which keeps it: one call at a time.
        let (jobs, waiting) = mpsc::channel::<Job>();
        let (started, ready) = mpsc::channel::<Result<String, String>>();
        let proactor =
            new_platform_proactor().map_err(|e| format!("cannot start the proactor: {e}"))?;
        let handle = proactor.handle();
        let server = Arc::new(Server {
            handle: handle.clone(),
            connections: Mutex::new(HashMap::new()),
            next_connection: AtomicU64::new(1),
            jobs: Mutex::new(jobs),
            queue: o.queue,
            uid: current_uid(),
        });
        {
            let (store, context, cpu, reasoning, max_tokens) =
                (o.store.clone(), o.context, o.cpu, o.reasoning, o.max_tokens);
            let server = Arc::clone(&server);
            thread::Builder::new()
                .name("gpt-oss model".into())
                .spawn(move || {
                    worker(
                        &store, context, cpu, reasoning, max_tokens, &started, &waiting, &server,
                    )
                })
                .map_err(|e| format!("cannot start the model thread: {e}"))?;
        }
        let engine = ready
            .recv()
            .map_err(|_| "the model thread stopped".to_owned())??;

        let listener = UnixListener::bind(&o.socket)
            .map_err(|e| format!("cannot listen at {}: {e}", o.socket.display()))?;
        fs::set_permissions(&o.socket, fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("cannot set {}'s mode: {e}", o.socket.display()))?;
        listener
            .set_nonblocking(true)
            .map_err(|e| format!("cannot make the socket non-blocking: {e}"))?;
        let stop = stop_on_signals(&handle)?;
        server.accept(listener.as_raw_fd());
        println!(
            "gpt_oss_serve_listening socket={} model={} engine={engine}",
            o.socket.display(),
            o.store.model.hash
        );
        let result = proactor
            .run_until_stopped()
            .map_err(|e| format!("the proactor failed: {e}"));
        drop(stop);
        drop(listener);
        let _ = fs::remove_file(&o.socket);
        eprintln!("gpt_oss_serve: stopped");
        result
    }

    /// Makes the socket's directory (0700) and clears a socket left by a server that is
    /// gone; refuses one that still answers.
    fn prepare_socket_dir(socket: &Path) -> Result<(), String> {
        let dir = socket
            .parent()
            .filter(|d| !d.as_os_str().is_empty())
            .ok_or_else(|| format!("{} has no directory", socket.display()))?;
        fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("cannot make {} 0700: {e}", dir.display()))?;
        if socket.exists() {
            if UnixStream::connect(socket).is_ok() {
                return Err(format!("another server answers at {}", socket.display()));
            }
            fs::remove_file(socket)
                .map_err(|e| format!("cannot remove the stale {}: {e}", socket.display()))?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn worker(
        store: &ModelStore,
        context: usize,
        cpu: bool,
        reasoning: Reasoning,
        max_tokens: usize,
        started: &mpsc::Sender<Result<String, String>>,
        waiting: &mpsc::Receiver<Job>,
        server: &Arc<Server>,
    ) {
        let loaded = load(store, context, cpu);
        let (mut engine, tokenizer) = match loaded {
            Ok(loaded) => loaded,
            Err(e) => {
                let _ = started.send(Err(e));
                return;
            }
        };
        let _ = started.send(Ok(engine.name()));
        // Parked here until a call comes; the channel closes when the server stops.
        while let Ok(job) = waiting.recv() {
            let reply = answer(&mut engine, &tokenizer, reasoning, max_tokens, &job);
            let server_for_reply = Arc::clone(server);
            let connection = job.connection;
            if server
                .handle
                .enqueue_work(move |_| server_for_reply.reply_from_model(connection, &reply))
                .is_err()
            {
                break;
            }
        }
    }

    /// The model, verified by hash while it loads, and its engine.
    fn load(store: &ModelStore, context: usize, cpu: bool) -> Result<(Engine, Tokenizer), String> {
        let started = Instant::now();
        let path = match store.locate().map_err(|e| format!("{e:#}"))? {
            ModelLocation::Cached(path) => path,
            ModelLocation::InCas(_) => store.resolve().map_err(|e| format!("{e:#}"))?,
        };
        // A cached copy is hashed beside the load; nothing is answered until it matches.
        let (tokenizer, model) = thread::scope(|scope| {
            let check = scope.spawn(|| model_store::verify_file(&path, store.model));
            let (file, _) = gguf::open(&path).map_err(|e| e.to_string())?;
            let tokenizer = Tokenizer::from_gguf(&file).map_err(|e| e.to_string())?;
            let model = Model::load(&path).map_err(|e| e.to_string())?;
            check
                .join()
                .map_err(|_| "the hash thread failed".to_owned())?
                .map_err(|e| format!("{e:#}"))?;
            Ok::<_, String>((tokenizer, model))
        })?;
        let engine = Engine::new(model, !cpu, context)?;
        eprintln!(
            "gpt_oss_serve_model hash={} path={} verified (BLAKE3) loaded in {:.1}s on {}",
            store.model.hash,
            path.display(),
            started.elapsed().as_secs_f64(),
            engine.name()
        );
        Ok((engine, tokenizer))
    }

    fn answer(
        engine: &mut Engine,
        tokenizer: &Tokenizer,
        reasoning: Reasoning,
        max_tokens: usize,
        job: &Job,
    ) -> Reply {
        if Instant::now() >= job.deadline {
            return Reply::failed(
                job.call_id,
                Status::Expired,
                "the deadline passed while it waited",
            );
        }
        let mut conversation = Conversation::new(loadngo_inference::agent::clock::now().date);
        conversation.reasoning = reasoning;
        conversation.instructions = job.chat.instructions.clone();
        conversation.messages = vec![Message::User(job.chat.message.clone())];
        let started = Instant::now();
        match engine.answer(tokenizer, &conversation, max_tokens, job.deadline) {
            Ok(answered) => {
                eprintln!(
                    "gpt_oss_serve_call call_id={} tokens={} seconds={:.2} expired={} complete={}",
                    job.call_id,
                    answered.tokens,
                    started.elapsed().as_secs_f64(),
                    answered.expired,
                    answered.reply.complete
                );
                if answered.expired {
                    Reply::failed(
                        job.call_id,
                        Status::Expired,
                        "the deadline passed while it answered",
                    )
                } else if answered.reply.answer.trim().is_empty() {
                    Reply::failed(
                        job.call_id,
                        Status::Error,
                        &format!("no answer within {max_tokens} tokens"),
                    )
                } else {
                    Reply::text(job.call_id, answered.reply.answer.trim())
                }
            }
            Err(e) => Reply::failed(job.call_id, Status::Error, &e),
        }
    }

    /// One accepted connection: what it has sent so far, and its socket.
    struct Connection {
        fd: OwnedFd,
        read: Vec<u8>,
    }

    struct Server {
        handle: ProactorHandle<PlatformPort>,
        connections: Mutex<HashMap<u64, Connection>>,
        next_connection: AtomicU64,
        jobs: Mutex<mpsc::Sender<Job>>,
        queue: usize,
        uid: libc::uid_t,
    }

    /// Bytes read from a connection at a time.
    const READ_BYTES: usize = 4096;

    impl Server {
        fn accept(self: &Arc<Self>, listener: RawFd) {
            let server = Arc::clone(self);
            if let Err(e) = self.handle.accept(listener, move |result: AcceptResult| {
                server.accepted(listener, result)
            }) {
                eprintln!("gpt_oss_serve: cannot accept: {e}");
                let _ = self.handle.stop();
            }
        }

        fn accepted(self: &Arc<Self>, listener: RawFd, result: AcceptResult) {
            if !self.handle.is_running() {
                return;
            }
            self.accept(listener);
            let accepted = match result {
                Ok(accepted) => accepted,
                Err(e) => {
                    eprintln!("gpt_oss_serve: accept failed: {e}");
                    return;
                }
            };
            // SAFETY: accept handed us this new descriptor; nothing else owns it.
            let fd = unsafe { OwnedFd::from_raw_fd(accepted.new_fd) };
            match peer_uid(fd.as_raw_fd()) {
                Ok(uid) if uid == self.uid => {}
                Ok(uid) => {
                    eprintln!("gpt_oss_serve: closed a connection from user {uid}");
                    return;
                }
                Err(e) => {
                    eprintln!("gpt_oss_serve: cannot read a connection's user: {e}");
                    return;
                }
            }
            let id = self.next_connection.fetch_add(1, Ordering::Relaxed);
            let raw = fd.as_raw_fd();
            self.connections
                .lock()
                .expect("connections poisoned")
                .insert(
                    id,
                    Connection {
                        fd,
                        read: Vec::new(),
                    },
                );
            self.receive(id, raw, IoBuf::with_capacity(READ_BYTES));
        }

        fn receive(self: &Arc<Self>, id: u64, raw: RawFd, buf: IoBuf) {
            let server = Arc::clone(self);
            if self
                .handle
                .recv(raw, buf, move |result: IoResult| {
                    server.received(id, raw, result)
                })
                .is_err()
            {
                self.close(id);
            }
        }

        fn received(self: &Arc<Self>, id: u64, raw: RawFd, result: IoResult) {
            if !self.handle.is_running() {
                return;
            }
            let Ok(transfer) = result else {
                self.close(id);
                return;
            };
            let count = transfer.bytes_transferred as usize;
            if count == 0 {
                // The caller hung up before finishing its line.
                self.close(id);
                return;
            }
            let mut bytes = transfer.buf.into_vec();
            let line = {
                let mut connections = self.connections.lock().expect("connections poisoned");
                let Some(connection) = connections.get_mut(&id) else {
                    return;
                };
                connection
                    .read
                    .extend_from_slice(&bytes[..count.min(bytes.len())]);
                match connection.read.iter().position(|&b| b == b'\n') {
                    Some(end) => Some(connection.read[..end].to_vec()),
                    None if connection.read.len() > MAX_CALL_BYTES => Some(Vec::new()),
                    None => None,
                }
            };
            let Some(line) = line else {
                bytes.resize(READ_BYTES, 0);
                self.receive(id, raw, IoBuf::from_vec(bytes));
                return;
            };
            if line.is_empty() {
                self.reply(
                    id,
                    &Reply::failed(0, Status::Error, "no call within 64 KiB"),
                );
                return;
            }
            self.dispatch(id, &line);
        }

        fn dispatch(self: &Arc<Self>, id: u64, line: &[u8]) {
            let received = Instant::now();
            let (call, chat) = match call::read(line) {
                Ok(read) => read,
                Err(refusal) => {
                    self.reply(id, &refusal);
                    return;
                }
            };
            if call.deadline_ms == 0 {
                self.reply(
                    id,
                    &Reply::failed(call.call_id, Status::Expired, "the deadline had passed"),
                );
                return;
            }
            if PENDING.fetch_add(1, Ordering::AcqRel) >= self.queue {
                PENDING.fetch_sub(1, Ordering::AcqRel);
                self.reply(
                    id,
                    &Reply::failed(
                        call.call_id,
                        Status::Busy,
                        &format!("{} calls are answered or waiting", self.queue),
                    ),
                );
                return;
            }
            let job = Job {
                call_id: call.call_id,
                chat,
                deadline: received + Duration::from_millis(call.deadline_ms),
                connection: id,
            };
            if self.jobs.lock().expect("jobs poisoned").send(job).is_err() {
                PENDING.fetch_sub(1, Ordering::AcqRel);
                self.reply(
                    id,
                    &Reply::failed(call.call_id, Status::Error, "the model stopped"),
                );
            }
        }

        /// What the worker answered, on the proactor: frees the call's place in the queue,
        /// then replies.
        fn reply_from_model(self: &Arc<Self>, id: u64, reply: &Reply) {
            PENDING.fetch_sub(1, Ordering::AcqRel);
            self.reply(id, reply);
        }

        /// Sends `reply` and closes the connection.
        fn reply(self: &Arc<Self>, id: u64, reply: &Reply) {
            let raw = {
                let connections = self.connections.lock().expect("connections poisoned");
                connections.get(&id).map(|c| c.fd.as_raw_fd())
            };
            self.finish(id, raw, reply.line());
        }

        fn finish(self: &Arc<Self>, id: u64, raw: Option<RawFd>, line: Vec<u8>) {
            let Some(raw) = raw else {
                return;
            };
            let server = Arc::clone(self);
            let sent =
                self.handle.send(
                    raw,
                    IoBuf::from_vec(line),
                    move |result: IoResult| match result {
                        Ok(transfer) => {
                            let count = transfer.bytes_transferred as usize;
                            let rest = transfer.buf.into_vec();
                            if count < rest.len() {
                                server.finish(id, Some(raw), rest[count..].to_vec());
                                return;
                            }
                            server.close(id);
                        }
                        Err(_) => server.close(id),
                    },
                );
            if sent.is_err() {
                self.close(id);
            }
        }

        fn close(&self, id: u64) {
            self.connections
                .lock()
                .expect("connections poisoned")
                .remove(&id);
        }
    }

    fn current_uid() -> libc::uid_t {
        // SAFETY: geteuid cannot fail and touches no memory.
        unsafe { libc::geteuid() }
    }

    /// The user at the other end of a Unix socket.
    fn peer_uid(fd: RawFd) -> std::io::Result<libc::uid_t> {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            let mut cred = libc::ucred {
                pid: 0,
                uid: 0,
                gid: 0,
            };
            let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
            // SAFETY: cred and len are valid for writes of the sizes given.
            let rc = unsafe {
                libc::getsockopt(
                    fd,
                    libc::SOL_SOCKET,
                    libc::SO_PEERCRED,
                    std::ptr::addr_of_mut!(cred).cast(),
                    &mut len,
                )
            };
            if rc != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(cred.uid)
        }
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        {
            let (mut uid, mut gid) = (0, 0);
            // SAFETY: uid and gid are valid for writes.
            if unsafe { libc::getpeereid(fd, &mut uid, &mut gid) } != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(uid)
        }
    }

    /// The write end of the pipe SIGTERM and SIGINT write to.
    static SIGNAL_PIPE: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

    extern "C" fn on_signal(_: libc::c_int) {
        let fd = SIGNAL_PIPE.load(Ordering::Relaxed);
        if fd >= 0 {
            // SAFETY: write is async-signal-safe; the byte is a valid one-byte buffer.
            unsafe {
                libc::write(fd, [1u8].as_ptr().cast(), 1);
            }
        }
    }

    /// The pipe's two ends, kept open while the server runs.
    struct SignalPipe {
        _read: OwnedFd,
        _write: OwnedFd,
    }

    /// SIGTERM and SIGINT write to a pipe the proactor waits on, which stops it.
    fn stop_on_signals(handle: &ProactorHandle<PlatformPort>) -> Result<SignalPipe, String> {
        let mut fds = [0; 2];
        // SAFETY: fds is valid for two descriptors.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return Err(format!(
                "cannot make the signal pipe: {}",
                std::io::Error::last_os_error()
            ));
        }
        // SAFETY: pipe made both descriptors just now; nothing else owns them.
        let (read, write) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        for fd in [read.as_raw_fd(), write.as_raw_fd()] {
            // SAFETY: fd is open; F_SETFL with O_NONBLOCK changes only its flags.
            unsafe {
                let flags = libc::fcntl(fd, libc::F_GETFL);
                libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
            }
        }
        SIGNAL_PIPE.store(write.as_raw_fd(), Ordering::Relaxed);
        let stopper = handle.clone();
        handle
            .register_readable(read.as_raw_fd(), SIGNAL_TOKEN, move |_| {
                let _ = stopper.stop();
            })
            .map_err(|e| format!("cannot wait on the signal pipe: {e}"))?;
        for signal in [libc::SIGTERM, libc::SIGINT] {
            // SAFETY: on_signal only calls write, which is async-signal-safe.
            unsafe {
                let mut action: libc::sigaction = std::mem::zeroed();
                action.sa_sigaction = on_signal as *const () as usize;
                libc::sigemptyset(&mut action.sa_mask);
                if libc::sigaction(signal, &action, std::ptr::null_mut()) != 0 {
                    return Err(format!(
                        "cannot handle signal {signal}: {}",
                        std::io::Error::last_os_error()
                    ));
                }
            }
        }
        Ok(SignalPipe {
            _read: read,
            _write: write,
        })
    }

    /// The signal pipe's readiness token: large and distinctive, as the proactor asks.
    const SIGNAL_TOKEN: u64 = 0x4750_544f_5353_4947;
}
