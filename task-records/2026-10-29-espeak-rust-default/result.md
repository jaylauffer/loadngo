# Task 10 result: espeak-ng-rs builds the Rust core by default

commit: 72a8c53b9f0b13363d330965da7b195fe210e797

## What changed

- `USE_RUST_CORE` defaults ON (CMake); the C engine stays buildable with
  `-DUSE_RUST_CORE=OFF` and is built and tested by the new "C oracle" workflow.
- The Rust target follows the C compiler (Android ABI, MSVC architecture, iOS,
  Apple architecture, `-dumpmachine`, 32-bit hosts); MSVC uses `/MD` and C11;
  `ESPEAK_RUST_SANITIZER` builds the core instrumented on nightly.
- The Visual Studio solution builds `espeak_ng_rs.lib` through cargo and the
  command-line program from `rust_cli.c`.
- The Android app, CI's 57 lanes, Windows, windows-msbuild and Dist install the
  Rust target their lane builds for.
- 32-bit targets: loadngo's proactor builds there (`2f7ecbaa`), layout checks
  carry 32-bit sizes (ARM EABI's 8-byte doubles included).
- The arm lanes run qemu with a guest address space below 3 GB, which AddressSanitizer
  on arm32 needs (`605ebbd4`).
- Fixes the matrix found (commits listed below): io_uring refused under qemu
  (loadngo `6962d8a0`, epoll fallback), MSVC C11 and config switches, Android
  source paths and liblog, a voice-copy leak on queue stop, the 32-bit rule
  trace, reprotest's faketime and Rust's timed waits, and test oracles kept
  defined under sanitizers and 32-bit targets.

## Commits

espeak-ng-rs `dev`, in order: `4bfbf60d`, `b1a32762`, `b527f645`, `95105b7b`,
`41386eed`, `074a93da`, `21131bce`, `9360fa33`, `86c7beff` (round 2); `5d1f95a9`,
`00572b0e`, `e5aace22`, `63960797`, `c9423100`, `c620c493`, `51dfd01b`, `a137813a`
(round 3); `19ad1689`, `7cf91fc8`, `c78535d0`, `4ee468b1` (round 4); `a397d924`
(Windows DLL beside the reference CLI); `c2b9b18d` (TSan CLI parity timeout, Windows
sscanf, Linux lint); `605ebbd4` (arm lanes in a guest space below 3 GB); `72a8c53b` (rust.cmake's help names
the C oracle option in words: the first result, naming `605ebbd4`, was rejected because
the check's pattern for turning USE_RUST_CORE off matched that help text). loadngo
`6962d8a0` (epoll fallback where io_uring is refused).

## Evidence

All workflows on GitHub dev at `72a8c53b`:

- Android: run 40986012640, success
- C oracle: run 40986003686, success
- CI: run 40986003637, success
- Dist: run 40986015445, success
- Rust port: run 40986003705, success
- windows-msbuild: run 40986009939, success
- Windows: run 40986006896, success

Local gates before each push: cargo fmt, tests (all features and none), clippy -D warnings on the host, non-native Linux and Windows and 14 native targets; both CMake lanes (129 and 133 CTests).
