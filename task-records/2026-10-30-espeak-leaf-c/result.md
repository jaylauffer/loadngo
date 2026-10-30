# Task 11 result: the leaf and tool files hold only bridges

commit: c2b9b18d1d9986afc4e64cb20523f880a02bf69c

## What changed

- common.c, error.c, spect.c, the MBROLA shims, rust_cli.c, langopts.c, compiledict.c and compiledata.c hold only bridges to Rust (c_inventory: 1,444 to 1,322 lines).
- CI found that the CLI and phoneme compiler called `sscanf`, which the UCRT defines only inline; `crt::scan` now calls the CRT parser itself (`__stdio_common_vsscanf` on Windows), and cli_io writes its messages from Rust.

## Commits

- espeak-ng-rs `0c425610`
- espeak-ng-rs `600752be`
- espeak-ng-rs `c2b9b18d`

## Evidence

All workflows on GitHub dev at `605ebbd4`, which contains this work:

- Android: run 40981185512, success
- C oracle: run 40981174998, success
- CI: run 40981175291, success
- Dist: run 40981188919, success
- Rust port: run 40981174932, success
- windows-msbuild: run 40981182189, success
- Windows: run 40981178645, success

Local gates before each push: cargo fmt, tests (all features and none), clippy -D warnings on the host, non-native Linux and Windows and 14 native targets; both CMake lanes (129 and 133 CTests).
