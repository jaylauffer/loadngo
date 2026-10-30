# Task 14 result: speech.c and voices.c hold only bridges

commit: 012c4a2b2b11e9f1d708bbbd58a543a600eecbe6

## What changed

- The data-path search, the lifecycle and driver tables' primitives, the API's state calls, and the voice catalog, selection and native/control hosts are Rust (929 to 775 lines).

## Commits

- espeak-ng-rs `012c4a2b`

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
