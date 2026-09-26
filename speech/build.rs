//! macOS asks a program for its reasons before granting microphone or speech-recognition
//! access, and stops one that has none. A command-line binary carries them in an
//! Info.plist embedded in its `__TEXT,__info_plist` section.
fn main() {
    println!("cargo:rerun-if-changed=speech-probe-Info.plist");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        let plist = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap())
            .join("speech-probe-Info.plist");
        println!(
            "cargo:rustc-link-arg-bin=speech_probe=-Wl,-sectcreate,__TEXT,__info_plist,{}",
            plist.display()
        );
    }
}
