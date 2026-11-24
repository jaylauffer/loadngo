//! Coarsely sample the platform thermal provider through proactor timer events.
//! With a command after `--`, observations continue until that process exits.

use anyhow::{anyhow, bail, Context, Result};
use loadngo_proactor::{new_platform_proactor, CompletionKind};
use loadngo_thermal::{platform_provider, ThermalGovernor, ThermalProvider};
use std::io::{self, Write};
use std::process::Command;
use std::sync::mpsc::{self, TryRecvError};
use std::time::{Duration, Instant};

const DEFAULT_INTERVAL: Duration = Duration::from_secs(10);

fn main() {
    if let Err(error) = run() {
        eprintln!("repository_thermal_benchmark: {error:#}; pass --help for usage");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || matches!(args[0].as_str(), "--help" | "-h" | "help") {
        print_help();
        return Ok(());
    }

    let (options, command) = match args.iter().position(|arg| arg == "--") {
        Some(separator) => (&args[..separator], &args[separator + 1..]),
        None => (&args[..], &[][..]),
    };
    let mut interval = DEFAULT_INTERVAL;
    let mut duration = None;
    let mut index = 0;
    while index < options.len() {
        let flag = options[index].as_str();
        let value = options
            .get(index + 1)
            .ok_or_else(|| anyhow!("missing value for {flag}"))?;
        match flag {
            "--interval-seconds" => {
                let seconds: u64 = value
                    .parse()
                    .with_context(|| format!("invalid interval {value:?}"))?;
                if seconds == 0 || seconds > 3600 {
                    bail!("--interval-seconds must be between 1 and 3600");
                }
                interval = Duration::from_secs(seconds);
            }
            "--duration-seconds" => {
                let seconds: u64 = value
                    .parse()
                    .with_context(|| format!("invalid duration {value:?}"))?;
                if seconds == 0 {
                    bail!("--duration-seconds must be positive");
                }
                duration = Some(Duration::from_secs(seconds));
            }
            _ => bail!("unknown option {flag:?}"),
        }
        index += 2;
    }
    if command.is_empty() && duration.is_none() {
        bail!("sampler-only mode requires --duration-seconds");
    }
    if !command.is_empty() && duration.is_some() {
        bail!("--duration-seconds is only valid without a command");
    }

    let mut child = if command.is_empty() {
        None
    } else {
        let child = Command::new(&command[0])
            .args(&command[1..])
            .spawn()
            .with_context(|| format!("failed to start benchmark command {:?}", command[0]))?;
        Some(child)
    };
    let proactor = new_platform_proactor().context("failed to create platform proactor")?;
    let handle = proactor.handle();
    let mut provider = platform_provider();
    let mut governor = ThermalGovernor::new(Default::default());
    let started = Instant::now();
    let mut samples = 0_u64;

    print_sample(&mut provider, &mut governor, started, &mut samples);
    loop {
        if let Some(process) = child.as_mut() {
            if let Some(status) = process.try_wait()? {
                if !status.success() {
                    bail!("benchmark command exited with {status}");
                }
                break;
            }
        } else if started.elapsed() >= duration.unwrap_or_default() {
            break;
        }

        let wait = if child.is_none() {
            interval.min(
                duration
                    .unwrap_or_default()
                    .saturating_sub(started.elapsed()),
            )
        } else {
            interval
        };
        if wait.is_zero() {
            break;
        }
        wait_for_timer(&proactor, &handle, wait)?;
        print_sample(&mut provider, &mut governor, started, &mut samples);
    }
    eprintln!(
        "thermal\tfinished_elapsed_ms\t{}",
        started.elapsed().as_millis()
    );
    eprintln!("thermal\tsample_count\t{samples}");
    if let Some(mut process) = child {
        let _ = process.wait();
    }
    Ok(())
}

fn print_sample(
    provider: &mut Box<dyn ThermalProvider>,
    governor: &mut ThermalGovernor,
    started: Instant,
    samples: &mut u64,
) {
    let observation = provider.observe();
    let snapshot = governor
        .observe(&observation, Instant::now())
        .unwrap_or_else(|| governor.snapshot().clone());
    *samples += 1;
    let _ = writeln!(
        io::stderr().lock(),
        "thermal\t{}\t{:?}\t{:?}\t{:?}\t{:?}\t{:?}\t{:?}",
        started.elapsed().as_millis(),
        observation.pressure,
        snapshot.pressure,
        snapshot.recommendation,
        observation.source,
        observation.temperature_c,
        observation.throttling
    );
}

fn wait_for_timer(
    proactor: &loadngo_proactor::Proactor<loadngo_proactor::PlatformPort>,
    handle: &loadngo_proactor::ProactorHandle<loadngo_proactor::PlatformPort>,
    duration: Duration,
) -> io::Result<()> {
    let (sender, receiver) = mpsc::sync_channel(1);
    handle.defer_for(duration, CompletionKind::Timer, 0, move |_| {
        let _ = sender.send(());
    })?;
    loop {
        match receiver.try_recv() {
            Ok(()) => return Ok(()),
            Err(TryRecvError::Disconnected) => {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "thermal timer completion channel closed",
                ));
            }
            Err(TryRecvError::Empty) => {
                proactor.run_once()?;
            }
        }
    }
}

fn print_help() {
    println!(
        "repository_thermal_benchmark — sample platform thermal state on proactor timer events\
         \n\
         \nFlags:\
         \n  --interval-seconds N [optional] sample cadence in seconds; default 10, range 1..=3600\
         \n  --duration-seconds N [optional] sampler-only duration; required when no command follows --\
         \n\
         \nWithout a command, the process samples for the requested duration. With a command,\
         \nit samples immediately and at each deferred timer event until the command exits.\
         \nSamples go to stderr as TSV: elapsed milliseconds, raw pressure, governed pressure,\
         \nrecommendation, source, temperature, throttling. The native macOS provider has no\
         \ntemperature or throttling fields. The timer is driven by loadngo-proactor.\
         \n\
         \nExamples:\
         \n  repository_thermal_benchmark --duration-seconds 60 --interval-seconds 10\
         \n  repository_thermal_benchmark --interval-seconds 10 -- python3 scripts/repository_chunk_workload.py --repo-root . --probe target/release/repository_chunk_probe --cas-root /Volumes/data/repo-cas"
    );
}
