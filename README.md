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
- `audio-io` (`loadngo-audio-io`): pitch detection plus (desktop) live
  audio-input device enumeration and low-latency input-to-output
  monitoring, e.g. for an instrument tuner or a live monitoring/amplifier
  feature.

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
cargo test -q -p loadngo-audio-io
```

Run macOS host sample (on macOS):

```bash
cargo run -p host-mac
```

### Linux must always build

Large parts of `host-desktop` and `gfx-gles` sit behind
`#[cfg(target_os = ...)]`, so **a clean `cargo check` on macOS proves
nothing about Linux or Android** — it silently skips every cfg'd-out
block, exhaustive `match` arms included. Adding a `FrameCommand` variant
once broke five such matches while macOS stayed green the whole time.

Before pushing a change to a shared enum, trait, or widely-matched type:

```bash
# --all-features matters: gfx-gles/src/linux_egl.rs compiles only under it
cargo check --workspace --all-features
cargo check -p loadngo-gfx-gles --target aarch64-linux-android
```

`dolores` is the real Linux box and the self-hosted CI runner, so a green
build there is a green CI. CI itself runs `cargo fmt --check` and
`cargo clippy --workspace --all-targets --all-features -- -D warnings`,
both stricter than `cargo check` — **with `PLATFORM_EXCLUDES`**
(`--exclude loadngo-gfx-metal --exclude loadngo-gfx-dx12 --exclude
gui-win32 --exclude proactor-harness`). Copy that from
`.github/workflows/ci.yml`; without it the run fails on crates CI never
builds, which reads as a real failure and isn't one.

Note that **`cargo clippy --workspace --all-targets` does not pass on
macOS at all**, with or without `--all-features` — `gfx-metal` and
`gfx-gles` test code references cfg'd-out items, ~68 errors at a clean
checkout. So there is no local whole-workspace gate on macOS: lint the
crates you touched (`-p …`), and treat dolores as the only real gate.

Compiling is necessary but not sufficient. Clipping shipped compiling
everywhere and still regressed Android twice, because the bug was in
per-backend interpretation of a shared field. When a change adds something
every backend must interpret, put the interpretation in one shared tested
function rather than in per-backend arms — see
[`docs/CLIP_AND_SCISSOR.md`](docs/CLIP_AND_SCISSOR.md).

## Architecture docs
- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md): layering and ownership boundaries.
- [`docs/GAMEPAD_INPUT.md`](docs/GAMEPAD_INPUT.md): platform-agnostic gamepad/controller input design and platform rollout plan.
- [`docs/INPUT_PHILOSOPHY.md`](docs/INPUT_PHILOSOPHY.md): the values framing (energy as currency of exchange, healthy engagement) behind input-surface design decisions.
- [`docs/PROACTOR_ENGINE_ADOPTION.md`](docs/PROACTOR_ENGINE_ADOPTION.md): active proactor-first host-runtime adoption and evidence plan.
- [`docs/RENDERER_ROADMAP.md`](docs/RENDERER_ROADMAP.md): macOS-first renderer ownership plan and multilingual requirements.
- [`docs/AUDIO.md`](docs/AUDIO.md): music/voice separation and the reusable overlapping sound-effect controller contract.
- [`docs/AUDIO_IO.md`](docs/AUDIO_IO.md): live audio-input tooling -- pitch detection and desktop input-to-output monitoring, for tuners and live-amplifier-style features.
- [`docs/PQ_AUTHENTICATOR.md`](docs/PQ_AUTHENTICATOR.md): repo-owned post-quantum authenticator model for signed challenge tokens.
- [`docs/PUDDING_CAS_PQ_MODEL.md`](docs/PUDDING_CAS_PQ_MODEL.md): native pudding CAS repository model design.
- [`docs/FIELD_NETWORK_BACKLOG.md`](docs/FIELD_NETWORK_BACKLOG.md): network and replication considerations.
- [`docs/TASK_OFFER_PROTOCOL.md`](docs/TASK_OFFER_PROTOCOL.md): submitter/worker task coordination protocol over multicast discovery and direct unicast follow-up.
- [`docs/TASK_EXECUTION_TEST_PLAN.md`](docs/TASK_EXECUTION_TEST_PLAN.md): lab validation plan for correlation, worker selection, status cadence, and reward closure.
- [`docs/TASK_REWARD_FLOW.md`](docs/TASK_REWARD_FLOW.md): explicit worker-facing explanation of how accepted task work becomes qcoin-backed reward proof.
- [`docs/WORKER_FIRST_TASK_MODEL.md`](docs/WORKER_FIRST_TASK_MODEL.md): worker posture and reward-gating model inside the submitter-driven task protocol.
- [`docs/ZHOENUS_HEAD_MODEL_RUNNER.md`](docs/ZHOENUS_HEAD_MODEL_RUNNER.md): local `llama-server` supervision path for the Zhoenus talking-head assistant.

## Codex skill
- [`skills/loadngo-task/SKILL.md`](skills/loadngo-task/SKILL.md): repo-owned Codex skill for meaningful `loadngo` task work and qcoin-backed reward closure.
- [`skills/loadngo-worker/SKILL.md`](skills/loadngo-worker/SKILL.md): repo-owned Codex skill for active worker/listener posture, including capability, time, and energy-based offer discipline.

## Task runtimes
- `cargo run -p network --bin task-node -- ...`: standing worker node on top of `loadngo-proactor`.
- `cargo run -p network --bin task_worker -- ...`: bounded/manual worker helper for a single listening window or a narrow local session.
- `cargo run -p network --bin task_submitter -- ...`: submitter-side request, selection, verification, and qcoin reward closure flow.
- `cargo run -p network --bin zhoenus_head_model -- --dry-run`: inspect the supervised local model service command for the Zhoenus talking head.
