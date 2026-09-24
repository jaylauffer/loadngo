# Metal compute backend for local models: plan

Status: plan, 2026-09-24 (Claude Code, at Jay's request). Nothing here is built yet.
Numbers marked *estimate* are arithmetic from measured sizes, not measurements.

## What "the Metal backend" means

Metal is Apple's programming interface for the GPU. On this Mac mini the GPU sits on the
same unified memory as the CPU and the Neural Engine and can read it at close to the M4
Pro's 273 GB/s. A Metal compute backend means running a model's arithmetic as GPU
*kernels* (small programs written in the Metal Shading Language, MSL), with the weights
kept in GPU-visible memory, instead of as CPU loops or Core ML predictions.

It is what ollama/llama.cpp does on this machine, and it is why their models feel fast.

## Why the current path is slow

Generating one token is a pass over every active weight once: it is limited by how fast
weights can be read, not by arithmetic. Measured on Kimi Linear 48B-A3B with
`--accel ane` (kimi-k3-in-rust `docs/KIMI_LINEAR.md`), ~0.75 s per token:

| Cost per token | Measured | Cause |
|---|---|---|
| ~980 synchronous Core ML predictions | ~0.37 s | the ANE streaming weights at 15-25 GB/s; measured later, call overhead is not the cost (`NPU_ACCELERATION.md`) |
| bf16 -> fp16 conversion of 6.4 GB | ~0.23 s | the ANE takes fp16; now mostly hidden by converting on a helper thread during the previous prediction (1.31 -> 1.68 tok/s) |
| the rest | ~0.15 s | single-threaded scalar CPU attention/routing, expert reads from disk |

Two structural limits sit under that: the ANE streamed weights at 24-29 GB/s in every
measurement (`NPU_ACCELERATION.md`), about a tenth of the memory bandwidth; and bf16
routed experts (94 GB) do not fit in 64 GB, so they stream from disk through a cache.

## Budget: what the GPU could do

Per decoded Kimi Linear token the active weights are about 3.2 GB of trunk (attention,
shared expert, router, LM head) plus 8 experts x 26 layers x 14.2 MB = 2.95 GB of routed
experts at bf16.

| Configuration | Bytes read per token | Resident? | Ceiling at ~200 GB/s *(estimate)* |
|---|---|---|---|
| bf16 everything (today's weights) | ~6.1 GB | no: experts 94 GB | disk-bound on cache misses |
| bf16 trunk + MXFP4 experts | ~4.0 GB | yes: ~4 GB + ~25 GB | ~50 tokens/s |
| practical, at llama.cpp-like 60-70% efficiency | | | ~25-35 tokens/s |

So the GPU alone is not enough: the experts must also fit in memory, which means storing
them at 4 bits. MXFP4 (OCP MX v1.0) is the natural choice: K3's experts are already
MXFP4, loadngo and kimi already have exact MXFP4 decoding, and the format is an open
spec. Quantizing changes the model's numbers, so it needs a measured quality check and
Jay's decision (M2 below).

K3 would gain much less: it is bound by reading 135 GB per token from the drive, which
no compute backend changes (`kimi-k3-in-rust/docs/APPLE_NEURAL_ENGINE.md`).

## Design

- **New crate `loadngo-metal-compute`** (macOS, later iOS), fresh BSD-3 code, with the
  same audited-unsafe boundary as `loadngo-coreml`. Typed `objc2-metal` bindings (cached
  locally, 0.3.2); kernels in MSL compiled at load time, as `gfx-metal` already does for
  its shaders. `gfx-metal` stays the renderer; the two share only a device.
- **Weights uploaded once** into `MTLStorageModeShared` buffers (unified memory: no
  second copy on a discrete GPU, and the CPU reference can read the same bytes). Loading
  copies once from the safetensors reads; no per-token conversion ever.
- **Whole-token command buffers, not per-matrix calls.** The lesson of the ANE path's
  980 calls: the device must run the entire layer graph (norms, projections, KDA
  recurrence, MLA attention, routing, experts, residuals) from one command buffer per
  token, with the CPU only tokenizing and sampling. So the interface is a model-level
  backend, not another `DenseAccel`.
- **Kernels** (each gated against the CPU reference): bf16 GEMV and small-batch GEMM,
  MXFP4 GEMV (dequantize in registers), RMSNorm, SiLU-gate, embedding gather, top-k
  sigmoid router with selection bias, expert gather/scatter, KDA short conv and
  recurrence (per-head d x d state), MLA attention over the KV cache, softmax.
- **Proactor, no polling.** Commit the token's command buffer, register a completion
  handler that posts to the loadngo proactor, and return; no `waitUntilCompleted` spin on
  a UI path, no timer thread, bounded in-flight work (`PROACTOR_ENGINE_ADOPTION.md`).
- **Kimi side** (Apache, in kimi-k3-in-rust): a `LinearBackend` trait over the model's
  forward, with the existing CPU path as the reference implementation and fallback.
- **The ANE stays** where it measured well: batched prompt processing (1.16 ms for a
  4096x4096 product over 64 positions). Whether prefill runs on the GPU or the ANE is
  decided by measurement in M3, not in advance.

## Phases and gates

Each phase lands only with its evidence recorded (commit, run, device session).

| Phase | Work | Gate |
|---|---|---|
| M0 | Device, buffers, MSL build; bf16 and MXFP4 GEMV kernels and a benchmark | sustained >= 150 GB/s on Kimi-sized matrices; results within fp32-accumulation tolerance of the CPU reference |
| M1 | Kimi Linear decode fully on the GPU, bf16 experts through the existing RAM cache | per-layer relative error vs CPU reference below a fixed bound; same greedy tokens on the France and Japan prompts; tokens/s and idle CPU recorded |
| M2 | Offline MXFP4 conversion of the routed experts into a derived checkpoint on Jarraya (hash-recorded, the original untouched); all weights resident | **Jay's decision**, after a quality report: top-1 agreement and logit error vs bf16 on a fixed prompt set; then tokens/s, peak memory |
| M3 | Prompt processing: GPU GEMM vs ANE, measured on 64-2048-token prompts | tokens/s for both, pick the faster per size |
| M4 | Thermal and pacing on this Mac mini (`AGENTS.md`): idle and sustained generation, CPU/GPU utilisation, thermal-pressure state | no thermal warning over a 10-minute generation; nothing runs when idle |
| M5 | K3 on the same kernels (streamed trunk) | measured; expected to stay disk-bound |

## Risks

- bf16 in MSL needs Metal 3.1+ / Apple GPU family 9; M4 qualifies, older Macs may need an
  fp16 conversion at load (still once, not per token).
- 29 GB of resident weights on a 64 GB machine leaves room, but not for K3 at the same
  time; the launcher runs one model at a time.
- The KDA recurrence is sequential per token; on the GPU it parallelises across heads and
  state columns, which should suffice at decode but is the kernel most likely to need
  tuning.
- MXFP4 experts may lose quality; M2 measures before anything becomes the default.
