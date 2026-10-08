//! Profiles loadngo-proactor on the machine it runs on: the same scenarios
//! for every backend this platform has (io_uring and its epoll fallback on
//! Linux, kqueue on Apple, IOCP on Windows), one operation at a time, each
//! timed on its own so the table shows medians and tails, not just means.
//!
//! The `proactor-profile` workflow runs it on GitHub's Linux (x86-64 and
//! arm64), macOS and Windows runners and writes each table to the job
//! summary, so a change to a backend shows up as a change in these numbers
//! on every platform. Hosted runners are shared machines: compare runs, not
//! single numbers, and expect a few percent of noise.

use std::io::Write;
use std::path::PathBuf;

const USAGE: &str = "\
proactor-profile: time loadngo-proactor's operations on this machine

Runs each scenario on every backend this platform has, --rounds times, and
prints per-operation median, 99th percentile and fastest times. Operations
are submitted and waited for one at a time. Any wait that has not completed
within 2 s stops that scenario and is reported as a stall.

Scenarios:
  enqueue          enqueue_work, dispatched by run_once on the same thread
  cross-thread     enqueue_work from this thread to a proactor blocked in
                   run_once on another thread, and the reply back
  timer-1ms        defer_for(1 ms); reports how late it fires
  idle-100ms       run_once_until 100 ms ahead with nothing queued; reports
                   how late it returns, and fails as spinning if the waits
                   took more than 3 turns each on average
  file-read-4k     4 KiB reads from one open file
  file-read-64k    64 KiB reads from one open file
  file-churn       open, read 4 KiB, close, repeated
  udp-pair         send_to + recv_from of 4 bytes between two sockets
  udp-churn        bind, send_to once, close, repeated

Options (all optional):
  --rounds N      rounds per scenario (default 5)
  --ops N         operations per round (default 5000; timers and idle
                  waits use N/50)
  --markdown P    also append the results as a Markdown table to file P
                  (the workflow passes $GITHUB_STEP_SUMMARY)
  -h, --help      this text

Example:
  cargo run --release -p proactor-harness --bin proactor-profile -- --rounds 3
";

pub struct Options {
    rounds: usize,
    ops: usize,
    markdown: Option<PathBuf>,
}

fn parse_options() -> Result<Option<Options>, String> {
    let mut options = Options {
        rounds: 5,
        ops: 5_000,
        markdown: None,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut count = |name: &str| -> Result<usize, String> {
            args.next()
                .and_then(|value| value.parse().ok())
                .filter(|&value| value > 0)
                .ok_or_else(|| format!("{name} needs a positive whole number"))
        };
        match arg.as_str() {
            "-h" | "--help" => return Ok(None),
            "--rounds" => options.rounds = count("--rounds")?,
            "--ops" => options.ops = count("--ops")?,
            "--markdown" => {
                options.markdown = Some(args.next().ok_or("--markdown needs a file path")?.into())
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(Some(options))
}

fn main() {
    let options = match parse_options() {
        Ok(Some(options)) => options,
        Ok(None) => {
            print!("{USAGE}");
            return;
        }
        Err(message) => {
            eprintln!("proactor-profile: {message} (see --help)");
            std::process::exit(2);
        }
    };
    let rows = scenarios::run_all(&options);
    let table = render(&rows);
    print!("{table}");
    if let Some(path) = &options.markdown {
        let written = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut file| file.write_all(table.as_bytes()));
        if let Err(error) = written {
            eprintln!("proactor-profile: cannot write {}: {error}", path.display());
            std::process::exit(1);
        }
    }
}

/// One scenario on one backend: every operation's time in nanoseconds,
/// across all rounds.
pub struct Row {
    scenario: &'static str,
    backend: &'static str,
    samples: Vec<u64>,
    note: String,
}

fn render(rows: &[Row]) -> String {
    let mut out = format!(
        "\n### loadngo-proactor on {} {}\n\n\
         | scenario | backend | ops | median ns | p99 ns | fastest ns | note |\n\
         |---|---|---:|---:|---:|---:|---|\n",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    for row in rows {
        let mut sorted = row.samples.clone();
        sorted.sort_unstable();
        let at = |fraction: f64| -> String {
            if sorted.is_empty() {
                String::from("-")
            } else {
                let index = ((sorted.len() - 1) as f64 * fraction).round() as usize;
                sorted[index].to_string()
            }
        };
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} |\n",
            row.scenario,
            row.backend,
            sorted.len(),
            at(0.5),
            at(0.99),
            at(0.0),
            row.note
        ));
    }
    out
}

mod scenarios {
    use super::{Options, Row};
    use loadngo_proactor::{
        Completion, CompletionKind, CompletionPort, IoBuf, IoPort, IoResult, Proactor, RawFdCompat,
    };
    use std::net::UdpSocket;
    use std::path::Path;
    use std::sync::mpsc::{self, Receiver};
    use std::thread;
    use std::time::{Duration, Instant};

    const FILE_BYTES: usize = 4 << 20;
    /// Longer than any working operation takes, even on a loaded runner.
    const STALL: Duration = Duration::from_secs(2);

    /// Why a scenario stopped early.
    struct Stalled(String);

    type Measure<P> = fn(&Proactor<P>, &Path, usize) -> Result<Vec<u64>, Stalled>;

    pub fn run_all(options: &Options) -> Vec<Row> {
        let directory =
            std::env::temp_dir().join(format!("loadngo-proactor-profile-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("data");
        let contents: Vec<u8> = (0..FILE_BYTES).map(|i| (i * 7) as u8).collect();
        std::fs::write(&path, &contents).unwrap();

        let mut rows = Vec::new();
        #[cfg(target_os = "linux")]
        {
            backend(
                "io_uring",
                || loadngo_proactor::IoUringPort::new().map(Proactor::new),
                &path,
                options,
                &mut rows,
            );
            backend(
                "epoll",
                || loadngo_proactor::EpollPort::new().map(Proactor::new),
                &path,
                options,
                &mut rows,
            );
        }
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        backend(
            "kqueue",
            || loadngo_proactor::KqueuePort::new().map(Proactor::new),
            &path,
            options,
            &mut rows,
        );
        #[cfg(windows)]
        backend(
            "iocp",
            || loadngo_proactor::IocpPort::new().map(Proactor::new),
            &path,
            options,
            &mut rows,
        );

        let _ = std::fs::remove_dir_all(&directory);
        rows
    }

    fn backend<P: IoPort>(
        name: &'static str,
        make: impl Fn() -> std::io::Result<Proactor<P>>,
        path: &Path,
        options: &Options,
        rows: &mut Vec<Row>,
    ) {
        let scenarios: [(&'static str, Measure<P>, usize); 9] = [
            ("enqueue", enqueue, options.ops),
            ("cross-thread", cross_thread, options.ops),
            ("timer-1ms", timer, (options.ops / 50).max(1)),
            ("idle-100ms", idle, (options.ops / 500).max(1)),
            ("file-read-4k", file_read_4k, options.ops),
            ("file-read-64k", file_read_64k, options.ops),
            ("file-churn", file_churn, options.ops),
            ("udp-pair", udp_pair, options.ops),
            ("udp-churn", udp_churn, options.ops),
        ];
        for (scenario, measure, ops) in scenarios {
            let mut samples = Vec::new();
            let mut note = String::new();
            for round in 0..options.rounds {
                let proactor = match make() {
                    Ok(proactor) => proactor,
                    Err(error) => {
                        note = format!("backend unavailable: {error}");
                        break;
                    }
                };
                match measure(&proactor, path, ops) {
                    Ok(mut round_samples) => samples.append(&mut round_samples),
                    Err(Stalled(why)) => {
                        note = format!("stalled in round {round}: {why}");
                        break;
                    }
                }
            }
            eprintln!("{name} {scenario} done");
            rows.push(Row {
                scenario,
                backend: name,
                samples,
                note,
            });
        }
    }

    /// Drives `proactor` until `rx` yields, failing after [`STALL`].
    fn wait<P: CompletionPort, T>(
        proactor: &Proactor<P>,
        rx: &Receiver<T>,
        what: &str,
    ) -> Result<T, Stalled> {
        let deadline = Instant::now() + STALL;
        loop {
            if let Ok(value) = rx.try_recv() {
                return Ok(value);
            }
            if Instant::now() >= deadline {
                return Err(Stalled(what.to_string()));
            }
            proactor.run_once_until(deadline).unwrap();
        }
    }

    fn enqueue<P: IoPort>(
        proactor: &Proactor<P>,
        _: &Path,
        ops: usize,
    ) -> Result<Vec<u64>, Stalled> {
        let handle = proactor.handle();
        let (tx, rx) = mpsc::channel();
        let mut samples = Vec::with_capacity(ops);
        for _ in 0..ops {
            let tx = tx.clone();
            let started = Instant::now();
            handle
                .enqueue_work(move |_: Completion| tx.send(()).unwrap())
                .unwrap();
            wait(proactor, &rx, "enqueued work")?;
            samples.push(started.elapsed().as_nanos() as u64);
        }
        Ok(samples)
    }

    /// A pump thread blocked in `run_once`, woken by each post.
    fn cross_thread<P: IoPort>(
        proactor: &Proactor<P>,
        _: &Path,
        ops: usize,
    ) -> Result<Vec<u64>, Stalled> {
        let handle = proactor.handle();
        let (tx, rx) = mpsc::channel();
        let mut samples = Vec::with_capacity(ops);
        thread::scope(|scope| {
            scope.spawn(|| proactor.run_until_stopped().unwrap());
            let mut result = Ok(());
            for _ in 0..ops {
                let tx = tx.clone();
                let started = Instant::now();
                handle
                    .enqueue_work(move |_: Completion| tx.send(()).unwrap())
                    .unwrap();
                if rx.recv_timeout(STALL).is_err() {
                    result = Err(Stalled(String::from("post to a blocked pump")));
                    break;
                }
                samples.push(started.elapsed().as_nanos() as u64);
            }
            handle.stop().unwrap();
            result
        })?;
        Ok(samples)
    }

    /// Nanoseconds after its deadline that a 1 ms timer fires.
    fn timer<P: IoPort>(proactor: &Proactor<P>, _: &Path, ops: usize) -> Result<Vec<u64>, Stalled> {
        let handle = proactor.handle();
        let (tx, rx) = mpsc::channel();
        let mut samples = Vec::with_capacity(ops);
        for _ in 0..ops {
            let tx = tx.clone();
            let due = Instant::now() + Duration::from_millis(1);
            handle
                .defer_until(due, CompletionKind::Timer, 0, move |_: Completion| {
                    tx.send(Instant::now()).unwrap()
                })
                .unwrap();
            let fired = wait(proactor, &rx, "1 ms timer")?;
            samples.push(fired.saturating_duration_since(due).as_nanos() as u64);
        }
        Ok(samples)
    }

    /// Nanoseconds late returning from an idle 100 ms wait. A backend whose
    /// poll returns early takes many turns per wait: that is reported as
    /// spinning rather than timed.
    fn idle<P: IoPort>(proactor: &Proactor<P>, _: &Path, ops: usize) -> Result<Vec<u64>, Stalled> {
        let mut samples = Vec::with_capacity(ops);
        let mut turns = 0usize;
        for _ in 0..ops {
            let deadline = Instant::now() + Duration::from_millis(100);
            while Instant::now() < deadline {
                proactor.run_once_until(deadline).unwrap();
                turns += 1;
            }
            samples.push(Instant::now().duration_since(deadline).as_nanos() as u64);
        }
        if turns > ops * 3 {
            return Err(Stalled(format!(
                "{turns} turns for {ops} idle waits: the poll returns early (spinning)"
            )));
        }
        Ok(samples)
    }

    fn open(path: &Path) -> std::fs::File {
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.custom_flags(0x4000_0000); // FILE_FLAG_OVERLAPPED, for IOCP
        }
        options.open(path).unwrap()
    }

    fn raw_file(file: &std::fs::File) -> RawFdCompat {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            file.as_raw_fd()
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            file.as_raw_handle() as usize as RawFdCompat
        }
    }

    fn raw_socket(socket: &UdpSocket) -> RawFdCompat {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            socket.as_raw_fd()
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawSocket;
            socket.as_raw_socket()
        }
    }

    /// Reads `bytes` at a time through the file, one read in flight.
    fn file_reads<P: IoPort>(
        proactor: &Proactor<P>,
        path: &Path,
        ops: usize,
        bytes: usize,
    ) -> Result<Vec<u64>, Stalled> {
        let handle = proactor.handle();
        let file = open(path);
        let fd = raw_file(&file);
        let (tx, rx) = mpsc::channel::<IoBuf>();
        let mut buf = Some(IoBuf::with_capacity(bytes));
        let mut samples = Vec::with_capacity(ops);
        for op in 0..ops {
            let tx = tx.clone();
            let offset = ((op * bytes) % FILE_BYTES) as u64;
            let started = Instant::now();
            handle
                .read(fd, buf.take().unwrap(), offset, move |result: IoResult| {
                    tx.send(result.unwrap().buf).unwrap();
                })
                .unwrap();
            buf = Some(wait(proactor, &rx, "file read")?);
            samples.push(started.elapsed().as_nanos() as u64);
        }
        Ok(samples)
    }

    fn file_read_4k<P: IoPort>(
        proactor: &Proactor<P>,
        path: &Path,
        ops: usize,
    ) -> Result<Vec<u64>, Stalled> {
        file_reads(proactor, path, ops, 4096)
    }

    fn file_read_64k<P: IoPort>(
        proactor: &Proactor<P>,
        path: &Path,
        ops: usize,
    ) -> Result<Vec<u64>, Stalled> {
        file_reads(proactor, path, ops, 64 << 10)
    }

    /// Open, read 4 KiB, close: handle values are reused from cycle to cycle.
    fn file_churn<P: IoPort>(
        proactor: &Proactor<P>,
        path: &Path,
        ops: usize,
    ) -> Result<Vec<u64>, Stalled> {
        let handle = proactor.handle();
        let (tx, rx) = mpsc::channel::<IoBuf>();
        let mut buf = Some(IoBuf::with_capacity(4096));
        let mut samples = Vec::with_capacity(ops);
        for cycle in 0..ops {
            let started = Instant::now();
            let file = open(path);
            let tx = tx.clone();
            handle
                .read(
                    raw_file(&file),
                    buf.take().unwrap(),
                    0,
                    move |result: IoResult| {
                        tx.send(result.unwrap().buf).unwrap();
                    },
                )
                .unwrap();
            buf = Some(wait(
                proactor,
                &rx,
                &format!("read after reopen, cycle {cycle}"),
            )?);
            drop(file);
            samples.push(started.elapsed().as_nanos() as u64);
        }
        Ok(samples)
    }

    fn udp_pair<P: IoPort>(
        proactor: &Proactor<P>,
        _: &Path,
        ops: usize,
    ) -> Result<Vec<u64>, Stalled> {
        let handle = proactor.handle();
        let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
        let target = receiver.local_addr().unwrap();
        let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
        let (tx, rx) = mpsc::channel::<(bool, IoBuf)>();
        let mut receive_buf = Some(IoBuf::with_capacity(64));
        let mut send_buf = Some(IoBuf::from_vec(b"ping".to_vec()));
        let mut samples = Vec::with_capacity(ops);
        for _ in 0..ops {
            let started = Instant::now();
            let receive_tx = tx.clone();
            handle
                .recv_from(
                    raw_socket(&receiver),
                    receive_buf.take().unwrap(),
                    move |result: IoResult| receive_tx.send((true, result.unwrap().buf)).unwrap(),
                )
                .unwrap();
            let send_tx = tx.clone();
            handle
                .send_to(
                    raw_socket(&sender),
                    send_buf.take().unwrap(),
                    target,
                    move |result: IoResult| send_tx.send((false, result.unwrap().buf)).unwrap(),
                )
                .unwrap();
            for _ in 0..2 {
                match wait(proactor, &rx, "send_to/recv_from")? {
                    (true, returned) => receive_buf = Some(returned),
                    (false, returned) => send_buf = Some(returned),
                }
            }
            samples.push(started.elapsed().as_nanos() as u64);
        }
        Ok(samples)
    }

    fn udp_churn<P: IoPort>(
        proactor: &Proactor<P>,
        _: &Path,
        ops: usize,
    ) -> Result<Vec<u64>, Stalled> {
        let handle = proactor.handle();
        let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
        receiver.set_nonblocking(true).unwrap();
        let target = receiver.local_addr().unwrap();
        let (tx, rx) = mpsc::channel::<IoBuf>();
        let mut buf = Some(IoBuf::from_vec(b"ping".to_vec()));
        let mut drain = [0u8; 64];
        let mut samples = Vec::with_capacity(ops);
        for cycle in 0..ops {
            let started = Instant::now();
            let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
            let tx = tx.clone();
            handle
                .send_to(
                    raw_socket(&sender),
                    buf.take().unwrap(),
                    target,
                    move |result: IoResult| tx.send(result.unwrap().buf).unwrap(),
                )
                .unwrap();
            buf = Some(wait(
                proactor,
                &rx,
                &format!("send after rebind, cycle {cycle}"),
            )?);
            drop(sender);
            samples.push(started.elapsed().as_nanos() as u64);
            // Keep the receiver's buffer from filling and dropping datagrams.
            while receiver.recv(&mut drain).is_ok() {}
        }
        Ok(samples)
    }
}
