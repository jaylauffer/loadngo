# Local inference

`loadngo-inference` owns model-independent conversation state: bounded token
history, multi-turn generation, resumable truncation, undo/reset, cooperative
cancellation, output backpressure and UTF-8 streaming. It has no dependencies,
network requests, implicit threads or polling loop. This is new BSD-3-Clause
framework code, not a relocation of Apache-licensed Kimi implementation files.

The first consumer is `kimi-k3-in-rust`'s terminal chat. Kimi keeps its model
architecture, tokenizer, XTML format and CPU backend. Tensor reads already go
through Loadngo's proactor. `loadngo-weights` remains the generic weight-format
library; unifying the existing Kimi weight reader is separate, unfinished work.

`Session::begin_turn` takes backend-rendered tokens. `generate` calls a supplied
next-token function with the exact history and emits tokens synchronously, so a
slow output consumer naturally applies backpressure. Stop tokens remain in history
but are not emitted. A failed/cancelled/truncated reply remains pending; callers
must continue it or undo/reset, never append another user message to half a reply.
Context overflow is explicit, never silent history eviction.

GUI integration must dispatch inference through the host's bounded offload path
and post results as invalidations; do not call it from paint/input, add a timer
thread, or poll for output. An idle terminal blocks on input. Backends should
check the caller's cancellation flag at safe intermediate points; the session
itself checks between tokens. This crate does not promise preemption inside an
arbitrary kernel, KV caching, persistent transcripts, GPU or NPU acceleration.

Run `cargo test --offline -p loadngo-inference` and
`cargo clippy --offline -p loadngo-inference --all-targets -- -D warnings`.
Fixtures cover exact follow-up context, EOS, bounds, continuation, rollback,
failure, cancellation and UTF-8 split across token boundaries. Fake-token tests
validate session mechanics, not model quality or throughput.
