# loadngo
A lifetime of work, love, and imagination.

Rust workspace for the `loadngo` GUI/runtime stack plus data/network/task crates.

## Workspace crates
- `ui-core`: platform-agnostic UI model layer (widgets, geometry, input, paint ops).
- `host-core` (`loadngo-host-core`): host/backend contracts (window descriptors, frame/input snapshots, render ops, image decode/registry, texture/font seams).
- `renderer` (`loadngo-renderer`): renderer-owned frame command encoding, multilingual text contracts, and backend interfaces.
- `gfx-metal` (`loadngo-gfx-metal`): macOS-first renderer backend landing zone for Metal execution.
- `gfx-gles` (`loadngo-gfx-gles`): cross-platform OpenGL ES renderer backend.
- `gfx-dx12` (`loadngo-gfx-dx12`): Windows DirectX 12 renderer backend.
- `host-desktop` (`loadngo-host-desktop`): desktop backend implementation of host/render/input primitives.
- `gui`: platform-agnostic GUI composition over `ui-core`.
- `gui-win32`: Win32 host shim for `gui`.
- `host-mac`: current macOS host executable for exercising `ui-core` + `host-core`.
- `data`: data and CAS-oriented storage primitives.
- `pq-auth` (`loadngo-pq-auth`): post-quantum signed challenge-token authenticator for operators and nodes.
- `network`: networking primitives and protocol tests.
- `task`: app surface for Task planning features.
- `proactor` (`loadngo-proactor`): async I/O and event processing primitives.
- `touch` (`loadngo-touch`): touch input handling components.

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
cargo test -q -p loadngo-pq-auth
cargo test -q -p proactor
cargo test -q -p touch
```

Run macOS host sample (on macOS):

```bash
cargo run -p host-mac
```

## Architecture docs
- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md): layering and ownership boundaries.
- [`docs/RENDERER_ROADMAP.md`](docs/RENDERER_ROADMAP.md): macOS-first renderer ownership plan and multilingual requirements.
- [`docs/PQ_AUTHENTICATOR.md`](docs/PQ_AUTHENTICATOR.md): repo-owned post-quantum authenticator model for signed challenge tokens.
- [`docs/PUDDING_CAS_PQ_MODEL.md`](docs/PUDDING_CAS_PQ_MODEL.md): native pudding CAS repository model design.
- [`docs/FIELD_NETWORK_BACKLOG.md`](docs/FIELD_NETWORK_BACKLOG.md): network and replication considerations.
- [`docs/TASK_OFFER_PROTOCOL.md`](docs/TASK_OFFER_PROTOCOL.md): submitter/worker task coordination protocol over multicast discovery and direct unicast follow-up.
- [`docs/TASK_EXECUTION_TEST_PLAN.md`](docs/TASK_EXECUTION_TEST_PLAN.md): lab validation plan for correlation, worker selection, status cadence, and reward closure.
- [`docs/TASK_REWARD_FLOW.md`](docs/TASK_REWARD_FLOW.md): explicit worker-facing explanation of how accepted task work becomes qcoin-backed reward proof.
- [`docs/WORKER_FIRST_TASK_MODEL.md`](docs/WORKER_FIRST_TASK_MODEL.md): worker posture and reward-gating model inside the submitter-driven task protocol.

## Codex skill
- [`skills/loadngo-task/SKILL.md`](skills/loadngo-task/SKILL.md): repo-owned Codex skill for meaningful `loadngo` task work and qcoin-backed reward closure.
