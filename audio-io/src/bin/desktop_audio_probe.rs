//! Bounded native desktop playback smoke test; silent unless --tone is set.
use std::time::Duration;

const HELP: &str = "desktop_audio_probe — exercise Loadngo's session-managed desktop output

Usage: desktop_audio_probe --seconds N [--target NAME] [--tone] [--panic]
       desktop_audio_probe --help

  --seconds N   Required: run for 1–60 seconds, then drop/close the stream.
  --target NAME Optional: PipeWire node.name/object.serial on Linux;
                platform device name elsewhere. Default: desktop policy.
  --tone        Optional: play a quiet 440 Hz test tone (2% amplitude).
                Without this flag the probe sends silence.
  --panic       Optional: deliberately panic once in the callback to test
                containment. Success means failure is reported, not a crash.
  --help, -h    Print this help and exit successfully.

Examples:
  cargo run -p loadngo-audio-io --bin desktop_audio_probe -- --seconds 5
  desktop_audio_probe --seconds 10 --target loadngo-test-sink --tone

No global volume changes, packages, configuration files, or persistent services.
Reports startup/stream errors and exits nonzero; no raw-hardware fallback.
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args.iter().any(|s| s == "--help" || s == "-h") {
        print!("{HELP}");
        return;
    }
    match parse(&args) {
        Ok(options) => {
            if let Err(error) = run(options) {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        Err(error) => {
            eprintln!("{error}; pass --help for usage");
            std::process::exit(2);
        }
    }
}

#[derive(Debug)]
struct Options {
    seconds: u64,
    target: Option<String>,
    tone: bool,
    panic: bool,
}

fn parse(args: &[String]) -> Result<Options, &'static str> {
    let mut seconds = None;
    let mut target = None;
    let mut tone = false;
    let mut panic = false;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--seconds" if seconds.is_none() => {
                seconds = Some(
                    args.next()
                        .ok_or("missing --seconds value")?
                        .parse::<u64>()
                        .map_err(|_| "invalid seconds")?,
                );
            }
            "--target" if target.is_none() => {
                target = Some(args.next().ok_or("missing --target value")?.clone())
            }
            "--tone" if !tone => tone = true,
            "--panic" if !panic => panic = true,
            _ => return Err("unknown or repeated argument"),
        }
    }
    let seconds = seconds
        .filter(|n| (1..=60).contains(n))
        .ok_or("--seconds must be 1–60")?;
    Ok(Options {
        seconds,
        target,
        tone,
        panic,
    })
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn run(options: Options) -> Result<(), String> {
    use loadngo_audio_io::{open_desktop_output_stream, DesktopOutputOptions};
    use std::sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    };
    let frames = Arc::new(AtomicU64::new(0));
    let callbacks = Arc::new(AtomicU64::new(0));
    let written = Arc::clone(&frames);
    let calls = Arc::clone(&callbacks);
    let mut phase = 0.0f32;
    let stream = open_desktop_output_stream(
        &DesktopOutputOptions {
            application_name: "Loadngo desktop audio probe".into(),
            target: options.target,
            ..Default::default()
        },
        move |samples, channels| {
            if options.panic {
                panic!("intentional desktop audio containment test");
            }
            for frame in samples.chunks_exact_mut(channels) {
                let value = if options.tone {
                    phase.sin() * 0.02
                } else {
                    0.0
                };
                frame.fill(value);
                phase = (phase + std::f32::consts::TAU * 440.0 / 48_000.0) % std::f32::consts::TAU;
            }
            written.fetch_add((samples.len() / channels) as u64, Ordering::Relaxed);
            calls.fetch_add(1, Ordering::Relaxed);
        },
    )
    .map_err(|e| e.to_string())?;
    println!("pid={} format={:?}", std::process::id(), stream.format());
    // One bounded wait, not a polling/render loop or a timer thread.
    std::thread::sleep(Duration::from_secs(options.seconds));
    let failure = stream.failure();
    drop(stream);
    println!(
        "closed: frames={} callbacks={} failure={failure:?}",
        frames.load(Ordering::Relaxed),
        callbacks.load(Ordering::Relaxed)
    );
    if options.panic {
        return if failure.as_deref().is_some_and(|s| s.contains("panicked")) {
            Ok(())
        } else {
            Err("callback panic was not reported".into())
        };
    }
    if let Some(error) = failure {
        return Err(error);
    }
    if callbacks.load(Ordering::Relaxed) == 0 {
        return Err("no audio callbacks observed".into());
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn run(_options: Options) -> Result<(), String> {
    Err("desktop audio is unavailable on this target".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }
    #[test]
    fn validates_duration_and_flags() {
        for s in [
            "--seconds 0",
            "--seconds 61",
            "--seconds",
            "--tone",
            "--seconds 2 --bogus",
            "--seconds 2 --seconds 3",
        ] {
            assert!(parse(&args(s)).is_err());
        }
        assert!(parse(&args("--seconds 2 --tone --target test")).is_ok());
    }
}
