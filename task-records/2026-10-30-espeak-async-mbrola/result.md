# Task 17 result: the asynchronous and MBROLA build has no C logic

commit: 47520204806af8fbd692c8abc722faf51bbe80f6

## What changed

- The MBROLA wrapper (rust/mbrowrap_compat.rs: mbrola.dll on Windows, the mbrola child over pipes elsewhere), the queue's callbacks and the remaining queue and MBROLA primitives are Rust (330 to 0 lines in -DUSE_ASYNC=ON -DUSE_MBROLA=ON). No test runs a real mbrola program.

## Commits

- espeak-ng-rs `47520204`

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
