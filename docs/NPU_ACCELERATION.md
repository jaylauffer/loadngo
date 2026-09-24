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

Measurements and implemented API details will be appended after the hardware probe;
the sections above describe decisions and acceptance gates, not completed support.
