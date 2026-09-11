//! Manual, real-hardware smoke tests. `#[ignore]`d by default -- these open
//! actual OS audio devices and would be flaky/meaningless on a headless CI
//! runner with no audio hardware. Run explicitly on a real desktop machine:
//!
//! ```bash
//! cargo test -p loadngo-audio-io --test hardware_smoke -- --ignored --nocapture
//! ```

use std::time::Duration;

use loadngo_audio_io::{list_input_devices, list_output_devices, LiveMonitor, LiveMonitorConfig};

#[test]
#[ignore]
fn lists_at_least_one_device_on_each_side() {
    let inputs = list_input_devices().expect("input device enumeration failed");
    let outputs = list_output_devices().expect("output device enumeration failed");
    println!("input devices: {inputs:?}");
    println!("output devices: {outputs:?}");
    assert!(!inputs.is_empty(), "expected at least one input device");
    assert!(!outputs.is_empty(), "expected at least one output device");
}

#[test]
#[ignore]
fn opens_and_cleanly_stops_a_default_device_monitor() {
    let monitor =
        LiveMonitor::start(LiveMonitorConfig::default()).expect("failed to start LiveMonitor");
    println!(
        "monitoring input {:?} -> output {:?} at {} Hz",
        monitor.input_device_name(),
        monitor.output_device_name(),
        monitor.input_sample_rate_hz()
    );
    monitor.set_gain(0.0); // muted-by-gain, not muted flag, exercises both paths elsewhere
    std::thread::sleep(Duration::from_millis(500));
    let mut tap = Vec::new();
    monitor.drain_tap(&mut tap);
    println!("captured {} tapped samples in 500ms", tap.len());
    // Dropping here exercises the `Proactor`-backed stop/join path.
}

#[test]
#[ignore]
fn probes_the_default_input_devices_capabilities() {
    let inputs = list_input_devices().expect("input device enumeration failed");
    for device in inputs {
        match loadngo_audio_io::probe_input_capabilities(Some(&device.name)) {
            Ok(capabilities) => {
                println!("{}:", capabilities.device_name);
                println!("  current stream:   {:?}", capabilities.current_stream);
                println!("  current physical: {:?}", capabilities.current_physical);
                println!(
                    "  capture as:       {:?}",
                    capabilities.capture_resolution()
                );
                println!("  stream formats:   {:?}", capabilities.stream_formats);
                println!("  physical formats: {:?}", capabilities.physical_formats);
            }
            Err(error) => println!("{}: probe failed: {error}", device.name),
        }
    }
}

#[test]
#[ignore]
fn records_full_width_frames_through_the_recording_tap() {
    let mut monitor =
        LiveMonitor::start(LiveMonitorConfig::default()).expect("failed to start LiveMonitor");
    monitor.set_gain(0.0);
    println!("recording from {:?}", monitor.input_device_name());
    let mut tap = monitor.take_recording_tap().expect("recording tap missing");
    assert!(monitor.take_recording_tap().is_none());

    tap.arm();
    std::thread::sleep(Duration::from_millis(500));
    assert!(tap.disarm_and_settle(Duration::from_millis(500)));
    let mut frames = Vec::new();
    tap.pop_into(&mut frames, usize::MAX);
    let channels = usize::from(tap.channels());
    println!(
        "captured {} samples ({} frames x {} ch at {} Hz), {} dropped",
        frames.len(),
        frames.len() / channels,
        channels,
        tap.sample_rate_hz(),
        tap.dropped_samples()
    );
    assert_eq!(frames.len() % channels, 0);
    assert_eq!(tap.channels(), monitor.input_channels());
    // ~500ms at the device rate, allowing for callback-block granularity.
    let expected = tap.sample_rate_hz() as usize * channels / 2;
    assert!(frames.len() > expected / 2, "far too few samples captured");
    assert_eq!(tap.dropped_samples(), 0);

    monitor.return_recording_tap(tap);
    assert!(monitor.take_recording_tap().is_some());
}

/// Changes a real converter's system-wide physical format and restores it.
/// Set `LOADNGO_PHYSICAL_FORMAT_DEVICE` to the device name to run it.
#[cfg(target_os = "macos")]
#[test]
#[ignore]
fn switches_a_converter_physical_format_and_restores_it() {
    let Ok(name) = std::env::var("LOADNGO_PHYSICAL_FORMAT_DEVICE") else {
        println!("LOADNGO_PHYSICAL_FORMAT_DEVICE not set; skipping");
        return;
    };
    let before = loadngo_audio_io::probe_input_capabilities(Some(&name)).expect("probe failed");
    let original = before
        .current_physical
        .expect("device reports no physical format");
    let Some(other) = before
        .physical_formats
        .iter()
        .find(|range| range.resolution != original.resolution)
    else {
        println!("{name} offers only one physical resolution; nothing to switch to");
        return;
    };
    let target = loadngo_audio_io::CurrentFormat {
        sample_rate_hz: original.sample_rate_hz,
        channels: original.channels,
        resolution: other.resolution,
    };
    println!(
        "switching {name}: {} -> {}",
        original.resolution, target.resolution
    );
    loadngo_audio_io::set_input_physical_format(&name, &target).expect("switch failed");
    std::thread::sleep(Duration::from_millis(300));
    let switched = loadngo_audio_io::probe_input_capabilities(Some(&name)).expect("probe failed");
    println!("now: {:?}", switched.current_physical);

    loadngo_audio_io::set_input_physical_format(&name, &original).expect("restore failed");
    std::thread::sleep(Duration::from_millis(300));
    let restored = loadngo_audio_io::probe_input_capabilities(Some(&name)).expect("probe failed");
    println!("restored: {:?}", restored.current_physical);

    assert_eq!(
        switched.current_physical.map(|format| format.resolution),
        Some(target.resolution)
    );
    assert_eq!(restored.current_physical, Some(original));
}

/// Opens a monitor on devices named by `LOADNGO_TEST_INPUT` /
/// `LOADNGO_TEST_OUTPUT`, silent (gain 0), and reports what it got.
#[test]
#[ignore]
fn opens_a_named_input_and_output_pair() {
    let input = std::env::var("LOADNGO_TEST_INPUT").ok();
    let output = std::env::var("LOADNGO_TEST_OUTPUT").ok();
    println!("requested input {input:?} -> output {output:?}");
    let preferred_buffer_frames = std::env::var("LOADNGO_TEST_BUFFER_FRAMES")
        .ok()
        .and_then(|value| value.parse().ok());
    let result = LiveMonitor::start(LiveMonitorConfig {
        input_device_name: input,
        output_device_name: output,
        initial_gain: 0.0,
        preferred_buffer_frames,
        ..LiveMonitorConfig::default()
    });
    match result {
        Ok(monitor) => {
            let seconds: u64 = std::env::var("LOADNGO_TEST_SECONDS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(2);
            println!(
                "opened {:?} ({} Hz x{}) -> {:?} ({} Hz)",
                monitor.input_device_name(),
                monitor.input_sample_rate_hz(),
                monitor.input_channels(),
                monitor.output_device_name(),
                monitor.output_sample_rate_hz(),
            );
            let mut tap = Vec::new();
            for second in 1..=seconds {
                std::thread::sleep(Duration::from_secs(1));
                tap.clear();
                monitor.drain_tap(&mut tap);
                let stats = monitor.drift_stats();
                println!(
                    "t={second:>4}s buffered={:>5} ({:.1} ms, target {}) correction={:+8.1} ppm underruns={} overflows={} overloads={} tap={} failure={:?}",
                    stats.buffered_samples,
                    monitor.buffered_ms(),
                    stats.target_samples,
                    stats.correction_ppm,
                    stats.underruns,
                    stats.overflows,
                    monitor.overloads(),
                    tap.len(),
                    monitor.failure()
                );
            }
        }
        Err(error) => println!("failed: {error}"),
    }
}

/// Prints the IO buffer range and current size CoreAudio reports for each
/// device, and what a request for `LOADNGO_TEST_BUFFER_FRAMES` resulted in.
#[cfg(target_os = "macos")]
#[test]
#[ignore]
fn reports_device_buffer_sizes() {
    let frames: u32 = std::env::var("LOADNGO_TEST_BUFFER_FRAMES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(128);
    let input = std::env::var("LOADNGO_TEST_INPUT").ok();
    let output = std::env::var("LOADNGO_TEST_OUTPUT").ok();
    let monitor = LiveMonitor::start(LiveMonitorConfig {
        input_device_name: input,
        output_device_name: output,
        initial_gain: 0.0,
        preferred_buffer_frames: Some(frames),
        ..LiveMonitorConfig::default()
    })
    .expect("monitor failed");
    std::thread::sleep(Duration::from_millis(500));
    println!(
        "requested {frames}: target {} samples ({:.1} ms)",
        monitor.drift_stats().target_samples,
        monitor.buffered_ms()
    );
}
