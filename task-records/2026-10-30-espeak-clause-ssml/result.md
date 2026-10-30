# Task 13 result: readclause.c and ssml.c hold only bridges

commit: 7512cf2ded6597f793130e3b50329db78c42a8bc

## What changed

- The clause reader's state and callbacks (rust/readclause_engine_compat.rs) and the SSML tag host (ssml_compat.rs `host`) are Rust (1,126 to 929 lines).

## Commits

- espeak-ng-rs `7512cf2d`

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
