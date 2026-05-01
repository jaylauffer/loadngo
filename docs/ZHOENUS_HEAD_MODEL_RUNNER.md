# Zhoenus Head Model Runner

Purpose: keep the local Zhoenus talking-head model behind a loadngo-owned
service boundary instead of launching ad hoc `llama-cli` sessions.

The first target is `/Users/jay/Downloads/gpt-oss-20b-mxfp4.gguf` served by
Homebrew `llama-server` on localhost. The runner supervises startup, captures
logs, probes readiness, and kills the child process if the backend fails or the
startup deadline is exceeded.

## Why This Exists

The observed failure mode was a direct `llama-cli` run that either:

- failed in Metal with `failed to create command queue`
- or continued loading in CPU-only mode after the interactive command was
  interrupted

That is not a safe operating shape for a game-facing lab service. Zhoenus needs
a durable local endpoint, and loadngo is the right place to own the process
supervision and later task-plane advertisement.

## Runner

Dry-run the planned command without starting the model:

```bash
cargo run -p network --bin zhoenus_head_model -- --dry-run
```

Start with automatic backend selection:

```bash
cargo run -p network --bin zhoenus_head_model -- \
  --backend auto \
  --startup-timeout-seconds 180
```

Force CPU-only mode when Metal is unavailable:

```bash
cargo run -p network --bin zhoenus_head_model -- \
  --backend cpu \
  --startup-timeout-seconds 300
```

The default endpoint is:

```text
http://127.0.0.1:8787
```

The runner probes `/health` until `llama-server` is ready. If startup fails, it
prints the captured log tail and classifies known failures such as:

- `metal-backend-unavailable`
- `unsupported-model`
- `resource-exhausted`
- `startup-timeout`

## Zhoenus Contract

Zhoenus should treat this as an external local service. The Unreal side should
not own model process lifetime directly.

Initial integration path:

1. `loadngo` starts and supervises `zhoenus_head_model`.
2. Zhoenus talks to `http://127.0.0.1:8787` when player text is submitted to
   the talking-head assistant.
3. If the service is unavailable, Zhoenus keeps using native fallback hints.
4. Later, a loadngo worker node can advertise `zhoenus-head-model` capability
   and attach qcoin/EAB proof to successful model-service work.

## Operational Notes

CPU-only `gpt-oss-20b-mxfp4` startup can consume substantial RAM and time. Do
not run it as an unbounded foreground experiment. Use the runner timeout and
verify the process list after interrupted starts.

Metal failure should not be treated as a game failure. It is a backend
availability condition; `--backend auto` falls back to CPU only when the logs
classify the failure as Metal-specific. Generic startup timeouts fail closed so
the runner does not blindly move from a costly Metal load into a more costly
CPU load.
