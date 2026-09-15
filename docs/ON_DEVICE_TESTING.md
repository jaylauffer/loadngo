# On-device testing

loadngo's cfg'd platform code (kqueue on iOS, `EpollPort` on Android, the
mobile hosts) can only be proven on a real phone. `cargo test` cannot run
there, and loadngo has no framework that can. Today every on-device run is
trail-blazing: hand-built commands, a borrowed app identity, and results
read by eye. This file records how it is done now, so the next run doesn't
start from scratch, and what a proper framework has to replace.

## How it is done today

### Android (adb)

Test binaries are plain ELF executables; `adb shell` runs them directly.

```bash
NDK=~/Library/Android/sdk/ndk/<version>/toolchains/llvm/prebuilt/darwin-x86_64/bin
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER=$NDK/aarch64-linux-android30-clang
cargo test -p loadngo-proactor --target aarch64-linux-android --no-run --message-format=json \
  | grep -o '"executable":"[^"]*"'            # the binaries to push
adb shell mkdir -p /data/local/tmp/<dir>
adb push target/aarch64-linux-android/debug/deps/<test-bin> /data/local/tmp/<dir>/
adb shell "cd /data/local/tmp/<dir> && ./<test-bin>"
adb shell rm -rf /data/local/tmp/<dir>       # clean up afterwards
```

Constraints found the hard way:

- The adb shell's SELinux domain may not create socket files under
  `/data/local/tmp` (`bind` fails with EACCES). Use the abstract namespace
  (`std::os::android::net::SocketAddrExt`) for AF_UNIX tests.
- There is no cargo config for the Android linker; set it per shell.

### iOS (signed wrapper app)

A test binary must be inside a signed `.app` whose provisioning profile
covers the device. `scripts/ios-device-test.sh` builds that wrapper,
installs it with `xcrun devicectl`, runs it with `--console`, and exits with
the test binary's exit code. Signing values are private and live outside
the repos (`~/pudding/sng-roguelite-ios-signing-notes.md`).

Constraints:

- The Apple team is a free team: profiles are per bundle id, expire about
  weekly, and there is no wildcard. The wrapper therefore **borrows** an
  existing app's bundle id and replaces that app on the phone. Jay has
  designated `sng-mahjong` as the test bed (2026-09-15); reinstall
  `sng-mahjong/build/ios/device/SNGMahjong.app` afterwards.
- `ios-deploy` can no longer launch on iOS 17+; use `devicectl`.
- The app's temp dir is ~90 bytes deep, so AF_UNIX socket paths under it
  must be short (`sun_path` is 104 bytes).
- Each test binary is a separate install and launch.

## What a proper framework needs

Wanted, not yet designed or scheduled:

- **One command per platform** that builds, deploys, runs every test
  binary of a crate and reports a pass/fail summary with a real exit code,
  so a phone can be a CI gate rather than a manual step.
- **A dedicated test identity**: an iOS bundle id and profile that belong
  to testing, so a run never replaces a game, plus automated profile
  expiry warnings.
- **Result capture that is machine-readable** (libtest JSON or JUnit),
  not console text scraped by eye.
- **Temp-path and permission portability** handled once in shared test
  helpers (sockets, temp files, sandbox paths), not rediscovered per test.
- **Device claiming** that fits `AGENT-BOARD.md` so two agents never drive
  the same phone at once.
- **Windows and Linux parity**: the same runner shape for the CI hosts.

## Runs on record

| Date | Commit | Device | Result |
|---|---|---|---|
| 2026-09-15 | `98d58d5a` | Android, Xiaomi 22111317I | lib 3/3, `core` 6/6, `epoll` 12/13 (AF_UNIX accept test: `bind` EACCES) |
| 2026-09-15 | `98d58d5a` | iPhone 13 Pro Max, iOS 26.6 | lib 3/3, `core` 6/6, `kqueue` 11/12 (AF_UNIX accept test: path longer than `sun_path`) |
| 2026-09-15 | with this doc's commit (both accept tests fixed) | Android, Xiaomi 22111317I | `epoll` 13/13 |
| 2026-09-15 | with this doc's commit (both accept tests fixed) | iPhone 13 Pro Max, via `scripts/ios-device-test.sh` | `kqueue` 12/12, script exit 0 |
