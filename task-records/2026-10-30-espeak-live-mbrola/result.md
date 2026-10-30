# Result: espeak-ng-rs, a live MBROLA test in CI, the Rust engine against the C oracle

commit: cb9b86671041d5dfd3338f6d33e4c2c6750e4f42

## What changed (espeak-ng-rs dev, cb9b8667)

- `tests/common`: `is_mbrola` exits 1 instead of 0 when mbrola is missing and
  `ESPEAK_REQUIRE_MBROLA=1`; without the variable it still skips.
- `tests/mbrola.test`: a missing voice file fails the test under the same
  variable instead of printing "was not tested".
- `.github/workflows/rust.yml`: the Linux shared lane (Rust engine,
  USE_MBROLA=ON) installs Debian's `mbrola` and `mbrola-fr4` and runs
  `ctest -R '^mbrola$' --no-tests=error -V` with `ESPEAK_REQUIRE_MBROLA=1`.
- `.github/workflows/c-oracle.yml`: the Linux lane (C engine) does the same.

## Evidence

- Rust port run 41001751707, job "speech-parity (ubuntu-latest, ON)", step
  "Live MBROLA (the Rust engine)": "checking if MBROLA is installed ... yes",
  "testing mb-fr4 Bonjour", "testing mb-fr4+announcer Bonjour", Passed.
- C oracle run 41001751679, job "c-engine (ubuntu-latest)", step "Live
  MBROLA (the C engine)": the same three lines, Passed.
- Both engines matched upstream's expected SHA-1 values in the same
  `tests/mbrola.test`, so the Rust wrapper's mbrola child process
  (`rust/mbrowrap_compat.rs`) gives the C engine's WAV bytes.
- On this Mac (no mbrola), `ESPEAK_REQUIRE_MBROLA=1 sh tests/mbrola.test`
  exits 1 ("no, and ESPEAK_REQUIRE_MBROLA=1"); without the variable, 0.

CI, Windows, windows-msbuild, Android and Dist run on master pushes or by
hand; they ran (dispatched) on the Task 18 commit 5cc8ce38, which contains
this one, and all passed: CI 41002808976 (57/57), Windows 41002811873,
windows-msbuild 41002814815, Android 41002817446, Dist 41002820516. Rust
port 41002781460 and C oracle 41002781402 at 5cc8ce38 ran the live MBROLA
steps again and passed.

Not covered: the Windows path that loads mbrola.dll has no live test.
