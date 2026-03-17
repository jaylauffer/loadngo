# Macroquad Removal Status

Date: 2026-03-16

## Current state

### Completed
- `sng-rusty` no longer depends on `macroquad`, `macroquad_macro`, or `miniquad`.
- `loadngo-host-desktop` no longer depends on `macroquad`.
- The obsolete `host-mac` crate has been removed from the workspace.
- macOS now runs through a `loadngo`-owned AppKit + Metal host path.
- `ui-core`, `host-core`, `renderer`, `gui`, and `gui-win32` remain backend-agnostic.

### In progress
- Non-mac desktop targets now compile against a `loadngo` placeholder host path instead of a hidden Macroquad fallback.
- Android now exports a `loadngo`-owned `android_main` placeholder in `sng-rusty/src/lib.rs`; the remaining work is replacing that placeholder with a real mobile host.

## What has already been completed
- Input snapshots, touch model, and key polling live in `loadngo-host-core`.
- `loadngo-renderer` owns command encoding and frame resource planning.
- `loadngo-gfx-metal` owns the active macOS render path, including text and image presentation.
- `sng-rusty` runtime/editor host shims now delegate to `loadngo-host-desktop`.
- Shared font assets and the active font manifest now live under `loadngo/assets/fonts/`.

## Remaining work
1. Implement `loadngo`-owned native hosts for Windows, Linux, iOS, and Android.
2. Replace the temporary non-mac placeholder backend in `loadngo-host-desktop/src/fallback.rs` with real platform backends.
3. Replace the temporary Android `android_main` placeholder with a real `loadngo` mobile bootstrap.

## Acceptance checks
- `cargo tree -p sng-rusty | rg "macroquad|miniquad"` returns no matches on macOS.
- `cargo test -q -p loadngo-host-core --test macroquad_removal` passes.
- `cargo test -q dependency_tests` in `sng-rusty` passes, with the Android `quad_main` removal check now enabled.
- Manual validation on macOS confirms:
  - startup and frame presentation
  - menu interaction
  - font/text rendering
  - Dock icon and cursor ownership
