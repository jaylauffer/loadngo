# Macroquad Removal Status

Date: 2026-03-15

## Current state

### In `loadngo`
- `macroquad` is currently used by loadngo backend implementations:
  - `host-desktop` (shared desktop backend crate)
  - [`host-mac/Cargo.toml`](../host-mac/Cargo.toml)
  - [`host-mac/src/main.rs`](../host-mac/src/main.rs)
- `ui-core`, `host-core`, `gui`, and `gui-win32` do not depend on `macroquad`.
- Conclusion: `loadngo` core abstractions are backend-agnostic; Macroquad is isolated to backend crates.

### In `sng-rusty` (consumer)
- Runtime/editor host shims now delegate to `loadngo-host-desktop`.
- `sng-rusty/src` no longer imports `macroquad` directly.
- Direct `macroquad` dependency has been removed from `sng-rusty/Cargo.toml`.

## What has already been completed
- Input snapshots, touch model, and key polling are in `loadngo-host-core`.
- Pointer helper primitives are now centralized in `loadngo-host-core` (`pointer_in_rect`, `pointer_pressed_in_rect`, `pointer_released`).
- Image decode/registry and texture upload seams are defined in `loadngo-host-core`.
- `sng-rusty` has moved script/image file loading through host seams.

## Remaining work to fully remove Macroquad
1. Replace Macroquad inside backend crates (`loadngo-host-desktop`, `host-mac`) with non-Macroquad platform backends.
2. Introduce backend selection for desktop targets (macOS/Linux/Windows) without changing app/runtime code.
3. Remove Android `quad_main` dependency path in `sng-rusty` with equivalent host bootstrap.

## Recommended acceptance checks
- `rg -n "macroquad" sng-rusty loadngo --glob '!**/target/**'` returns no runtime/editor/backend usage except intentionally transitional crates.
- `cargo check` and test suites pass for:
  - `loadngo-host-core`
  - `ui-core`
  - `sng-rusty` runtime/editor binaries
- Manual validation confirms:
  - Button hover/click behavior
  - Menu/submenu navigation
  - Mouse + touch interaction parity
  - Text/font and image rendering parity
