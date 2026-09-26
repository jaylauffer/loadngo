//! A small desktop monitor for the machine it runs on: CPU load (busy and
//! I/O wait, overall and per core), GPU load, Neural Engine power (Apple silicon),
//! temperature and thermal pressure, CPU clock, fan, supply voltage, power draw,
//! memory, disk and load average -- whatever the platform reports.
//!
//! It samples every two seconds from a host proactor deadline
//! (`FrameDemand::After`), reads temperature at the cadence `loadngo-thermal`
//! sets for the current pressure, and repaints only when a sample arrives or
//! the window changes size, so pointer events over it cost no drawing. The
//! thermal band shown is the governor's published band, the one loadngo
//! consumers act on. See `docs/SYSTEM_MONITOR.md` for the lab-desktop setup.

use loadngo_host_core::{FrameDemand, WindowDescriptor};
use loadngo_system_stats::{SystemSample, SystemSampler};
use loadngo_thermal::{
    GovernorConfig, ThermalGovernor, ThermalObservation, ThermalPressure, ThermalProvider,
    ThermalZoneProvider, TripPoints,
};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use ui_core::{
    Color, HorizontalAlign, PaintOp, Rect, TextLayoutMode, TextOverflow, TextStyle,
    TextVerticalMetricMode, VerticalAlign,
};

const WINDOW_WIDTH: i32 = 320;
const SAMPLE_EVERY: Duration = Duration::from_secs(2);
/// Three minutes of history at one sample every two seconds.
const HISTORY: usize = 90;
/// Temperature graph range; trip lines outside it are clamped to its edges.
const GRAPH_MIN_C: f32 = 30.0;
const GRAPH_MAX_C: f32 = 90.0;

const BACKGROUND: Color = Color::rgba(0x0d, 0x12, 0x1b, 0xff);
const GRAPH_BACKGROUND: Color = Color::rgba(0x19, 0x22, 0x31, 0xff);
const TEXT: Color = Color::rgba(0xec, 0xf1, 0xfb, 0xff);
const MUTED: Color = Color::rgba(0x8a, 0x98, 0xad, 0xff);
const ACCENT: Color = Color::rgba(0x68, 0xc9, 0xee, 0xff);
const NOMINAL: Color = Color::rgba(0x74, 0xd2, 0x9a, 0xff);
const FAIR: Color = Color::rgba(0xf2, 0xd0, 0x5c, 0xff);
const SERIOUS: Color = Color::rgba(0xf2, 0x9a, 0x4c, 0xff);
const CRITICAL: Color = Color::rgba(0xf0, 0x6a, 0x6a, 0xff);
const IOWAIT: Color = Color::rgba(0xb0, 0x7c, 0xe8, 0xff);

const USAGE: &str = "\
system_monitor: a small desktop window showing this machine's CPU and GPU load,
Neural Engine power (Apple silicon), temperature or thermal pressure, clock,
fan, supply voltage, power draw, memory, disk and load average, as far as the
platform reports them. Samples every 2 s; repaints only when a sample arrives.

Usage: system_monitor [--disk PATH] [--print]

  --disk PATH   optional  filesystem whose usage to show (default: /)
  --print       optional  print one reading (two samples 2 s apart) and exit,
                          without opening a window
  -h, --help              print this help

Example:
  system_monitor --disk /home
";

fn main() {
    let mut disk = PathBuf::from("/");
    let mut print = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return;
            }
            "--print" => print = true,
            "--disk" => match args.next() {
                Some(path) => disk = PathBuf::from(path),
                None => fail("--disk needs a path"),
            },
            other => fail(&format!("unknown argument {other:?}")),
        }
    }
    let monitor = Monitor::new(disk);
    if print {
        monitor.print_reading();
        return;
    }
    let height = monitor.layout.height();
    loadngo_host_desktop::launch(window_descriptor(&monitor.host, height), None, async move {
        monitor.run().await;
    });
}

fn fail(message: &str) -> ! {
    eprintln!("system_monitor: {message} (see --help)");
    std::process::exit(2);
}

fn window_descriptor(host: &str, height: f32) -> WindowDescriptor {
    WindowDescriptor {
        title: format!("{host} system monitor"),
        width: Some(WINDOW_WIDTH),
        height: Some(height as i32),
        high_dpi: true,
        linux_wm_class: Some("loadngo-system-monitor"),
    }
}

/// Fixed-size ring of the last [`HISTORY`] values; never reallocates.
struct History {
    values: [f32; HISTORY],
    len: usize,
    next: usize,
}

impl History {
    const fn new() -> Self {
        Self {
            values: [0.0; HISTORY],
            len: 0,
            next: 0,
        }
    }

    fn push(&mut self, value: f32) {
        self.values[self.next] = value;
        self.next = (self.next + 1) % HISTORY;
        self.len = (self.len + 1).min(HISTORY);
    }

    /// Oldest first.
    fn iter(&self) -> impl Iterator<Item = f32> + '_ {
        let start = (self.next + HISTORY - self.len) % HISTORY;
        (0..self.len).map(move |i| self.values[(start + i) % HISTORY])
    }

    fn max(&self) -> f32 {
        self.iter().fold(0.0, f32::max)
    }
}

/// Which optional sections this machine gets, fixed at start-up so the window keeps
/// its size: a Neural Engine row where one is named, a temperature graph where a
/// thermal zone reports temperature (otherwise one line with the pressure band and
/// power), and a fan/supply row on Linux.
struct Layout {
    npu: bool,
    temp_graph: bool,
    supply_row: bool,
}

impl Layout {
    const NPU: f32 = 56.0;
    const TEMP_GRAPH: f32 = 72.0;
    const ROW: f32 = 24.0;

    /// Design-space height: the fixed rows above `166`, the optional sections, then
    /// memory, disk and load average and a bottom margin.
    fn height(&self) -> f32 {
        let mut y = 166.0;
        if self.npu {
            y += Self::NPU;
        }
        y += if self.temp_graph {
            Self::TEMP_GRAPH
        } else {
            Self::ROW
        };
        if self.supply_row {
            y += Self::ROW;
        }
        y + 2.0 * Self::ROW + 36.0
    }
}

struct Monitor {
    host: String,
    sampler: SystemSampler,
    sample: SystemSample,
    thermal: Box<dyn ThermalProvider>,
    trips: Option<TripPoints>,
    governor: ThermalGovernor,
    observation: ThermalObservation,
    next_thermal_at: Instant,
    busy_history: History,
    iowait_history: History,
    gpu_history: History,
    npu_history: History,
    temp_history: History,
    layout: Layout,
    scene: Vec<PaintOp>,
}

impl Monitor {
    fn new(disk: PathBuf) -> Self {
        let zone = ThermalZoneProvider::discover();
        let trips = zone.as_ref().map(ThermalZoneProvider::trips);
        let thermal: Box<dyn ThermalProvider> = match zone {
            Some(zone) => Box::new(zone),
            None => loadngo_thermal::platform_provider(),
        };
        let sampler = SystemSampler::new(&disk);
        let layout = Layout {
            npu: sampler.npu_name().is_some(),
            temp_graph: trips.is_some(),
            supply_row: cfg!(target_os = "linux"),
        };
        Self {
            host: loadngo_system_stats::hostname().unwrap_or_else(|| "this machine".to_string()),
            sampler,
            sample: SystemSample::default(),
            thermal,
            trips,
            governor: ThermalGovernor::new(GovernorConfig::default()),
            observation: ThermalObservation::unavailable(),
            next_thermal_at: Instant::now(),
            busy_history: History::new(),
            iowait_history: History::new(),
            gpu_history: History::new(),
            npu_history: History::new(),
            temp_history: History::new(),
            layout,
            scene: Vec::with_capacity(256),
        }
    }

    /// `--print`: two samples two seconds apart (usage needs an interval), as text.
    fn print_reading(mut self) {
        let now = Instant::now();
        self.take_sample(now);
        std::thread::sleep(SAMPLE_EVERY);
        self.take_sample(now + SAMPLE_EVERY);
        let s = &self.sample;
        let pct = |v: f32| format!("{:.0}%", v * 100.0);
        let watts = |w: Option<f32>| w.map_or_else(|| "-".to_string(), |w| format!("{w:.2} W"));
        let power = s.power.unwrap_or_default();
        println!("host      {}", self.host);
        if let Some(cpu) = &s.cpu {
            let cores: Vec<String> = cpu.cores.iter().map(|c| pct(c.busy)).collect();
            println!(
                "cpu       {} busy, cores {}",
                pct(cpu.all.busy),
                cores.join(" ")
            );
            if self.sampler.reports_iowait() {
                println!("iowait    {}", pct(cpu.all.iowait));
            }
        }
        println!(
            "gpu       {}{}, {}",
            s.gpu.map_or_else(|| "-".to_string(), |g| pct(g.busy)),
            self.sampler
                .gpu_driver()
                .map_or_else(String::new, |d| format!(" ({d})")),
            watts(power.gpu_w)
        );
        if let Some(npu) = self.sampler.npu_name() {
            println!("npu       {}, {npu}", watts(power.npu_w));
        }
        println!(
            "cpu power {}, dram {}",
            watts(power.cpu_w),
            watts(power.dram_w)
        );
        let snapshot = self.governor.snapshot();
        println!(
            "thermal   {}{}",
            snapshot.pressure,
            self.observation
                .temperature_c
                .map_or_else(String::new, |t| format!(", {t:.1} C"))
        );
        if let Some(m) = s.memory {
            println!(
                "memory    {} / {}",
                format_bytes(m.used_bytes()),
                format_bytes(m.total_bytes)
            );
        }
        if let Some(d) = s.disk {
            println!(
                "disk      {} / {}",
                format_bytes(d.used_bytes()),
                format_bytes(d.total_bytes)
            );
        }
        if let Some([one, five, fifteen]) = s.load_average {
            println!("load      {one:.2} {five:.2} {fifteen:.2}");
        }
        if let Some(uptime) = s.uptime {
            println!("uptime    {}", format_uptime(uptime));
        }
    }

    async fn run(mut self) {
        #[cfg(target_os = "macos")]
        loadngo_host_desktop::make_desktop_widget();
        let mut next_sample_at = Instant::now();
        let mut painted_surface = (0.0, 0.0);
        loop {
            let frame = loadngo_host_desktop::capture_frame();
            let now = Instant::now();
            let mut dirty = false;
            if now >= next_sample_at {
                self.take_sample(now);
                next_sample_at = now + SAMPLE_EVERY;
                dirty = true;
            }
            let surface = (frame.surface.width, frame.surface.height);
            if surface != painted_surface {
                painted_surface = surface;
                dirty = true;
            }
            if dirty {
                self.scene.clear();
                self.paint(surface.0, surface.1);
                loadngo_host_desktop::clear(BACKGROUND);
                loadngo_host_desktop::render_widget_paint_ops(&self.scene);
            }
            let wait = next_sample_at.saturating_duration_since(Instant::now());
            loadngo_host_desktop::next_frame(FrameDemand::after(
                wait.max(Duration::from_millis(1)),
            ))
            .await;
        }
    }

    fn take_sample(&mut self, now: Instant) {
        self.sampler.sample(&mut self.sample);
        if now >= self.next_thermal_at {
            self.observation = self.thermal.observe();
            self.governor.observe(&self.observation, now);
            self.next_thermal_at = now + self.governor.snapshot().pressure.sample_interval();
        }
        if let Some(cpu) = &self.sample.cpu {
            self.busy_history.push(cpu.all.busy);
            self.iowait_history.push(cpu.all.iowait);
        }
        if let Some(gpu) = self.sample.gpu {
            self.gpu_history.push(gpu.busy);
        }
        if let Some(npu) = self.sample.power.and_then(|p| p.npu_w) {
            self.npu_history.push(npu);
        }
        if let Some(temp) = self.observation.temperature_c {
            self.temp_history.push(temp);
        }
    }

    fn paint(&mut self, width: f32, height: f32) {
        let scale = (width / WINDOW_WIDTH as f32)
            .min(height / self.layout.height())
            .max(0.25);
        let mut p = Painter {
            scene: &mut self.scene,
            scale,
        };
        let pad = 12.0;
        let inner = WINDOW_WIDTH as f32 - 2.0 * pad;
        let sample = &self.sample;

        // Header: host and uptime.
        p.text(pad, 8.0, inner, &self.host, 18, TEXT, HorizontalAlign::Left);
        if let Some(uptime) = sample.uptime {
            let up = format!("up {}", format_uptime(uptime));
            p.text(pad, 11.0, inner, &up, 12, MUTED, HorizontalAlign::Right);
        }

        // CPU: value, history graph, per-core bars.
        p.text(pad, 36.0, 60.0, "CPU", 13, MUTED, HorizontalAlign::Left);
        match &sample.cpu {
            Some(cpu) => {
                let reports_iowait = self.sampler.reports_iowait();
                let busy = format!("{:.0}%", cpu.all.busy * 100.0);
                p.text(
                    pad + 44.0,
                    34.0,
                    80.0,
                    &busy,
                    16,
                    ACCENT,
                    HorizontalAlign::Left,
                );
                if reports_iowait {
                    let iowait = format!("I/O wait {:.0}%", cpu.all.iowait * 100.0);
                    p.text(
                        pad,
                        37.0,
                        inner,
                        &iowait,
                        12,
                        IOWAIT,
                        HorizontalAlign::Right,
                    );
                }
            }
            None => p.text(
                pad + 44.0,
                34.0,
                80.0,
                "-",
                16,
                MUTED,
                HorizontalAlign::Left,
            ),
        }
        let cpu_graph = Rect {
            x: pad,
            y: 58.0,
            width: 222.0,
            height: 44.0,
        };
        p.fill(cpu_graph, GRAPH_BACKGROUND);
        p.stacked_bars(cpu_graph, &self.busy_history, Some(&self.iowait_history));
        if let Some(cpu) = &sample.cpu {
            let cores = Rect {
                x: pad + 230.0,
                y: 58.0,
                width: inner - 230.0,
                height: 44.0,
            };
            p.core_bars(cores, cpu.cores.iter().map(|c| (c.busy, c.iowait)));
        }

        // GPU: busy share and history, driver name.
        p.text(pad, 110.0, 60.0, "GPU", 13, MUTED, HorizontalAlign::Left);
        let gpu = sample
            .gpu
            .map_or_else(|| "-".to_string(), |g| format!("{:.0}%", g.busy * 100.0));
        p.text(
            pad + 44.0,
            108.0,
            80.0,
            &gpu,
            16,
            ACCENT,
            HorizontalAlign::Left,
        );
        let power = sample.power.unwrap_or_default();
        let gpu_note = match (power.gpu_w, self.sampler.gpu_driver()) {
            (Some(w), _) => Some(format!("{w:.1} W")),
            (None, Some(driver)) => Some(driver.to_string()),
            (None, None) => None,
        };
        if let Some(note) = gpu_note {
            p.text(pad, 111.0, inner, &note, 12, MUTED, HorizontalAlign::Right);
        }
        let gpu_graph = Rect {
            x: pad,
            y: 132.0,
            width: inner,
            height: 24.0,
        };
        p.fill(gpu_graph, GRAPH_BACKGROUND);
        p.stacked_bars(gpu_graph, &self.gpu_history, None);
        let mut y = 166.0;

        // Neural Engine: power (macOS publishes no utilisation for it), history scaled to
        // the highest draw seen or 2 W.
        if self.layout.npu {
            p.text(pad, y, 60.0, "NPU", 13, MUTED, HorizontalAlign::Left);
            let value = power
                .npu_w
                .map_or_else(|| "-".to_string(), |w| format!("{w:.2} W"));
            p.text(
                pad + 44.0,
                y - 2.0,
                90.0,
                &value,
                16,
                ACCENT,
                HorizontalAlign::Left,
            );
            if let Some(name) = self.sampler.npu_name() {
                p.text(pad, y + 1.0, inner, name, 12, MUTED, HorizontalAlign::Right);
            }
            let graph = Rect {
                x: pad,
                y: y + 22.0,
                width: inner,
                height: 24.0,
            };
            p.fill(graph, GRAPH_BACKGROUND);
            let full_scale = self.npu_history.max().max(2.0);
            p.scaled_bars(graph, &self.npu_history, full_scale);
            y += Layout::NPU;
        }

        let snapshot = self.governor.snapshot();
        let band_color = pressure_color(snapshot.pressure);
        let band = snapshot.pressure.to_string();
        if self.layout.temp_graph {
            // Temperature: value and band, clock, graph with the band entry points.
            p.text(pad, y, 60.0, "Temp", 13, MUTED, HorizontalAlign::Left);
            let temp = self
                .observation
                .temperature_c
                .map_or_else(|| "-".to_string(), |t| format!("{t:.1} °C"));
            p.text(
                pad + 44.0,
                y - 2.0,
                80.0,
                &temp,
                16,
                band_color,
                HorizontalAlign::Left,
            );
            p.text(
                pad + 118.0,
                y + 1.0,
                80.0,
                &band,
                13,
                band_color,
                HorizontalAlign::Left,
            );
            if let Some(clock) = sample.clock {
                let ghz = |hz: u64| hz as f32 / 1e9;
                let text = format!("{:.1}/{:.1} GHz", ghz(clock.current_hz), ghz(clock.max_hz));
                p.text(
                    pad,
                    y + 1.0,
                    inner,
                    &text,
                    12,
                    MUTED,
                    HorizontalAlign::Right,
                );
            }
            let temp_graph = Rect {
                x: pad,
                y: y + 22.0,
                width: inner,
                height: 40.0,
            };
            p.fill(temp_graph, GRAPH_BACKGROUND);
            if let Some(trips) = self.trips {
                for (entry, color) in [
                    (trips.fair_c, FAIR),
                    (trips.serious_c, SERIOUS),
                    (Some(trips.critical_c), CRITICAL),
                ] {
                    if let Some(entry) = entry.filter(|e| (GRAPH_MIN_C..=GRAPH_MAX_C).contains(e)) {
                        let line_y = graph_y(temp_graph, entry);
                        p.fill(
                            Rect {
                                x: temp_graph.x,
                                y: line_y,
                                width: temp_graph.width,
                                height: 1.0,
                            },
                            dim(color),
                        );
                    }
                }
            }
            p.line_graph(temp_graph, &self.temp_history, band_color);
            y += Layout::TEMP_GRAPH;
        } else {
            // No temperature to graph (macOS): the pressure band, and power where known.
            p.text(pad, y, 60.0, "Thermal", 13, MUTED, HorizontalAlign::Left);
            p.text(
                pad + 60.0,
                y,
                90.0,
                &band,
                13,
                band_color,
                HorizontalAlign::Left,
            );
            let mut parts = Vec::new();
            if let Some(w) = power.cpu_w {
                parts.push(format!("CPU {w:.1} W"));
            }
            if let Some(w) = power.dram_w {
                parts.push(format!("DRAM {w:.1} W"));
            }
            if !parts.is_empty() {
                p.text(
                    pad,
                    y + 1.0,
                    inner,
                    &parts.join("  "),
                    12,
                    MUTED,
                    HorizontalAlign::Right,
                );
            }
            y += Layout::ROW;
        }

        // Fan and supply voltage.
        if self.layout.supply_row {
            if let Some(fan) = sample.fan {
                let mut text = String::from("Fan");
                if let Some(duty) = fan.duty {
                    text.push_str(&format!(" {:.0}%", duty * 100.0));
                }
                if let Some(rpm) = fan.rpm {
                    text.push_str(&format!("  {rpm} rpm"));
                }
                p.text(pad, y, inner, &text, 13, TEXT, HorizontalAlign::Left);
            }
            match sample.undervoltage {
                Some(true) => p.text(
                    pad,
                    y,
                    inner,
                    "UNDER-VOLTAGE",
                    13,
                    CRITICAL,
                    HorizontalAlign::Right,
                ),
                Some(false) => p.text(
                    pad,
                    y,
                    inner,
                    "power ok",
                    13,
                    NOMINAL,
                    HorizontalAlign::Right,
                ),
                None => {}
            }
            y += Layout::ROW;
        }

        // Memory and disk.
        let rows = [
            (
                "Mem",
                sample.memory.map(|m| (m.used_bytes(), m.total_bytes)),
            ),
            ("Disk", sample.disk.map(|d| (d.used_bytes(), d.total_bytes))),
        ];
        for (label, usage) in rows {
            p.text(pad, y, 40.0, label, 13, MUTED, HorizontalAlign::Left);
            if let Some((used, total)) = usage {
                let share = if total == 0 {
                    0.0
                } else {
                    used as f32 / total as f32
                };
                let bar = Rect {
                    x: pad + 44.0,
                    y: y + 4.0,
                    width: 140.0,
                    height: 10.0,
                };
                p.fill(bar, GRAPH_BACKGROUND);
                let color = if share >= 0.9 { CRITICAL } else { ACCENT };
                p.fill(
                    Rect {
                        width: bar.width * share.clamp(0.0, 1.0),
                        ..bar
                    },
                    color,
                );
                let text = format!("{} / {}", format_bytes(used), format_bytes(total));
                p.text(pad, y, inner, &text, 12, TEXT, HorizontalAlign::Right);
            }
            y += 24.0;
        }

        // Load average.
        if let Some([one, five, fifteen]) = sample.load_average {
            p.text(pad, y, 40.0, "Load", 13, MUTED, HorizontalAlign::Left);
            let text = format!("{one:.2}  {five:.2}  {fifteen:.2}");
            p.text(
                pad + 44.0,
                y,
                inner - 44.0,
                &text,
                13,
                TEXT,
                HorizontalAlign::Left,
            );
        }
    }
}

/// Paint helpers in the 320x292 design space, scaled to the window.
struct Painter<'a> {
    scene: &'a mut Vec<PaintOp>,
    scale: f32,
}

impl Painter<'_> {
    fn rect(&self, r: Rect) -> Rect {
        Rect {
            x: r.x * self.scale,
            y: r.y * self.scale,
            width: r.width * self.scale,
            height: r.height * self.scale,
        }
    }

    fn fill(&mut self, r: Rect, color: Color) {
        let rect = self.rect(r);
        self.scene.push(PaintOp::FillRect { rect, color });
    }

    #[allow(clippy::too_many_arguments)]
    fn text(
        &mut self,
        x: f32,
        y: f32,
        width: f32,
        text: &str,
        size: u16,
        color: Color,
        align: HorizontalAlign,
    ) {
        let rect = self.rect(Rect {
            x,
            y,
            width,
            height: f32::from(size) * 1.5,
        });
        self.scene.push(PaintOp::Text {
            rect,
            clip_rect: Some(rect),
            text: text.to_string(),
            style: TextStyle {
                color,
                font_size: (f32::from(size) * self.scale).round().max(6.0) as u16,
                horizontal_align: align,
                vertical_align: VerticalAlign::Top,
                vertical_metric_mode: TextVerticalMetricMode::LogicalLineBox,
                layout_mode: TextLayoutMode::SingleLine,
                overflow: TextOverflow::Clip,
            },
        });
    }

    /// One column per sample: busy from the bottom, I/O wait (if any) stacked on top.
    fn stacked_bars(&mut self, area: Rect, busy: &History, iowait: Option<&History>) {
        let column = area.width / HISTORY as f32;
        let offset = HISTORY - busy.len;
        let mut waits = iowait.map(History::iter);
        for (i, b) in busy.iter().enumerate() {
            let w = waits.as_mut().and_then(Iterator::next).unwrap_or(0.0);
            let x = area.x + (offset + i) as f32 * column;
            let busy_h = area.height * b.clamp(0.0, 1.0);
            let wait_h = (area.height - busy_h) * w.clamp(0.0, 1.0);
            let bottom = area.y + area.height;
            self.fill(
                Rect {
                    x,
                    y: bottom - busy_h,
                    width: column,
                    height: busy_h,
                },
                ACCENT,
            );
            self.fill(
                Rect {
                    x,
                    y: bottom - busy_h - wait_h,
                    width: column,
                    height: wait_h,
                },
                IOWAIT,
            );
        }
    }

    fn core_bars(&mut self, area: Rect, cores: impl ExactSizeIterator<Item = (f32, f32)>) {
        let count = cores.len().max(1) as f32;
        let gap = 3.0;
        let width = ((area.width - gap * (count - 1.0)) / count).max(1.0);
        for (i, (busy, iowait)) in cores.enumerate() {
            let x = area.x + i as f32 * (width + gap);
            let column = Rect {
                x,
                y: area.y,
                width,
                height: area.height,
            };
            self.fill(column, GRAPH_BACKGROUND);
            let busy_h = area.height * busy.clamp(0.0, 1.0);
            let wait_h = (area.height - busy_h) * iowait.clamp(0.0, 1.0);
            let bottom = area.y + area.height;
            self.fill(
                Rect {
                    y: bottom - busy_h,
                    height: busy_h,
                    ..column
                },
                ACCENT,
            );
            self.fill(
                Rect {
                    y: bottom - busy_h - wait_h,
                    height: wait_h,
                    ..column
                },
                IOWAIT,
            );
        }
    }

    /// One column per sample, `value / full_scale` of the height.
    fn scaled_bars(&mut self, area: Rect, history: &History, full_scale: f32) {
        let column = area.width / HISTORY as f32;
        let offset = HISTORY - history.len;
        for (i, value) in history.iter().enumerate() {
            let h = area.height * (value / full_scale).clamp(0.0, 1.0);
            self.fill(
                Rect {
                    x: area.x + (offset + i) as f32 * column,
                    y: area.y + area.height - h,
                    width: column,
                    height: h,
                },
                ACCENT,
            );
        }
    }

    fn line_graph(&mut self, area: Rect, history: &History, color: Color) {
        if history.len < 2 {
            return;
        }
        let column = area.width / HISTORY as f32;
        let offset = HISTORY - history.len;
        let points = history
            .iter()
            .enumerate()
            .map(|(i, value)| {
                let x = area.x + (offset + i) as f32 * column + column / 2.0;
                ui_core::Point {
                    x: x * self.scale,
                    y: graph_y(area, value) * self.scale,
                }
            })
            .collect();
        self.scene.push(PaintOp::Polyline {
            points,
            color,
            thickness: (2.0 * self.scale).round().max(1.0) as i32,
            closed: false,
        });
    }
}

fn graph_y(area: Rect, temp_c: f32) -> f32 {
    let share = ((temp_c - GRAPH_MIN_C) / (GRAPH_MAX_C - GRAPH_MIN_C)).clamp(0.0, 1.0);
    area.y + area.height * (1.0 - share)
}

fn pressure_color(pressure: ThermalPressure) -> Color {
    match pressure {
        ThermalPressure::Unavailable => MUTED,
        ThermalPressure::Nominal => NOMINAL,
        ThermalPressure::Fair => FAIR,
        ThermalPressure::Serious => SERIOUS,
        ThermalPressure::Critical => CRITICAL,
    }
}

fn dim(color: Color) -> Color {
    Color::rgba(color.r, color.g, color.b, 0x70)
}

fn format_bytes(bytes: u64) -> String {
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    let gib = bytes as f64 / GIB;
    if gib >= 100.0 {
        format!("{gib:.0}G")
    } else if gib >= 1.0 {
        format!("{gib:.1}G")
    } else {
        format!("{:.0}M", bytes as f64 / (1024.0 * 1024.0))
    }
}

fn format_uptime(uptime: Duration) -> String {
    let minutes = uptime.as_secs() / 60;
    let (days, hours, minutes) = (minutes / 1440, minutes / 60 % 24, minutes % 60);
    match (days, hours) {
        (0, 0) => format!("{minutes}m"),
        (0, _) => format!("{hours}h {minutes}m"),
        _ => format!("{days}d {hours}h"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_keeps_the_newest_values_in_order() {
        let mut history = History::new();
        for value in 0..(HISTORY + 5) {
            history.push(value as f32);
        }
        let values: Vec<f32> = history.iter().collect();
        assert_eq!(values.len(), HISTORY);
        assert_eq!(values[0], 5.0);
        assert_eq!(values[HISTORY - 1], (HISTORY + 4) as f32);
    }

    #[test]
    fn sizes_and_uptime_read_naturally() {
        assert_eq!(format_bytes(3_886_832 * 1024), "3.7G");
        assert_eq!(format_bytes(235 * 1024 * 1024 * 1024), "235G");
        assert_eq!(format_bytes(468 * 1024 * 1024), "468M");
        assert_eq!(format_uptime(Duration::from_secs(119_536)), "1d 9h");
        assert_eq!(
            format_uptime(Duration::from_secs(3 * 3600 + 7 * 60)),
            "3h 7m"
        );
        assert_eq!(format_uptime(Duration::from_secs(59)), "0m");
    }

    #[test]
    fn graph_maps_the_range_and_clamps_outside_it() {
        let area = Rect {
            x: 0.0,
            y: 10.0,
            width: 100.0,
            height: 60.0,
        };
        assert_eq!(graph_y(area, GRAPH_MAX_C), 10.0);
        assert_eq!(graph_y(area, GRAPH_MIN_C), 70.0);
        assert_eq!(graph_y(area, 200.0), 10.0);
    }
}
