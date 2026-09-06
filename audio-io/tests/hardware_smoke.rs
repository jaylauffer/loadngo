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
