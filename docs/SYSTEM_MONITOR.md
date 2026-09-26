# System monitor

`system_monitor` (in `host-desktop`, behind the `system-monitor` feature) is a
small loadngo window that shows the machine it runs on:

- CPU load for the last three minutes: busy time (blue) with I/O wait (purple,
  Linux only) stacked above it, the current value, and one bar per core;
- GPU load for the last three minutes, with the GPU's kernel driver (Linux) or its
  power draw (macOS);
- on Apple silicon, the Neural Engine: its power draw now and for the last three
  minutes (macOS publishes no utilisation for it);
- temperature, the thermal pressure band, and a three-minute temperature graph
  with faint lines where the band changes (fair, serious, critical);
- CPU clock (current / maximum);
- fan duty and speed where the board has a fan (Pi 5), and the supply-voltage
  alarm (`power ok` or `UNDER-VOLTAGE`) where a sensor reports one;
- memory and disk (`/` by default, `--disk PATH` to choose) as used / total;
- load average.

It runs as a corner widget on the lab Pis' desktops (agnes, dolores) and on the Mac
mini. `--print` prints one reading as text and exits, without a window (useful over
ssh).

On macOS there is no public temperature, CPU clock or fan reading on Apple silicon, so
the temperature graph becomes one line: the thermal pressure band (from
`ProcessInfo.thermalState` through `loadngo-thermal`) and CPU and DRAM power.

## Where the numbers come from

| Shown | Source | Crate |
|---|---|---|
| CPU busy, I/O wait, per core | `/proc/stat`, difference between two samples | `loadngo-system-stats` |
| GPU busy | v3d `gpu_stats` (cumulative busy time per queue) under the first DRM render node; the busiest queue's share of the interval | `loadngo-system-stats` |
| Memory | `/proc/meminfo` `MemTotal` / `MemAvailable` | `loadngo-system-stats` |
| Disk | `statvfs` on the chosen path (space available to users) | `loadngo-system-stats` |
| Clock | cpufreq `scaling_cur_freq` / `cpuinfo_max_freq` | `loadngo-system-stats` |
| Fan, supply voltage | hwmon `fan1_input` / `pwm1`, `in*_lcrit_alarm` | `loadngo-system-stats` |
| Temperature, band | CPU thermal zone via `ThermalZoneProvider`, band from `ThermalGovernor` | `loadngo-thermal` |

On macOS:

| Shown | Source | Crate |
|---|---|---|
| CPU busy, per core | `host_processor_info` tick counters, difference between two samples | `loadngo-system-stats` |
| GPU busy | I/O Registry `IOAccelerator` `PerformanceStatistics` "Device Utilization %" | `loadngo-system-stats` |
| Power: CPU, GPU, Neural Engine, DRAM | IOReport "Energy Model" channels (`CPU Energy`, `GPU Energy`, `ANE`, `DRAM`), energy between two samples over the interval | `loadngo-system-stats` |
| Neural Engine name | I/O Registry `H11ANEIn` `DeviceProperties` (`ANEDevicePropertyNumANECores`) | `loadngo-system-stats` |
| Memory | `host_statistics64`: app memory (anonymous, not purgeable) + wired + compressed, as Activity Monitor counts "Memory Used"; total from `hw.memsize` | `loadngo-system-stats` |
| Disk | `statfs` on the chosen path; on APFS, used is the container's used space | `loadngo-system-stats` |
| Load, uptime | `getloadavg`, `kern.boottime` | `loadngo-system-stats` |
| Thermal band | `ProcessInfo.thermalState` | `loadngo-thermal` |

Everything on macOS works without root. IOReport (`/usr/lib/libIOReport.dylib`) is a
private Apple interface, the one sudo-free monitors use. If it changes, power shows
`-` and nothing else is affected. It is the only way to see Neural Engine activity
without root: the Neural Engine's power went from 0 W at idle to about 1.3 W while
Kimi ran on it (2026-09-26).

I/O wait is time a CPU sat idle while a task waited on I/O. On a Pi that is
usually the SD card, but a kernel worker stuck in uninterruptible sleep counts
too: agnes showed a steady 25% (one core of four) on 2026-09-25 with no disk
traffic in `vmstat`.

The band is the governor's published band, the one other loadngo consumers act
on, so it rises immediately and falls only after the recovery rules in
`THERMAL_AWARENESS.md`. On Raspberry Pi 4 and 5 the bands follow the firmware's
own limits: fair from 70 C, serious from 80 C (the firmware starts capping the
clock), critical from 85 C. Firmware throttling itself is not visible without
root, so the widget does not claim it; a low clock under load is the hint.

GPU load needs busy-time counters from the driver. Broadcom `v3d` (Raspberry
Pi 4 and 5) publishes them world-readable; other drivers show the driver name
and `-` until a source is added for them.

Everything other platforms cannot provide yet shows as `-` or is left out.
Nothing is shown as zero because it is unknown.

## Cost

It follows the loadngo proactor model for its timing: every two seconds is a
`FrameDemand::After` deadline, which the Linux host runs as a deferred
completion on its shared io_uring proactor, with the event loop waiting on it
(`ControlFlow::WaitUntil`); there is no timer thread, sleep or polling loop.
The sampling reads themselves (about ten procfs/sysfs files and one
`statvfs`) are synchronous on that tick. procfs/sysfs contents are generated
by the kernel on read and do not wait on the disk, but routing them through
the proactor's file reads would complete the model and is still to do. It
reads temperature at the cadence
`ThermalPressure::sample_interval` allows (5 s when nominal), and repaints only
when a sample arrives or the window is resized. Pointer movement over the
window wakes it but draws nothing. Sampling reuses its buffers.

Measured 2026-09-25 over two minutes of an idle desktop, from `/proc/<pid>/stat`
and `/proc/<pid>/status`:

| Host | CPU (one core) | Context switches | Threads | RSS |
|---|---|---|---|---|
| agnes (Pi 4, GLES) | 0.32% | 6.5/s | 2 | 96.8 MB |
| dolores (Pi 5, GLES) | 0.11% | 5.0/s | 2 | 34.2 MB |

On the Mac mini (M4 Pro, Metal), idle over a minute: 0.6% of one core, 6 threads,
89 MB resident. About 0.14% of that is IOReport:
each energy sample costs 2.8 ms of CPU time, whether it subscribes to four
channels or all 240. The rest is sampling and repainting every two seconds.

RSS grew by under 1.5 MB over the first ten minutes (allocator and driver
warm-up), then held flat on both Pis for the rest of a 10-minute series: no
growth from the value strings that change every sample. Most of it is the GL
driver.

## Installing on a labwc desktop

Build on the Pi (or on dolores and copy; one aarch64 binary runs on both the
Pi 4 and the Pi 5):

```sh
cargo build --release -p loadngo-host-desktop --features system-monitor --bin system_monitor
scripts/install-system-monitor.sh target/release/system_monitor
```

On macOS the same command installs the binary and a LaunchAgent,
`~/Library/LaunchAgents/com.loadngo.system-monitor.plist`, which starts it now and at
login. The monitor places its own window: no title bar, one level below ordinary
windows, on every Space, not in the Dock, top-right of the screen.

On Linux, the script installs `~/.local/bin/loadngo-system-monitor`, adds a labwc window
rule for app_id `loadngo-system-monitor` (no title bar, not in the task bar or
Alt-Tab, top-right, below other windows, on every workspace), adds an XDG
autostart entry, reloads labwc and starts the monitor. It backs up an existing
`~/.config/labwc/rc.xml` to `rc.xml.before-loadngo-system-monitor`. Its header
lists how to undo it.

The window is kept below other windows, like a desktop widget. To keep it on
top instead, replace `ToggleAlwaysOnBottom` with `ToggleAlwaysOnTop` in the
rule and reload labwc (`kill -HUP $(pgrep -x labwc)`).

The Linux host now sets the Wayland app_id as well as the X11 class from
`WindowDescriptor::linux_wm_class`, which is what the window rule matches.
