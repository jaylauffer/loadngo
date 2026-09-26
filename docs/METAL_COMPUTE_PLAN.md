# Metal compute backend for local models: plan

Status: M0 done 2026-09-26 (crate `metal-compute/`); M1 first stage done 2026-09-26 (Kimi
Linear's products on the GPU, below); the rest of M1 (attention, KDA and routing on the
GPU) and M3 onward not built.
Plan written 2026-09-24 (Claude Code, at Jay's request). Numbers marked *estimate* are
arithmetic from measured sizes, not measurements.

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

## M0 results (2026-09-26)

Built: crate `loadngo-metal-compute` (`metal-compute/`). `Gpu` opens the default device
and compiles the kernels from MSL source (safe math mode, so an E8M0 NaN scale stays
NaN). `Buffer` is shared-storage memory. A `Batch` is one command buffer with one compute
encoder, serial or concurrent with barriers. It owns its buffers until the GPU finishes.
`commit` returns at once, and the completion runs as a job on a loadngo proactor, which
hands the buffers back. There is no `waitUntilCompleted` and no polling. Kernels:
`gemv_bf16` and `gemv_mxfp4`, each with 1, 2 or 4 rows per simdgroup. A simdgroup's 32
lanes read each row in 16-byte loads and reuse every loaded slice of `x` for all of their
rows. Widths that are not a multiple of 8 (bf16) or 32 (MXFP4) take a scalar path.

**Gate met.** `metal_gemv_bench` (random weights in shapes approximating one
Kimi-Linear-48B-A3B decode step, timed by the command buffer's GPU start and end, median
of 12 steps, Apple M4 Pro, no thermal warning before or after):

| Workload | Dispatches | GB per step | 1 row | 2 rows | 4 rows |
|---|---|---|---|---|---|
| LM head, bf16 163840x2304 | 1 | 0.755 | 234 GB/s | 257 GB/s | 253 GB/s |
| Trunk, bf16, 27 layers + LM head | 190 | 3.289 | 239 GB/s | 237 GB/s | 238 GB/s |
| Routed experts, MXFP4, 26 x 8, serial | 624 | 0.782 | 83 GB/s | 100 GB/s | 119 GB/s |
| Routed experts, MXFP4, concurrent | 624 | 0.782 | 113 GB/s | 147 GB/s | 170 GB/s |
| Whole step, serial | 814 | 4.071 | 178 GB/s | 184 GB/s | 201 GB/s |
| Whole step, concurrent + barriers | 814 | 4.071 | 204 GB/s | 220 GB/s | **228 GB/s** |

- 4 rows per simdgroup is fastest overall, and it is the default.
- The weight reads of one decode step take 17.9 ms. That caps decoding at about 56
  tokens/s before attention, the KDA recurrence, routing and sampling are added (M1).
  Today's Neural Engine path does 2.6 tokens/s.
- Small matrices want concurrency. One expert matrix is 1.2 MB, and in serial order
  each dispatch waits for the one before it. So the concurrent encoder with a barrier
  between dependent groups (attention, then gate and up, then down) is the shape for M1.
- The experts come from a 4.3 GB pool, so no two steps read the same ones. The trunk
  alone is 3.3 GB. Both are far larger than the GPU's caches, so these are memory reads,
  not cache hits.
- **Correctness.** Before timing, every run checks its outputs against `loadngo-weights`
  (`HalfMatrix::mul_vec`, `Mxfp4Matrix::mul_vec`). The bound per row is the worst-case
  float32 summation error, `cols * eps * sum |w x|`. 15.9 million rows were checked
  across the full run.
- **Tests** (`metal-compute/tests/gemv.rs`):
  - The vector and scalar paths agree with the CPU reference for all three variants,
    with partial simdgroups and odd widths.
  - A NaN scale poisons only its own row.
  - A barrier orders two dependent products.
  - Bad dispatches are refused before encoding.
  - A batch dropped without being committed is harmless.
  - Swapping the bf16 halves, or the nibble order on the scalar MXFP4 path, fails the
    tests.
- **Approximations.** Every layer is modelled with four 4096x2304 projections. The
  real MLA layers and the KDA gate projections differ a little in shape, and their
  small matrices are left out. Chrome and WindowServer were using the GPU lightly
  during the runs.

Rerun: `cargo run --release -p loadngo-metal-compute --bin metal_gemv_bench -- --iterations 12`
(about 1 minute, 7.6 GB of buffers).

## M1, first stage (2026-09-26): every product on the GPU

The plan called for the whole decode step on the GPU at once. This first stage moves the
products only, which are almost all of the bytes, behind Kimi's existing `DenseAccel`
interface. It is what `--accel gpu` runs today.

Built:

- **`Resident`.** Read-only weight memory that many batches read at once and the CPU
  reads in place. Residents are carved from 1 GiB arenas, so a batch that touches
  thousands of weights names only a few dozen Metal buffers. `Batch::attach` returns the
  slice to use.
- **Untracked buffers.** Ordering comes from ownership and `Batch::barrier`.
- **`gemm_bf16` and `gemm_mxfp4`.** Several positions per dispatch, eight positions per
  weight read.
- **kimi-k3-in-rust.** `DenseAccel::share_words` and `share_bytes`, plus
  `LinearModel::share_weights`, move the trunk, the LM head and the resident MXFP4
  experts into GPU memory once, 28.3 GB in about 5.5 s. The CPU reference reads the same
  bytes afterwards. Every step's products become one command buffer. See
  `kimi-k3-in-rust/docs/KIMI_LINEAR.md`.

Measured on the M4 Pro with the 4-bit experts, with no thermal warning at any point:

| | Neural Engine (`--accel ane`) | GPU (`--accel gpu`) |
|---|---|---|
| Decode, short context | 2.6 tokens/s | 14-20 tokens/s (0.05 s/token at ~100 tokens of context) |
| One-sentence chat reply | 6.0 s first, 5.5 s after `/reset` | 8.3 s first, 3.4 s after |
| 70-token prompt | 5.8 s | 4.6 s |
| 819-token tool preamble at launch | 29 s | 30 s |
| Memory footprint | 31.6 GB peak | 30 GB steady, 41 GB peak while loading |
| Idle at the prompt | | 0% CPU over 20 s |

- **Correctness.** The same text was compared against the CPU reference one position at
  a time (`--compare decode`) and as a whole-text pass (`--compare cpu`). Top-1
  agreement was 100%, the KL divergence rounds to 0.00000 at five decimals, and
  perplexity was identical. The Neural Engine's fp16 path sits at KL 0.012 on the same
  kind of text.
- **Prompts are now CPU-bound.** A `sample` taken during the preamble puts about 54% of
  the main thread in the model's own single-threaded CPU code: MLA attention
  (quadratic in the prompt, in f64), the router and the KDA recurrence. Only about 19%
  is spent waiting on the GPU. That is the rest of M1.
- **Decode will slow down as the context grows.** The MLA attention done on the CPU
  grows with context length.
- **Findings from building it:**
  - The bf16 multi-position kernel reads 8 positions in the time of one (121 us for
    4096x2304).
  - The first MXFP4 multi-position kernel was 2-4x slower than one product per position.
    It re-reads each position's input once per weight row, so Kimi uses per-position
    products for experts. A threadgroup-staged input is the fix.
  - The first use of freshly filled arenas costs about 56 ms, once per process.
  - Hazard tracking and CPU gaps between submissions (about 0.5 ms) were not the cost.
  - A prompt step still waits about 20 ms beyond its 6 ms of GPU time in Kimi, but not
    in isolation. This is unexplained.
- **Memory.** Loading holds the heap and GPU copies of the experts together for a
  moment, which is the 41 GB peak. Reading experts straight into GPU memory would
  remove it.
- `metal-compute/tests/gemm_timing.rs` holds the timing experiments (ignored tests, run
  by hand).

## M1, second stage: what we are attempting (for review; not started)

Written 2026-09-26 at Jay's request ("before starting on KDA routing to the GPU we need to
understand clearly what we're attempting").

### Where one token's time goes today

These are steady-state `--accel gpu` decode figures at about 350 tokens of context:
0.06-0.07 s per token (about 15 tokens/s). The split is from a `sample` of the main
thread over 6 s, so treat it as approximate.

| Share | About | What |
|---|---|---|
| ~66% | 43 ms | Waiting on the GPU. About 20 ms of that is the GPU reading the weights (the M0 measurement). The rest is the cost of about 136 separate round trips per token: every step's products are one submission, and the CPU waits for each. |
| ~13% | 8.5 ms | The router on the CPU: 26 layers x 256 expert scores over 2304 inputs, then top-8. |
| ~9% | 6 ms | Encoding and submitting those ~136 command buffers. |
| ~7% | 4.5 ms | MLA attention (7 layers) and glue: norms, residual adds, SiLU, copies. |
| ~3% | 2 ms | The KDA recurrence and short convolutions (20 layers). |

### What would move

At present the CPU and GPU take turns about 136 times per token. The goal is **one
submission per token**. The CPU sends the token id; the GPU runs all 27 layers and the
LM head; the CPU gets back the logits (or just the chosen token) and samples. Per layer,
these move to GPU kernels:

- **RMSNorm** (before attention and before the MLP) and the **residual adds**. Simple.
- **KDA** (20 layers):
  - the short convolution over the last 4 positions, which keeps a small state;
  - L2 normalisation of q and k;
  - the decay gate, `g = -exp(A_log) * softplus(z + dt_bias)`;
  - the delta-rule recurrence on a 128x128 state per head (32 heads, 2 MB per layer);
  - a per-head RMSNorm times a sigmoid gate.

  It is sequential over positions, but each position's work is 32 heads x 128 x 128,
  which parallelises well.
- **MLA attention** (7 layers). Append to the KV cache, then compute scores against
  every cached position, softmax, and a weighted sum. Its cost grows with the length of
  the conversation, and it is what makes long chats slower.
- **The router.** Sigmoid scores for 256 experts, the selection bias, top-8,
  renormalise and scale.
- **Expert selection on the GPU. This is the one genuinely new piece.** Today the CPU
  picks the 8 experts and then submits their products. When the router runs on the
  GPU, the expert kernels must read *which* experts from GPU memory ("indirect"
  dispatch). That needs a table of every expert's weight location, and all 6,656
  experts are already resident, so the table is fixed at load.
- **Combining the experts.** A weighted sum of the 8 expert outputs plus the shared
  expert.

The session state moves into GPU memory too:

- the KDA recurrent and convolution state: about 40 MB for 20 layers;
- the MLA KV cache: about 134 MB per MLA layer at 4096 tokens, about 0.9 GB in total.

The chat's "tool preamble snapshot" then becomes a GPU buffer copy.

### What we expect

The weight reads (~20 ms) stay, and nearly everything else goes. That makes about
20-25 ms per token, or **40-50 tokens/s at short context**, against about 15 now. This
is an estimate from the table above, not a measurement. Long conversations should also
stop slowing down as much, because attention runs in parallel on the GPU.

Prompts use the same kernels over many positions. KDA stays sequential across
positions. Faster chunked forms of KDA exist but are a later step.

### How we know it is right

- The CPU path stays the reference.
- Each kernel gets tests against the CPU op it replaces, as M0 did.
- The whole model must still pass `--compare decode` and `--compare cpu`.
- Today's GPU path matches the CPU exactly: KL 0.00000 at five decimals. The CPU's MLA
  sums in f64 and the GPU will use f32, so expect a very small non-zero KL. The bound
  should be agreed before we start, for example mean KL below 0.001 and top-1 agreement
  of at least 99%.

### What it does not do: the Neural Engine

`--accel gpu` leaves the Neural Engine idle. The widget's 0 W NPU reading while Kimi
runs is correct. The reason is measured: the Neural Engine streams weights at 15-29
GB/s, against the GPU's 228 GB/s, and both draw on the same 273 GB/s of memory. During
decoding the GPU already uses most of that bandwidth, so the Neural Engine cannot add
speed there. It could help where the GPU is not bandwidth-bound:

- **Speculative decoding.** A small draft model on the Neural Engine guesses several
  tokens, and the big model on the GPU checks them in one pass. This would use the
  NPU for real. It needs a draft model that shares Kimi's tokenizer, and none has been
  chosen.
- **Prompt processing on the Neural Engine while the GPU decodes.** This helps only
  when there is a prompt to read and a reply to write at the same time, for example
  tool results arriving mid-reply.

Neither is part of this stage.

### The work, in order

1. `loadngo-metal-compute` kernels, each tested against a CPU reference:
   - RMSNorm;
   - residual add;
   - SiLU-gate;
   - short convolution with state;
   - KDA step;
   - MLA attention over a KV cache;
   - sigmoid router top-k;
   - indirect MXFP4 expert matrix-vector product;
   - weighted combine;
   - argmax.
2. A model-level backend in kimi (`LinearBackend`), with the CPU forward as the
   reference and fallback. The session state lives in GPU buffers.
3. One command buffer per token, completed through the proactor.
4. The gate: `--compare decode` and `--compare cpu` within the agreed bound, tokens/s,
   idle CPU, and the thermal state over a 10-minute generation.

This is several sessions of work. Kernels 1-3 (norms and adds) are small. KDA, MLA
attention and the indirect experts are the substance.

### Decisions for Jay

- **The agreement bound.** For example mean KL below 0.001 and top-1 agreement of at
  least 99%.
- **Scope.** Decode only first, with prompts later; or both together.
- **Speculative decoding.** Whether to pursue it on the Neural Engine as its own
  project.

## Risks

- bf16 in MSL needs Metal 3.1+ / Apple GPU family 9; M4 qualifies, older Macs may need an
  fp16 conversion at load (still once, not per token).
- 29 GB of resident weights on a 64 GB machine leaves room, but not for K3 at the same
  time; the launcher runs one model at a time.
- The KDA recurrence is sequential per token; on the GPU it parallelises across heads and
  state columns, which should suffice at decode but is the kernel most likely to need
  tuning.
- MXFP4 experts may lose quality; M2 measures before anything becomes the default.
