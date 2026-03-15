# loadngo
A lifetime of work, love, and imagination.

Rust workspace for the `loadngo` GUI/runtime stack plus data/network/task crates.

## Workspace crates
- `ui-core`: platform-agnostic UI model layer (widgets, geometry, input, paint ops).
- `host-core` (`loadngo-host-core`): host/backend contracts (window descriptors, frame/input snapshots, render ops, image decode/registry, texture/font seams).
- `renderer` (`loadngo-renderer`): renderer-owned frame command encoding, multilingual text contracts, and backend interfaces.
- `gfx-metal` (`loadngo-gfx-metal`): macOS-first renderer backend landing zone for Metal execution.
- `host-desktop` (`loadngo-host-desktop`): desktop backend implementation of host/render/input primitives.
- `gui`: platform-agnostic GUI composition over `ui-core`.
- `gui-win32`: Win32 host shim for `gui`.
- `host-mac`: current macOS host executable for exercising `ui-core` + `host-core`.
- `data`: data and CAS-oriented storage primitives.
- `network`: networking primitives and protocol tests.
- `task`: app surface for Task planning features.

## Legacy source tree
Top-level folders such as `Essay/`, `Outline/`, `Text/`, `Think/`, and `Xml/` contain historical C++/Visual Studio sources and assets. They are kept for reference during migration.

## Shared assets
- Shared renderer font assets belong under `loadngo/assets/fonts/`.

## Build and test
Run from `/Users/jay/pudding/loadngo`.

```bash
cargo build
cargo test -q -p ui-core
cargo test -q -p loadngo-host-core
cargo test -q -p data
cargo test -q -p network
```

Run macOS host sample (on macOS):

```bash
cargo run -p host-mac
```

## Architecture docs
- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md): layering and ownership boundaries.
- [`docs/RENDERER_ROADMAP.md`](docs/RENDERER_ROADMAP.md): macOS-first renderer ownership plan and multilingual requirements.
- [`docs/MACROQUAD_REMOVAL_STATUS.md`](docs/MACROQUAD_REMOVAL_STATUS.md): current dependency status and removal plan.
