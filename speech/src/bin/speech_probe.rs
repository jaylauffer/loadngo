//! Exercises loadngo-speech on this machine: permission, file transcription, speaking
//! and live listening. Run with `--help`.

const HELP: &str = "\
speech_probe: check on-device speech recognition and synthesis on this machine

Usage: speech_probe <command> [--locale L]

  auth            ask for speech-recognition permission (macOS shows a dialog once)
  file PATH       transcribe an audio file on the device
  speak TEXT      say TEXT with the system voice and wait until it has finished
  listen [N]      print N utterances heard on the default microphone (default 3)
  --locale L      optional, recognition/voice locale (default en-US)
  -h, --help      this text

Example: speech_probe listen 2
";

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("speech_probe needs macOS");
    std::process::exit(1);
}

#[cfg(target_os = "macos")]
fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut locale = String::from("en-US");
    if let Some(i) = args.iter().position(|a| a == "--locale") {
        if i + 1 >= args.len() {
            fail("--locale needs a value");
        }
        locale = args.remove(i + 1);
        args.remove(i);
    }
    let result = match args.first().map(String::as_str) {
        None | Some("-h" | "--help") => {
            print!("{HELP}");
            return;
        }
        Some("auth") => loadngo_speech::request_authorization().map(|()| println!("authorized")),
        Some("file") => {
            let path = args.get(1).unwrap_or_else(|| fail("file needs a path"));
            loadngo_speech::transcribe_file(std::path::Path::new(path), &locale)
                .map(|text| println!("{text}"))
        }
        Some("speak") => {
            let text = args[1..].join(" ");
            let speaker = loadngo_speech::Speaker::new(&locale);
            let start = std::time::Instant::now();
            speaker
                .speak(&text)
                .map(|()| println!("spoken in {:.1} s", start.elapsed().as_secs_f64()))
        }
        Some("listen") => {
            let n: usize = args.get(1).map_or(3, |n| {
                n.parse()
                    .unwrap_or_else(|_| fail("listen N: N is a number"))
            });
            listen(&locale, n)
        }
        Some(other) => fail(&format!("unknown command {other}")),
    };
    if let Err(e) = result {
        eprintln!("speech_probe: {e}");
        std::process::exit(1);
    }
}

#[cfg(target_os = "macos")]
fn listen(locale: &str, n: usize) -> Result<(), loadngo_speech::Error> {
    loadngo_speech::request_authorization()?;
    let mut listener = loadngo_speech::Listener::start(locale, &["Kimi"])?;
    eprintln!("listening on the default microphone...");
    let mut heard = 0;
    while heard < n {
        let text = listener.next_utterance(std::time::Duration::from_millis(1200))?;
        if !text.trim().is_empty() {
            println!("heard: {text}");
            heard += 1;
        }
    }
    Ok(())
}

fn fail(message: &str) -> ! {
    eprintln!("speech_probe: {message} (see --help)");
    std::process::exit(2);
}
