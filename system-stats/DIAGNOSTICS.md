# System monitor diagnostics — 2026-09-29

Source review and observations by Codex, against loadngo `f717b52a` /
`255f111e`. Full device evidence and limits are in the sibling workspace's
[`reviews/2026-09-29-system-monitor-codex.md`](../../reviews/2026-09-29-system-monitor-codex.md).
These are findings and proposed repairs; no runtime source or installed widget
was changed in this review.

## macOS ownership defects

The Mac widget (PID 8526, about 68 hours old) had a 1.0–1.1 GiB footprint,
mostly swapped, despite approximately 29–33 MiB process RSS. Its default malloc
zone grew from 979.3 to 980.9 MiB / 4,449,788 to 4,454,249 allocations over
four minutes. The exact attribution requires a corrected-binary comparison.

### Shared desktop host: missing autorelease scopes

`../host-desktop/src/macos.rs::run_event_loop` calls `pump_events_until`,
`drain_proactor` and `poll_entry_future` without an autorelease pool. The
initial window/backend setup and initial future poll are also unscoped.
`ns_date_for_timeout` uses an autoreleased NSDate, and
`../gfx-metal/src/lib.rs::present_scene` obtains autoreleased Metal objects.
Retaining an object in Rust does not itself drain its pending autorelease.

Apple describes the lifetime requirement in
[Using Autorelease Pool Blocks](https://developer.apple.com/library/archive/documentation/Cocoa/Conceptual/MemoryMgmt/Articles/mmAutoreleasePools.html).
This host supplies its own loop; it does not call `NSApplication::run` to
provide the usual surrounding event lifetime. Add a scope around each complete
iteration and appropriate initialization scopes, retaining objects that must
survive. Do not put a pool around the entire long-running loop, nor across an
async suspension. This is a strong explanation for heap growth, not a measured
allocation-stack attribution of every leaked byte.

### Sampler: unbalanced Mach host send rights

`src/macos.rs::read_cpu_times` and `memory_usage` each acquire a host send
right with `libc::mach_host_self()` on every sample but never deallocate it.
The CPU information array is separately freed by `vm_deallocate`; that does
not release the host right. Both successful calls and early errors need RAII
cleanup with `mach_port_deallocate(mach_task_self(), host)`.

Apple's [IOMasterPort implementation](https://github.com/apple-oss-distributions/IOKitUser/blob/main/IOKitLib.c)
explicitly balances the same acquisition. This defect is separate from the
large malloc footprint; do not claim it accounts for that heap. A focused
verification should compare send-right reference counts across repeated calls,
including failures, before and after cleanup.

`Owned` already releases CF Create/Copy results in the IOReport and registry
paths; `Service` releases IOKit objects. Those wrappers do not replace a host
autorelease pool or manage Mach rights created elsewhere.

## Linux I/O wait: interpretation and Agnes

`CpuTimes::from_proc_stat_fields` includes the first eight CPU fields, excludes
idle and iowait from busy, and avoids counting guest ticks twice.
`CpuLoad::between` computes interval deltas. Agnes's installed `--print` and
independent `vmstat` both showed 25%; the application is not merely painting
a stale cached label.

Linux documents limitations of the counter, including per-core attribution
and possible decreases, in
[/proc/stat](https://docs.kernel.org/filesystems/proc.html#miscellaneous-kernel-statistics-in-proc-stat).
I/O wait is not disk utilization or device health. A D-state task alone does
not establish the accounting cause.

Agnes had `procs_blocked=1`, yet today's root thread scan found no D-state
task and captured worker stacks were idle. `mmcblk0` showed zero reads, 16 KiB
written in 72 seconds and zero in-flight requests at both endpoints. The
historical 09-25 D-state worker explanation remains unverified today.
The kernel/driver cause is unresolved; accounting imbalance is a hypothesis.
Preserve the real reading and investigate independently of the renderer.

The Pi memory comparison was flat over 72 seconds: Agnes 50,800 KiB RSS /
19,484 KiB swap; Dolores 27,936 / 20,960 KiB, two threads and 16 descriptors
each. Both widgets had run about 68 hours; neither exhibits the Mac's large
footprint. This does not certify indefinite or interactive stability.

## Bounded caches and missing controls

- Monitor `History` is a fixed ring; the command scene is cleared before repaint.
- Metal text raster cache has a 512-entry ceiling. The monitor's text uses
  generated/transient images, so an unrelated persistent image registry should
  not be blamed without tracing the actual path.
- Linux `prepare_gles_frame` carries forward current generated images only;
  upload and GLES texture caches prune absent resources, then retire textures.
- `Monitor::run` does not consume input. No right-click menu, Close or Restart
  action exists. Add explicit lifecycle controls through host input/redraw
  handling, preserving sampling deadlines and preventing duplicate restarts.

## Thermal evidence

Agnes was approximately 58 C; Dolores approximately 47 C. Firmware reported
`0x80000` on Agnes and `0x0` on Dolores. Raspberry Pi's
[`get_throttled` bit table](https://www.raspberrypi.com/documentation/computers/os.html#get_throttled)
identifies `0x80000` as a past soft-temperature-limit event, not a current
limit. The command succeeded as user `jay`, without sudo. This is separate
from the widget's temperature-derived governor band. No active-load thermal
gate was attempted; no builds or stress runs accompanied this inspection.

## Proactor adoption: Jay's direction, 2026-09-29

Maximize use of the existing host-owned `loadngo-proactor`; do not introduce a
widget scheduler. The GUI already routes its two-second sampling deadline
through `FrameDemand::After`. Remaining synchronous work is
`Monitor::take_sample`: procfs/sysfs reads, disk-space queries, Mach/IOKit and
IOReport calls, plus thermal observations. Discovery also runs synchronously.

Source inspection of `proactor/src/lib.rs`, `proactor/src/file_offload.rs` and
`host-desktop/src/proactor_driver.rs` establishes these boundaries:

- `ProactorHandle::enqueue_work` posts a completion handler; it does **not**
  execute the handler on a background worker. Putting the entire sampler in
  that closure would still block the thread draining the host proactor.
- `IoPort::read` provides completion-based file reads. Readiness backends have
  lazy internal file workers; native completion backends use their own I/O
  path. Those private workers are not a public arbitrary-task API.
- The existing file worker count is bounded (4–16), but submission uses an
  unbounded channel. It should not be presented as bounded admission or used
  to justify unrestricted sampling submissions.

Implement the next slice through a shared host/proactor service:

1. Keep one deadline and **at most one in-flight sample per widget**. Coalesce
   missed ticks; never catch up with a burst after a stall. Use a coarse thermal
   deadline and reduce optional work under pressure.
2. Use completion reads with reusable bounded buffers for suitable Linux
   sources. Validate pseudo-file semantics and fallback behavior on real Linux;
   a successful regular-file read is not sufficient coverage.
3. Add or expose a shared bounded worker-offload service for calls without
   native asynchronous APIs, including filesystem-space and macOS sampling.
   Return immutable sample data through the host proactor and invalidate the
   UI once on completion. Rendering, window events and menus remain on the
   host thread. Keep the last completed sample visible during slow reads.
4. Give sampler state one owner. macOS CF/IOKit handles must be created and
   used on their intended worker, with its own per-job autorelease scope;
   do not add an unchecked `Send` implementation to move the current raw
   pointers between threads. Preserve buffers between jobs.
5. Close/Restart stops new admissions and invalidates late results. Drain or
   safely discard outstanding completions before releasing their buffers and
   host resources; an uncancellable syscall must not become a UI-thread join.
   Restart only once the old instance has released its window and work.

These are implementation requirements, not claims that an asynchronous sampler
or a generic worker API already exists. Repair ownership first, then compare
the proactor sampler against the synchronous baseline: values, idle CPU,
footprint including swap, wakeups, responsiveness with a delayed read,
cancellation, close/restart, and thermal pressure. Use serial low-rate runs;
neither a new per-widget thread nor a polling loop is needed.
