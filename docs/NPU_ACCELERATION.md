# NPU acceleration: decisions and evidence

## Conversation decisions, 2026-09-24

Jay asks for a platform-agnostic Loadngo NPU API, reuse of the proactor, and an
actual demonstration of faster Kimi computation using the Mac mini Neural Engine.
He explicitly asks that conversations and findings be documented. This file is
the design/evidence record; Kimi-specific results belong in
`kimi-k3-in-rust/docs/APPLE_NEURAL_ENGINE.md`. Do not turn a proposed API or an
available device into a claim of working acceleration.

### Responsibility boundary

Loadngo should own device capabilities, execution policy, tensor/shape validation,
bounded submission, completion delivery, cancellation and honest placement reports.
Platform adapters own native model artifacts, compilation, device selection and
execution. Kimi owns its architecture, weight binding, graph partitioning and
numerical acceptance tests. Start with one real Apple adapter; do not invent
unimplemented Windows/Android backends or a universal graph compiler.

On Apple, use public Core ML APIs. `CPUAndNeuralEngine` permits CPU fallback; it
does not force every operation onto ANE. Distinguish device discovery, requested
policy, planned placement, runtime hardware evidence and measured latency.
`MLComputePlan` describes anticipated placement, not a hardware execution trace.

### Proactor integration

Existing `ProactorHandle::enqueue_work` posts a completion handler; it does **not**
offload a long-running kernel. Do not place synchronous Core ML prediction inside
that handler: it would block event dispatch. Use native asynchronous prediction
where available, or a bounded platform-adapter worker for blocking calls; post the
owned result back through the existing proactor. Limit in-flight work, preserve
input/model lifetimes through completion, and drain outstanding work on shutdown.
Cancellation may discard results when the platform cannot preempt execution.
Never add a busy poll, timer thread, or per-token thread.

### First implementation/measurement gate

1. Compile a small, deterministic dense projection with supported public APIs.
2. Compare the same inputs/weights against a Rust reference and Core ML CPU-only.
3. Inspect the compute plan with CPU+ANE policy and record fallback explicitly.
4. Separate compile/load, first prediction, warmed median and I/O/copy costs.
5. Repeat with an actual Kimi matrix before claiming Kimi kernel acceleration;
   only then integrate a bounded partition and measure end-to-end token latency.

The full checkpoint is streamed and far larger than RAM. A faster resident
projection does not remove weight I/O, compilation/cache costs, recurrence or
repeated prefix compute. No end-to-end speedup is established at this point.

## Sources checked

- Apple Core ML `MLComputeUnits`, `MLComputePlan`, `MLComputePlanDeviceUsage`.
- Apple Core ML model-format schema and neural-network format reference.
- Local Rust `objc2-core-ml 0.3.2` bindings and actual Loadngo proactor source.

The sections above are Codex's original decisions and gates. What was then measured and
built follows.

## Measurements and implementation, 2026-09-24 (Claude Code, taking over from Codex)

Hardware: this Mac mini, Apple M4 Pro, 64 GB, macOS 26.6.2. All numbers are medians of
warmed repetitions measured in a scratch Core ML lab crate or with the tests named below;
placement is Core ML's `MLComputePlan` preferred device for the one layer in each model.

### The deciding measurement: streamed weights

Kimi K3 cannot keep its weights resident (a 109 GB bf16 trunk and 1.4 TB of experts on a
64 GB machine), so a baked-weight Core ML model per matrix would have to be compiled and
loaded on every token. The question was whether the ANE can multiply by a weight that
arrives as a runtime input. It can, but only through an IOSurface-backed buffer:

| 4096x4096 fp16 product | P=1 | P=64 |
|---|---|---|
| weights baked into the model, CPU (Core ML/AMX) | 1.26 ms | 3.09 ms |
| weights baked into the model, ANE | 2.0 ms | 1.16 ms |
| weight as a heap `MLMultiArray` input, ANE | 9.9 ms | 10.1 ms |
| weight as an IOSurface-backed `MLMultiArray` input, ANE | **1.16 ms** | **1.38 ms** |

A heap input is copied/relaid by Core ML at about 3.4 GB/s, no better than the external
drive. An IOSurface input (`CVPixelBuffer`, `kCVPixelFormatType_OneComponent16Half`, via
`MLMultiArray initWithPixelBuffer:shape:`) is read by the ANE at 24-29 GB/s of weights,
the same as baked weights, and the plan places it on the Neural Engine.

Limits found: the ANE takes this layer for dimensions up to exactly 16384 (16385 is
planned on the CPU; 7168x24576 and 33792x7168 both fell back). Tiles up to ~12288 stream
at 26-29 GB/s; 16384 drops to 12-18 GB/s. Expert-sized products (3584x3072) take 0.79 ms
at P=1 and P=4 alike, so per-prediction overhead is small next to weight bandwidth.

### What exists now

- `coreml::model::encode_dynamic_matmul`: `BatchedMatMul` with `transposeB`, fp16 `x`,
  `w` and `y` features (spec v7). Portable, no unsafe.
- `coreml::dense::DenseEngine` (macOS, audited unsafe): one compiled model per
  (row bucket, tile) shape, reusable IOSurface weight/input buffers, tiling to 12288 with
  fp32 accumulation across column tiles, rows padded to powers of two up to 256.
  `matmul_bf16` and `matmul_mxfp4` (OCP MX v1.0: E2M1 codes, E8M0 scale per 32).
  Stats record conversion, prediction and compile time, and how many compiled shapes the
  plan did not put on the NPU.
- Conversion is on the CPU. bf16 to fp16 uses NEON `shll`/`fcvtn` (round to nearest
  even, subnormals kept): 31.8 GB/s single-threaded, versus 4.5 GB/s for the first
  version, which cost 74 s per three Kimi tokens. MXFP4 to fp16 uses `tbl` lookups plus
  an exponent add, exact when the result is fp16-normal and hardware-rounded otherwise:
  15.5 G elements/s measured while another model run shared the CPU.
- Values outside fp16's range (weights, activations, results) are refused with an error,
  never saturated, so the caller computes that product on its own reference path.

Predictions are synchronous from the model's compute loop, which would otherwise be
running the same product on the CPU; nothing runs inside a proactor completion handler or
UI callback. Overlapping conversion of the next tile with the current prediction (the
async API) is the next refinement, not yet built.

### Verification

`cargo test -p loadngo-coreml` on this Mac: 8 tests, including exhaustive checks that the
bf16 conversion matches hardware rounding for all 65,536 bf16 patterns (both the vector
and the scalar path) and that MXFP4 expansion matches it for every code and every finite
scale; tiled bf16 and MXFP4 products against f64 references on the real ANE; refusal of
out-of-range values. Strict Clippy passes for macOS, aarch64 Linux and x86_64 Windows
targets (the engine is macOS-only; elsewhere only the portable encoder builds).

Kimi K3 results on the released checkpoint are recorded in
`kimi-k3-in-rust/docs/APPLE_NEURAL_ENGINE.md`.

### Still open

- Windows/Android NPU backends: none. The `compute` API has one real backend.
- Async double-buffering (convert tile n+1 while tile n predicts).
- No int8/int4 weight path: Core ML palettized weights need baked models.
- No hardware execution trace; placement evidence is the compute plan plus the fp16
  error signature and timing, not an ANE counter.
