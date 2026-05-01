use std::{env, time::Duration};

#[cfg(target_os = "netbsd")]
fn main() {
    let mut options = loadngo_host_desktop::netbsd_wsdesktop::WsDesktopOptions::default();

    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--device" => {
                if let Some(value) = args.next() {
                    options.display_path = value;
                }
            }
            "--mouse" => {
                if let Some(value) = args.next() {
                    options.mouse_path = Some(value);
                }
            }
            "--keyboard" => {
                if let Some(value) = args.next() {
                    options.keyboard_path = Some(value);
                }
            }
            "--no-input" => {
                options.mouse_path = None;
                options.keyboard_path = None;
            }
            "--continuous" => {
                options.continuous = true;
            }
            "--fps" => {
                if let Some(value) = args.next() {
                    options.fps = value.parse().unwrap_or(options.fps);
                }
            }
            "--cursor-hz" => {
                if let Some(value) = args.next() {
                    options.cursor_hz = value.parse().unwrap_or(options.cursor_hz);
                }
            }
            "--seconds" => {
                if let Some(value) = args.next() {
                    let seconds = value.parse().unwrap_or(0);
                    options.max_runtime = (seconds > 0).then(|| Duration::from_secs(seconds));
                }
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

    if let Err(err) = loadngo_host_desktop::netbsd_wsdesktop::run_desktop(options) {
        eprintln!("{err}");
        std::process::exit(1);
    }
}

#[cfg(not(target_os = "netbsd"))]
fn main() {
    eprintln!("netbsd_wsdesktop only runs on NetBSD.");
    std::process::exit(1);
}

fn print_help() {
    println!(
        "Usage: netbsd_wsdesktop [--device /dev/ttyE0] [--mouse /dev/wsmouse] [--keyboard /dev/wskbd] [--fps 2] [--cursor-hz 60] [--seconds 60] [--no-input] [--continuous]"
    );
}
