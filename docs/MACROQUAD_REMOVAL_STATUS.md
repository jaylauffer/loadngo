# Macroquad Removal Status

Date: 2026-03-18

## Current state

### Completed
- `sng-rusty` no longer depends on `macroquad`, `macroquad_macro`, or `miniquad`.
- `loadngo-host-desktop` no longer depends on `macroquad`.
- The obsolete `host-mac` crate has been removed from the workspace.
- macOS now runs through a `loadngo`-owned AppKit + Metal host path.
- Android now runs through a `loadngo`-owned `NativeActivity` path:
  - `sng-rusty` exports `ANativeActivity_onCreate`
  - `loadngo-host-desktop` owns the Android host implementation
- `ui-core`, `host-core`, `renderer`, `gui`, and `gui-win32` remain backend-agnostic.

### In progress
- Non-mac, non-android desktop targets still compile through the temporary fallback backend in `loadngo-host-desktop/src/fallback.rs`.
- Android packaging is now repo-owned, but the default stable link path still needs a `build-std` workaround on this branch.

## What has already been completed
- Input snapshots, touch model, and key polling live in `loadngo-host-core`.
- `loadngo-renderer` owns command encoding and frame resource planning.
- `loadngo-gfx-metal` owns the active macOS render path, including text and image presentation.
- `loadngo-gfx-gles` owns the Android GLES render path.
- `sng-rusty` runtime/editor host shims now delegate to `loadngo-host-desktop`.
- Shared font assets and the active font manifest now live under `loadngo/assets/fonts/`.

## Remaining work
1. Implement `loadngo`-owned native hosts for Windows, Linux, and iOS.
2. Replace the temporary non-mac fallback backend in `loadngo-host-desktop/src/fallback.rs` with real Windows/Linux backends.
3. Remove the Android `build-std` linkage workaround and complete device/runtime acceptance.

## Acceptance checks
- `cargo tree -p sng-rusty | rg "macroquad|miniquad"` returns no matches on macOS.
- `cargo test -q -p loadngo-host-core --test macroquad_removal` passes.
- `cargo test -q dependency_tests` in `sng-rusty` passes, including the Android `quad_main` removal check.
- Manual validation on macOS confirms:
  - startup and frame presentation
  - menu interaction
  - font/text rendering
  - Dock icon and cursor ownership
- Android validation confirms:
  - `scripts/android_device_build.sh` succeeds with `ANDROID_BUILD_STD=1`
  - app install and launch work on a connected device
