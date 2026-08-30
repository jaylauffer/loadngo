#[cfg(target_os = "netbsd")]
use std::{env, time::Duration};

#[cfg(target_os = "netbsd")]
fn main() {
    let mut device = String::from("/dev/ttyE0");
    let mut seconds = 4u64;
    let mut probe_only = false;

    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--device" => {
                if let Some(value) = args.next() {
                    device = value;
                }
            }
            "--seconds" => {
                if let Some(value) = args.next() {
                    seconds = value.parse().unwrap_or(seconds);
                }
            }
            "--probe-only" => {
                probe_only = true;
            }
            "--help" | "-h" => {
                print_help();
                return;
            }
            _ => {
                eprintln!("unknown argument: {arg}");
                print_help();
                std::process::exit(2);
            }
        }
    }

    let result = if probe_only {
        loadngo_host_desktop::netbsd_wsdisplay::probe(&device)
    } else {
        loadngo_host_desktop::netbsd_wsdisplay::paint_test_pattern(
            &device,
            Duration::from_secs(seconds),
        )
    };

    match result {
        Ok(info) => {
            println!(
                "wsdisplay {device}: {}x{} stride={} bpp={} fb_size={} fb_offset={} rgb=({}:{} {}:{} {}:{} a{}:{})",
                info.width,
                info.height,
                info.stride,
                info.bits_per_pixel,
                info.fb_size,
                info.fb_offset,
                info.red_offset,
                info.red_size,
                info.green_offset,
                info.green_size,
                info.blue_offset,
                info.blue_size,
                info.alpha_offset,
                info.alpha_size
            );
        }
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(1);
        }
    }
}

#[cfg(not(target_os = "netbsd"))]
fn main() {
    eprintln!("netbsd_wsdisplay_probe only runs on NetBSD.");
    std::process::exit(1);
}

#[cfg(target_os = "netbsd")]
fn print_help() {
    println!("Usage: netbsd_wsdisplay_probe [--device /dev/ttyE0] [--probe-only] [--seconds 4]");
}
