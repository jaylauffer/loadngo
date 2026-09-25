# System monitor

`system_monitor` (in `host-desktop`, behind the `system-monitor` feature) is a
small loadngo window that shows the machine it runs on:

- CPU load for the last three minutes: busy time (blue) with I/O wait (purple)
  stacked above it, the current value, and one bar per core;
- temperature, the thermal pressure band, and a three-minute temperature graph
  with faint lines where the band changes (fair, serious, critical);
- CPU clock (current / maximum);
- fan duty and speed where the board has a fan (Pi 5), and the supply-voltage
  alarm (`power ok` or `UNDER-VOLTAGE`) where a sensor reports one;
- memory and disk (`/` by default, `--disk PATH` to choose) as used / total;
- load average.

It runs on the lab Pis' desktops (agnes, dolores) as a corner widget.

## Where the numbers come from

| Shown | Source | Crate |
|---|---|---|
| CPU busy, I/O wait, per core | `/proc/stat`, difference between two samples | `loadngo-system-stats` |
| Memory | `/proc/meminfo` `MemTotal` / `MemAvailable` | `loadngo-system-stats` |
| Disk | `statvfs` on the chosen path (space available to users) | `loadngo-system-stats` |
| Clock | cpufreq `scaling_cur_freq` / `cpuinfo_max_freq` | `loadngo-system-stats` |
| Fan, supply voltage | hwmon `fan1_input` / `pwm1`, `in*_lcrit_alarm` | `loadngo-system-stats` |
| Temperature, band | CPU thermal zone via `ThermalZoneProvider`, band from `ThermalGovernor` | `loadngo-thermal` |

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

Everything other platforms cannot provide yet shows as `-` or is left out.
Nothing is shown as zero because it is unknown.

## Cost

It samples every two seconds from a host proactor deadline
(`FrameDemand::After`), reads temperature at the cadence
`ThermalPressure::sample_interval` allows (5 s when nominal), and repaints only
when a sample arrives or the window is resized. Pointer movement over the
window wakes it but draws nothing. Sampling reuses its buffers.

Measured 2026-09-25 over two minutes of an idle desktop, from `/proc/<pid>/stat`
and `/proc/<pid>/status`:

| Host | CPU (one core) | Context switches | Threads | RSS |
|---|---|---|---|---|
| agnes (Pi 4, GLES) | 0.32% | 6.5/s | 2 | 96.8 MB |
| dolores (Pi 5, GLES) | 0.11% | 5.0/s | 2 | 34.2 MB |

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

The script installs `~/.local/bin/loadngo-system-monitor`, adds a labwc window
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
