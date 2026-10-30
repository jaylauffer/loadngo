# Task 12 result: numbers.c and dictionary.c hold only bridges

commit: f6a73ed49537ece7a048f88a392419dfb2ef916e

## What changed

- The number, letter, Roman numeral, phoneme, rule, match and list hosts' callbacks, LoadDictionary and RemoveEnding are Rust (c_inventory: 1,322 to 1,126 lines).

## Commits

- espeak-ng-rs `f6a73ed4`

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
