# Persistent framework logging

Implemented locally on 2026-10-04 at Jay's request. Each supported
`host-desktop::launch` initializes logging automatically after the platform
proactor exists. The normal application command needs no wrapper.

The host uses `WindowDescriptor::linux_wm_class` as the stable application
identity on every platform (fallback: `loadngo`). Logs live in that identity's
platform app-data directory, under `logs/`. The startup console prints the path.
Android uses its private app container. Profile files and other application
data have independent lifetimes.

## Storage and overload policy

Default limits, enforced by `loadngo_proactor::PersistentLogConfig`:

| Resource | Limit |
|---|---|
| Owned log files | Eight, including the active file |
| Each file | 1 MiB |
| Total log contents | At most 8 MiB per application |
| Queued bytes | 128 KiB |
| Individual message | 16 KiB; longer UTF-8 messages get a truncation marker |
| File age | 14 days; pruned on startup and rotation |
| Concurrent flush waiters | 32 |
| File work | One worker, one in-flight batch |

`log.0.txt` is current; `log.1.txt` through `log.7.txt` are progressively
older. Rotation discards the oldest before shifting files. A newly reduced
quota also removes obsolete owned files, and oversized existing files are
removed before new writes. The empty `writer.lock` file is an OS advisory
lock; a second process cannot rotate a first process's logs. No other files
are pruned. The directory is dedicated to framework logs.

Queued overload drops new records; record headers include cumulative drop
counts, and `persistent_log_status()` exposes accepted, persisted, dropped,
truncated and error fields. Disk-full, permission and locking errors disable
capture with one console error. They do not block gameplay or cause a retry
loop. Console output continues. Relaunch retries initialization.

Retention is deliberately finite. Copy relevant logs into a separate evidence
archive before they age out or rotate; this directory is not an unlimited
playtest history or a backup service.

## Proactor and host contracts

The logger uses the **existing host proactor**:

- The first buffered record requests one 250 ms batching deadline through
  `ProactorHandle::defer_for`. No independent timer or polling loop exists.
- One bounded framework worker performs directory creation, file locking,
  rotation, appends and `sync_data`. Buffers are reused. This is blocking
  offload; `enqueue_work` is used only to deliver the resulting completion.
- The proactor dispatches persistence acknowledgements and wakes flush
  futures. A completion waiting for dispatch is bounded to one during normal
  operation. `flush_logs().await` requests immediate delivery to the worker.
- Native wake messages wake AppKit/winit so even an idle host processes a
  completion. They do not advance simulation or request a frame. Android's
  existing dedicated proactor pump already dispatches completions.
- Host teardown drains the bounded queue and joins the worker before disposing
  the proactor. Teardown does not depend on future completion dispatch.

The first backend uses bounded blocking file offload for this whole append and
rotation transaction. Native `IoPort::write` alone cannot cover directory
creation, rename, file locks or sync, and the Linux host's mixed io_uring/epoll
port exposes `CompletionPort` rather than `IoPort`. This implementation does
not claim kernel-native asynchronous appends on Linux/Windows.

On macOS, wake events use [Apple's documented subthread-safe event posting
API](https://developer.apple.com/documentation/appkit/nsapplication/postevent%28_%3Aatstart%3A%29?language=objc)
and an autorelease scope. The host recognizes the logging wake without
incrementing the input event epoch or waking an idle `next_frame`.

## Application usage and evidence

```rust
loadngo_host_desktop::log_info(format_args!("{}", report));
loadngo_host_desktop::log_error(format_args!("save failed: {error}"));
loadngo_host_desktop::flush_logs().await?;
```

Framework library console diagnostics are routed through this sink, including
Android's native log helpers. Applications must route their own messages
through `log_info`/`log_error` too. Arbitrary dependency stdout/stderr, native
crash dumps and panic hooks are not intercepted by redirecting process file
descriptors. Android's existing panic hook does use the native helper.

Records include Unix milliseconds, sequence, severity and cumulative drops,
followed by the full message (including multiline reports). Batch writes are
synced before acknowledgement. Explicit flushes preserve important boundaries;
abrupt process termination may lose the pending 250 ms buffer or interrupt a
file append. A trailing partial record is possible after a crash. Power-loss
durability of directory renames is not claimed.

Deterministic tests cover deadline dispatch, acknowledgement through the
supplied proactor, shutdown draining, Unicode and queue bounds, age/size/count
pruning, disk failures, and exclusive writers. Platform compilation and runtime
validation are recorded in the handoff; these rules alone are not a thermal
measurement.

## Validation on 2026-10-04

- macOS: affected host/proactor unit, integration and doc tests passed;
  strict workspace all-target/all-feature Clippy passed with CI's platform
  exclusions. A temporary native-window smoke executable verified an idle
  `flush_logs().await`, multiline persistence, and final shutdown draining;
  it exited automatically. Its retained logs are under the separate
  `sng-roguelite-log-validation` app identity.
- Dolores: 35 host and 11 proactor unit tests passed, including the new epoll
  completion/wake and concurrent-shutdown regression tests. Affected-crate
  all-target/all-feature strict Clippy passed. Full workspace validation was
  restarted with targets on the main disk after the 2 GiB `/tmp` filesystem
  filled; the temporary targets were removed to restore free space. Whole
  workspace Clippy then passed. The subsequent test run was stopped at Jay's
  request to use CI for Linux validation; full Linux workspace tests are not
  reported as passed.
- iOS host all-target compilation and Android host library compilation with
  NDK 29 passed. Android's existing `file_dialog_harness` fails all-target
  compilation because its future is not `Send`; logging library compilation
  succeeds. Windows proactor cross-compilation passed; Windows host runtime
  and mobile runtime validation remain open.
- Full macOS workspace tests were interrupted during a CPU-heavy Core ML
  attention test; they are not reported as passed. Later whole-workspace fmt
  checking also encountered a concurrent peer edit in `metal-compute`.
  The logging paths passed formatting checks and were left separate from
  those peer changes. No thermal or combat-balance claim follows from these
  checks.

Subsequent work stays on the development Mac and lets CI check Linux. Direct
Linux development/testing requires a specifically agreed need; sustained
Linux work may instead use a Codex instance running there. No remote
validation process remains running from this task.
