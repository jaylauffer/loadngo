//! Profiles how `IocpPort` associates handles with its completion port.
//!
//! An overlapped operation on a handle that is not associated with the port
//! never completes there. Until 2026-10-08 the port associated each handle on
//! its first operation and remembered its raw value; Windows gives a closed
//! handle's value to the next handle opened, so the new handle was never
//! associated and its first read hung (espeak-ng-rs's Windows CI, six hours a
//! job; docs/PROACTOR_IOPORT_DEFECTS.md, defect 4). The port now has two
//! paths, measured here against each other on real Windows, one operation
//! at a time, so that the cost of associating is a visible share of each:
//!
//! - `Unregistered`: a raw handle, associated before every operation
//!   (`CreateIoCompletionPort`, failing with `ERROR_INVALID_PARAMETER` once
//!   it already is).
//! - `Registered`: `register` associated it once and returned a tagged value
//!   (registration number << 32 | handle) that operations look up.
//!
//! Run by the `proactor-profile` workflow's `iocp-association` job on
//! GitHub's `windows-latest`, which writes the table to the job summary.
//! Windows only; `--help` works anywhere.

use std::io::Write;
use std::path::PathBuf;

const USAGE: &str = "\
iocp-association-profile: time IocpPort's unregistered and registered handles

Runs each scenario --rounds times, the two paths interleaved within a round,
and prints the median and fastest time per operation for each. Every
operation is submitted and waited for before the next, so per-operation cost
shows directly. An operation that has not completed within 2 s is reported
as a stall (the handle was not associated) and that run stops.

Scenarios:
  association-call  the association step alone: a locked set lookup, and
                    CreateIoCompletionPort on an already-associated handle
  file-steady       4 KiB reads from one open file
  file-churn        open, read 4 KiB, close, repeated (handle values reused)
  udp-steady        send_to + recv_from of 4 bytes between two sockets
  udp-churn         bind, send_to once, close, repeated (socket values reused)

Options (all optional):
  --rounds N      rounds per scenario (default 7)
  --ops N         operations per steady round (default 20000)
  --cycles N      cycles per churn round (default 2000)
  --calls N       calls per association-call round (default 200000)
  --markdown P    also append the results as a Markdown table to file P
                  (the workflow passes $GITHUB_STEP_SUMMARY)
  -h, --help      this text

Example:
  cargo run --release -p proactor-harness --bin iocp-association-profile -- --rounds 3
";

struct Options {
    rounds: usize,
    ops: usize,
    cycles: usize,
    calls: usize,
    markdown: Option<PathBuf>,
}

fn parse_options() -> Result<Option<Options>, String> {
    let mut options = Options {
        rounds: 7,
        ops: 20_000,
        cycles: 2_000,
        calls: 200_000,
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
            "--cycles" => options.cycles = count("--cycles")?,
            "--calls" => options.calls = count("--calls")?,
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
            eprintln!("iocp-association-profile: {message} (see --help)");
            std::process::exit(2);
        }
    };
    let rows = profile::run(&options);
    let table = render(&rows);
    print!("{table}");
    if let Some(path) = &options.markdown {
        let written = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut file| file.write_all(table.as_bytes()));
        if let Err(error) = written {
            eprintln!(
                "iocp-association-profile: cannot write {}: {error}",
                path.display()
            );
            std::process::exit(1);
        }
    }
}

/// One scenario's result for one strategy.
struct Row {
    scenario: &'static str,
    strategy: &'static str,
    /// Nanoseconds per operation, one per completed round.
    samples: Vec<f64>,
    note: String,
}

fn render(rows: &[Row]) -> String {
    let mut out = format!(
        "\n### IOCP handle association\n\n{}; each figure is one round's \
         total time over its operation count\n\n\
         | scenario | strategy | median ns/op | fastest ns/op | median vs Unregistered | note |\n\
         |---|---|---:|---:|---:|---|\n",
        proactor_harness::clock_resolution()
    );
    for row in rows {
        let (median, fastest) = summary(&row.samples);
        let baseline = rows
            .iter()
            .find(|other| other.scenario == row.scenario && other.strategy == "Unregistered")
            .map(|other| summary(&other.samples).0)
            .filter(|baseline| baseline.is_finite() && row.strategy != "Unregistered");
        let delta = match baseline {
            Some(baseline) if median.is_finite() => format!("{:+.0}", median - baseline),
            _ => String::from("-"),
        };
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} |\n",
            row.scenario,
            row.strategy,
            number(median),
            number(fastest),
            delta,
            row.note
        ));
    }
    out
}

fn number(value: f64) -> String {
    if value.is_finite() {
        format!("{value:.0}")
    } else {
        String::from("-")
    }
}

/// Median and minimum; NaN for no samples.
fn summary(samples: &[f64]) -> (f64, f64) {
    if samples.is_empty() {
        return (f64::NAN, f64::NAN);
    }
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    (sorted[sorted.len() / 2], sorted[0])
}

#[cfg(not(windows))]
mod profile {
    pub fn run(_options: &super::Options) -> Vec<super::Row> {
        eprintln!("iocp-association-profile: IOCP exists only on Windows (see --help)");
        std::process::exit(2);
    }
}

#[cfg(windows)]
mod profile {
    use super::{Options, Row};
    use loadngo_proactor::{IoBuf, IoResult, IocpPort, Proactor, ProactorHandle};
    use std::collections::HashSet;
    use std::hint::black_box;
    use std::net::UdpSocket;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::{AsRawHandle, AsRawSocket};
    use std::path::{Path, PathBuf};
    use std::sync::mpsc::{self, Receiver};
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    use windows::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows::Win32::System::IO::CreateIoCompletionPort;

    const FILE_FLAG_OVERLAPPED: u32 = 0x4000_0000;
    const FILE_BYTES: usize = 1 << 20;
    const READ_BYTES: usize = 4096;
    /// Longer than any working operation takes, even on a loaded runner.
    const STALL: Duration = Duration::from_secs(2);
    /// Whether handles are registered, and the row name.
    const STRATEGIES: [(bool, &str); 2] = [(false, "Unregistered"), (true, "Registered")];

    enum Outcome {
        /// Nanoseconds per operation.
        Done(f64),
        Stalled {
            cycle: usize,
            reused: usize,
        },
    }

    struct Rig {
        proactor: Proactor<IocpPort>,
        handle: ProactorHandle<IocpPort>,
        register: bool,
    }

    impl Rig {
        fn new(register: bool) -> Self {
            let proactor = Proactor::new(IocpPort::new().unwrap());
            let handle = proactor.handle();
            Self {
                proactor,
                handle,
                register,
            }
        }

        fn operand(&self, raw: u64) -> u64 {
            if self.register {
                self.handle.register(raw).unwrap()
            } else {
                raw
            }
        }

        fn release(&self, fd: u64) {
            if self.register {
                self.handle.release(fd);
            }
        }

        /// Drives the proactor until `rx` yields, or `None` after [`STALL`].
        fn wait<T>(&self, rx: &Receiver<T>) -> Option<T> {
            let deadline = Instant::now() + STALL;
            loop {
                if let Ok(value) = rx.try_recv() {
                    return Some(value);
                }
                if Instant::now() >= deadline {
                    return None;
                }
                self.proactor.run_once_until(deadline).unwrap();
            }
        }
    }

    fn open(path: &Path) -> std::fs::File {
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_OVERLAPPED)
            .open(path)
            .unwrap()
    }

    fn file_steady(register: bool, path: &Path, ops: usize) -> Outcome {
        let rig = Rig::new(register);
        let file = open(path);
        let fd = rig.operand(file.as_raw_handle() as usize as u64);
        let (tx, rx) = mpsc::channel::<IoBuf>();
        let mut buf = Some(IoBuf::with_capacity(READ_BYTES));
        let started = Instant::now();
        for op in 0..ops {
            let tx = tx.clone();
            let offset = ((op * READ_BYTES) % FILE_BYTES) as u64;
            rig.handle
                .read(fd, buf.take().unwrap(), offset, move |result: IoResult| {
                    tx.send(result.unwrap().buf).unwrap();
                })
                .unwrap();
            match rig.wait(&rx) {
                Some(returned) => buf = Some(returned),
                None => {
                    return Outcome::Stalled {
                        cycle: op,
                        reused: 0,
                    }
                }
            }
        }
        let per_op = started.elapsed().as_nanos() as f64 / ops as f64;
        rig.release(fd);
        Outcome::Done(per_op)
    }

    fn file_churn(register: bool, path: &Path, cycles: usize) -> Outcome {
        let rig = Rig::new(register);
        let (tx, rx) = mpsc::channel::<IoBuf>();
        let mut buf = Some(IoBuf::with_capacity(READ_BYTES));
        let mut seen = HashSet::new();
        let mut reused = 0;
        let started = Instant::now();
        for cycle in 0..cycles {
            let file = open(path);
            let raw = file.as_raw_handle() as usize as u64;
            if !seen.insert(raw) {
                reused += 1;
            }
            let fd = rig.operand(raw);
            let tx = tx.clone();
            rig.handle
                .read(fd, buf.take().unwrap(), 0, move |result: IoResult| {
                    tx.send(result.unwrap().buf).unwrap();
                })
                .unwrap();
            match rig.wait(&rx) {
                Some(returned) => buf = Some(returned),
                // The read stays pending on the unassociated handle; its
                // buffer belongs to the leaked operation, so closing the
                // file below cannot free memory the kernel still writes.
                None => return Outcome::Stalled { cycle, reused },
            }
            rig.release(fd);
            drop(file);
        }
        Outcome::Done(started.elapsed().as_nanos() as f64 / cycles as f64)
    }

    fn udp_steady(register: bool, ops: usize) -> Outcome {
        let rig = Rig::new(register);
        let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
        let target = receiver.local_addr().unwrap();
        let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
        let receive_fd = rig.operand(receiver.as_raw_socket());
        let send_fd = rig.operand(sender.as_raw_socket());
        let (tx, rx) = mpsc::channel::<(bool, IoBuf)>();
        let mut receive_buf = Some(IoBuf::with_capacity(64));
        let mut send_buf = Some(IoBuf::from_vec(b"ping".to_vec()));
        let started = Instant::now();
        for op in 0..ops {
            let receive_tx = tx.clone();
            rig.handle
                .recv_from(
                    receive_fd,
                    receive_buf.take().unwrap(),
                    move |result: IoResult| {
                        receive_tx.send((true, result.unwrap().buf)).unwrap();
                    },
                )
                .unwrap();
            let send_tx = tx.clone();
            rig.handle
                .send_to(
                    send_fd,
                    send_buf.take().unwrap(),
                    target,
                    move |result: IoResult| {
                        send_tx.send((false, result.unwrap().buf)).unwrap();
                    },
                )
                .unwrap();
            for _ in 0..2 {
                match rig.wait(&rx) {
                    Some((true, returned)) => receive_buf = Some(returned),
                    Some((false, returned)) => send_buf = Some(returned),
                    None => {
                        return Outcome::Stalled {
                            cycle: op,
                            reused: 0,
                        }
                    }
                }
            }
        }
        let per_op = started.elapsed().as_nanos() as f64 / ops as f64;
        rig.release(receive_fd);
        rig.release(send_fd);
        Outcome::Done(per_op)
    }

    fn udp_churn(register: bool, cycles: usize) -> Outcome {
        let rig = Rig::new(register);
        let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
        let target = receiver.local_addr().unwrap();
        let (tx, rx) = mpsc::channel::<IoBuf>();
        let mut buf = Some(IoBuf::from_vec(b"ping".to_vec()));
        let mut seen = HashSet::new();
        let mut reused = 0;
        let mut drain = [0u8; 64];
        receiver.set_nonblocking(true).unwrap();
        let started = Instant::now();
        for cycle in 0..cycles {
            let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
            let raw = sender.as_raw_socket();
            if !seen.insert(raw) {
                reused += 1;
            }
            let fd = rig.operand(raw);
            let tx = tx.clone();
            rig.handle
                .send_to(fd, buf.take().unwrap(), target, move |result: IoResult| {
                    tx.send(result.unwrap().buf).unwrap();
                })
                .unwrap();
            match rig.wait(&rx) {
                Some(returned) => buf = Some(returned),
                None => return Outcome::Stalled { cycle, reused },
            }
            // Keep the receiver's buffer from filling and dropping datagrams.
            while receiver.recv(&mut drain).is_ok() {}
            rig.release(fd);
            drop(sender);
        }
        Outcome::Done(started.elapsed().as_nanos() as f64 / cycles as f64)
    }

    /// The association step alone, on a handle that is already associated.
    fn association_calls(path: &Path, calls: usize) -> (f64, f64, String) {
        let file = open(path);
        let raw = file.as_raw_handle() as usize as u64;

        let set = Mutex::new(HashSet::from([raw, raw | 7 << 32]));
        let started = Instant::now();
        for _ in 0..calls {
            black_box(set.lock().unwrap().contains(black_box(&raw)));
        }
        let lookup = started.elapsed().as_nanos() as f64 / calls as f64;

        let port = unsafe {
            CreateIoCompletionPort(INVALID_HANDLE_VALUE, HANDLE::default(), 0, 0).unwrap()
        };
        let handle = HANDLE(raw as usize as *mut _);
        unsafe { CreateIoCompletionPort(handle, port, 3, 0) }.unwrap();
        let mut codes = std::collections::BTreeMap::<String, usize>::new();
        let started = Instant::now();
        for _ in 0..calls {
            let result = unsafe { CreateIoCompletionPort(black_box(handle), port, 3, 0) };
            let code = match result {
                Ok(_) => String::from("ok"),
                Err(error) => format!("{}", error.code().0 & 0xffff),
            };
            *codes.entry(code).or_default() += 1;
        }
        let syscall = started.elapsed().as_nanos() as f64 / calls as f64;
        unsafe {
            let _ = CloseHandle(port);
        }
        let codes = codes
            .iter()
            .map(|(code, count)| format!("{code} x{count}"))
            .collect::<Vec<_>>()
            .join(", ");
        (lookup, syscall, codes)
    }

    pub fn run(options: &Options) -> Vec<Row> {
        let directory =
            std::env::temp_dir().join(format!("loadngo-iocp-association-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path: PathBuf = directory.join("data");
        let contents: Vec<u8> = (0..FILE_BYTES).map(|i| (i * 7) as u8).collect();
        std::fs::write(&path, &contents).unwrap();

        let mut rows = Vec::new();

        let mut lookup = Vec::new();
        let mut syscall = Vec::new();
        let mut codes = String::new();
        for _ in 0..options.rounds {
            let (l, s, c) = association_calls(&path, options.calls);
            lookup.push(l);
            syscall.push(s);
            codes = c;
        }
        rows.push(Row {
            scenario: "association-call",
            strategy: "Unregistered",
            samples: syscall,
            note: format!("CreateIoCompletionPort results: {codes}"),
        });
        rows.push(Row {
            scenario: "association-call",
            strategy: "Registered",
            samples: lookup,
            note: String::from("locked set lookup of the tagged value"),
        });

        type Scenario<'a> = (&'static str, Box<dyn Fn(bool) -> Outcome + 'a>);
        let scenarios: [Scenario; 4] = [
            (
                "file-steady",
                Box::new(|a| file_steady(a, &path, options.ops)),
            ),
            (
                "file-churn",
                Box::new(|a| file_churn(a, &path, options.cycles)),
            ),
            ("udp-steady", Box::new(|a| udp_steady(a, options.ops))),
            ("udp-churn", Box::new(|a| udp_churn(a, options.cycles))),
        ];
        for (scenario, measure) in &scenarios {
            let mut samples: Vec<Vec<f64>> = vec![Vec::new(); STRATEGIES.len()];
            let mut notes: Vec<String> = vec![String::new(); STRATEGIES.len()];
            for round in 0..options.rounds {
                for (index, (register, name)) in STRATEGIES.iter().enumerate() {
                    if !notes[index].is_empty() {
                        continue; // stalled once; it would stall again
                    }
                    match measure(*register) {
                        Outcome::Done(per_op) => samples[index].push(per_op),
                        Outcome::Stalled { cycle, reused } => {
                            notes[index] = format!(
                                "stalled at cycle {cycle} of round {round} \
                                 ({reused} handle values reused by then)"
                            );
                        }
                    }
                    eprintln!("{scenario} {name} round {round} done");
                }
            }
            for (index, (_, name)) in STRATEGIES.iter().enumerate() {
                rows.push(Row {
                    scenario,
                    strategy: name,
                    samples: std::mem::take(&mut samples[index]),
                    note: std::mem::take(&mut notes[index]),
                });
            }
        }

        let _ = std::fs::remove_dir_all(&directory);
        rows
    }
}
