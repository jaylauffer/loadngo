//! System load sampling for monitors and work budgets: CPU busy and I/O-wait time per
//! core, GPU busy time, memory, disk, CPU clock, fan, and supply-voltage alarms.
//!
//! [`SystemSampler::sample`] fills a caller-owned [`SystemSample`] in place and reuses
//! one read buffer, so a monitor that samples every few seconds does not allocate in
//! steady state. Nothing here owns a thread or a timer: the caller decides when to
//! sample, normally from a proactor deadline.
//!
//! Utilisation is workload evidence, not a thermal signal; thermal pressure comes from
//! `loadngo-thermal`. Linux reads procfs and sysfs. Other platforms report every field
//! as `None` until they have a provider. `None` means unknown, never zero.

// The procfs parsers are only called on Linux but are unit-tested everywhere.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::path::Path;
use std::time::Duration;

/// Cumulative CPU time in kernel ticks, split the way a monitor shows it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CpuTimes {
    /// user + nice + system + irq + softirq + steal.
    pub busy: u64,
    /// Idle while a task waits on I/O (on a Pi, usually the SD card).
    pub iowait: u64,
    /// Every field above plus idle.
    pub total: u64,
}

impl CpuTimes {
    /// From the numeric fields of a `/proc/stat` `cpu` line:
    /// user nice system idle iowait irq softirq steal [guest guest_nice].
    /// Guest time is already counted in user, so it is not added again.
    fn from_proc_stat_fields<'a>(fields: impl Iterator<Item = &'a str>) -> Option<Self> {
        let mut values = [0u64; 8];
        let mut count = 0;
        for (slot, field) in values.iter_mut().zip(fields) {
            *slot = field.parse().ok()?;
            count += 1;
        }
        if count < 4 {
            return None;
        }
        let [user, nice, system, idle, iowait, irq, softirq, steal] = values;
        let busy = user + nice + system + irq + softirq + steal;
        Some(Self {
            busy,
            iowait,
            total: busy + idle + iowait,
        })
    }
}

/// Share of one interval, each in `0.0..=1.0`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CpuLoad {
    pub busy: f32,
    pub iowait: f32,
}

impl CpuLoad {
    /// Load between two cumulative readings of the same CPU. A counter that went
    /// backwards or did not advance reads as idle rather than as garbage.
    #[must_use]
    pub fn between(earlier: CpuTimes, later: CpuTimes) -> Self {
        let total = later.total.saturating_sub(earlier.total);
        if total == 0 {
            return Self::default();
        }
        let share = |a: u64, b: u64| (b.saturating_sub(a) as f64 / total as f64).min(1.0) as f32;
        Self {
            busy: share(earlier.busy, later.busy),
            iowait: share(earlier.iowait, later.iowait),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CpuUsage {
    /// All CPUs together.
    pub all: CpuLoad,
    /// One entry per CPU, in kernel order.
    pub cores: Vec<CpuLoad>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryUsage {
    pub total_bytes: u64,
    /// What the kernel estimates can be allocated without swapping.
    pub available_bytes: u64,
}

impl MemoryUsage {
    #[must_use]
    pub fn used_bytes(&self) -> u64 {
        self.total_bytes.saturating_sub(self.available_bytes)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskUsage {
    pub total_bytes: u64,
    /// Available to an unprivileged user (excludes the root reserve).
    pub available_bytes: u64,
}

impl DiskUsage {
    #[must_use]
    pub fn used_bytes(&self) -> u64 {
        self.total_bytes.saturating_sub(self.available_bytes)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuClock {
    /// Mean of the current frequency across cpufreq policies.
    pub current_hz: u64,
    /// Highest hardware maximum across policies.
    pub max_hz: u64,
}

/// Share of the last interval the GPU was busy, in `0.0..=1.0`: the largest share
/// any of its queues (binning, rendering, texture, compute) ran.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GpuUsage {
    pub busy: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fan {
    pub rpm: Option<u32>,
    /// PWM duty in `0.0..=1.0`.
    pub duty: Option<f32>,
}

/// One reading of everything the platform exposes. Reused across samples.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SystemSample {
    /// `None` on the first sample (usage needs two readings) and where unsupported.
    pub cpu: Option<CpuUsage>,
    /// `None` on the first sample and where the driver keeps no busy-time counters
    /// (today only Broadcom `v3d`, the Raspberry Pi 4 and 5 GPU, does).
    pub gpu: Option<GpuUsage>,
    pub memory: Option<MemoryUsage>,
    pub disk: Option<DiskUsage>,
    pub load_average: Option<[f32; 3]>,
    pub uptime: Option<Duration>,
    pub clock: Option<CpuClock>,
    pub fan: Option<Fan>,
    /// A supply-voltage sensor reports below its lower critical limit. On a
    /// Raspberry Pi this is the firmware's under-voltage flag.
    pub undervoltage: Option<bool>,
}

/// Samples the running system; see the crate docs.
#[derive(Debug)]
pub struct SystemSampler {
    buf: String,
    previous: Vec<CpuTimes>,
    current: Vec<CpuTimes>,
    gpu_previous: Option<GpuTimes>,
    #[cfg(target_os = "linux")]
    linux: linux::Paths,
}

impl SystemSampler {
    /// `disk_path` is any path on the filesystem whose usage to report, usually `/`.
    /// Sensor files are discovered once here, not on every sample.
    #[must_use]
    pub fn new(disk_path: &Path) -> Self {
        #[cfg(not(target_os = "linux"))]
        let _ = disk_path;
        Self {
            buf: String::with_capacity(4096),
            previous: Vec::new(),
            current: Vec::new(),
            gpu_previous: None,
            #[cfg(target_os = "linux")]
            linux: linux::Paths::discover(disk_path),
        }
    }

    /// Overwrites `out` with a fresh reading.
    pub fn sample(&mut self, out: &mut SystemSample) {
        #[cfg(target_os = "linux")]
        self.sample_linux(out);
        #[cfg(not(target_os = "linux"))]
        {
            *out = SystemSample::default();
            let _ = (&mut self.buf, &mut self.previous, &mut self.current);
            let _ = &mut self.gpu_previous;
        }
    }

    /// The GPU's kernel driver (for example `v3d`), found once at construction.
    #[must_use]
    pub fn gpu_driver(&self) -> Option<&str> {
        #[cfg(target_os = "linux")]
        {
            self.linux.gpu_driver.as_deref()
        }
        #[cfg(not(target_os = "linux"))]
        {
            None
        }
    }

    /// Stores `current` as usage against the previous GPU reading.
    fn update_gpu(&mut self, current: Option<GpuTimes>) -> Option<GpuUsage> {
        let previous = std::mem::replace(&mut self.gpu_previous, current);
        GpuTimes::busy_between(&previous?, &current?).map(|busy| GpuUsage { busy })
    }

    /// Stores the newly parsed `current` readings as usage against `previous`,
    /// then keeps `current` as the next baseline.
    fn update_cpu(&mut self, parsed: bool, out: &mut Option<CpuUsage>) {
        if !parsed || self.current.is_empty() {
            self.previous.clear();
            *out = None;
            return;
        }
        if self.previous.len() == self.current.len() {
            let usage = out.get_or_insert_with(CpuUsage::default);
            usage.all = CpuLoad::between(self.previous[0], self.current[0]);
            usage.cores.clear();
            usage.cores.extend(
                self.previous[1..]
                    .iter()
                    .zip(&self.current[1..])
                    .map(|(&a, &b)| CpuLoad::between(a, b)),
            );
        } else {
            // First reading, or CPUs came and went: no interval to measure yet.
            *out = None;
        }
        std::mem::swap(&mut self.previous, &mut self.current);
    }
}

/// Cumulative busy nanoseconds per GPU queue, as v3d's `gpu_stats` reports them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GpuTimes {
    timestamp_ns: u64,
    queues: usize,
    runtime_ns: [u64; 8],
}

impl GpuTimes {
    /// Parses v3d `gpu_stats`: a header, then `queue timestamp jobs runtime` rows.
    fn parse_v3d(text: &str) -> Option<Self> {
        let mut times = Self {
            timestamp_ns: 0,
            queues: 0,
            runtime_ns: [0; 8],
        };
        for line in text.lines().skip(1) {
            let mut fields = line.split_ascii_whitespace();
            let (_queue, timestamp, _jobs, runtime) = (
                fields.next()?,
                fields.next()?,
                fields.next()?,
                fields.next()?,
            );
            if times.queues == times.runtime_ns.len() {
                break;
            }
            times.timestamp_ns = timestamp.parse().ok()?;
            times.runtime_ns[times.queues] = runtime.parse().ok()?;
            times.queues += 1;
        }
        (times.queues > 0).then_some(times)
    }

    fn busy_between(earlier: &Self, later: &Self) -> Option<f32> {
        let elapsed = later.timestamp_ns.checked_sub(earlier.timestamp_ns)?;
        if elapsed == 0 || earlier.queues != later.queues {
            return None;
        }
        let busiest = (0..later.queues)
            .map(|q| later.runtime_ns[q].saturating_sub(earlier.runtime_ns[q]))
            .max()?;
        Some((busiest as f64 / elapsed as f64).min(1.0) as f32)
    }
}

/// This machine's name, for a monitor's title. Allocates; call once.
#[must_use]
pub fn hostname() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/sys/kernel/hostname")
            .ok()
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty())
    }
    #[cfg(not(target_os = "linux"))]
    {
        std::env::var("HOSTNAME")
            .ok()
            .filter(|name| !name.is_empty())
    }
}

/// Parses `/proc/stat` into `out`: the aggregate `cpu` line first, then `cpuN`.
fn parse_proc_stat(text: &str, out: &mut Vec<CpuTimes>) -> bool {
    out.clear();
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("cpu") else {
            if !out.is_empty() {
                break; // the cpu lines are contiguous and come first
            }
            continue;
        };
        let mut fields = rest.split_ascii_whitespace();
        let aggregate = rest.starts_with(' ');
        if !aggregate {
            fields.next(); // the CPU number
        }
        if aggregate != out.is_empty() {
            return false; // aggregate must come first, exactly once
        }
        match CpuTimes::from_proc_stat_fields(fields) {
            Some(times) => out.push(times),
            None => return false,
        }
    }
    !out.is_empty()
}

fn parse_meminfo(text: &str) -> Option<MemoryUsage> {
    let mut total = None;
    let mut available = None;
    for line in text.lines() {
        let slot = if line.starts_with("MemTotal:") {
            &mut total
        } else if line.starts_with("MemAvailable:") {
            &mut available
        } else {
            continue;
        };
        let kib: u64 = line.split_ascii_whitespace().nth(1)?.parse().ok()?;
        *slot = Some(kib * 1024);
        if total.is_some() && available.is_some() {
            break;
        }
    }
    Some(MemoryUsage {
        total_bytes: total?,
        available_bytes: available?,
    })
}

fn parse_loadavg(text: &str) -> Option<[f32; 3]> {
    let mut fields = text.split_ascii_whitespace();
    let mut next = || fields.next()?.parse().ok();
    Some([next()?, next()?, next()?])
}

fn parse_uptime(text: &str) -> Option<Duration> {
    let seconds: f64 = text.split_ascii_whitespace().next()?.parse().ok()?;
    (seconds.is_finite() && seconds >= 0.0).then(|| Duration::from_secs_f64(seconds))
}

fn parse_u64(text: &str) -> Option<u64> {
    text.trim().parse().ok()
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{
        parse_loadavg, parse_meminfo, parse_proc_stat, parse_u64, parse_uptime, CpuClock,
        DiskUsage, Fan, GpuTimes, Path, SystemSample, SystemSampler,
    };
    use std::ffi::CString;
    use std::io::Read;
    use std::path::PathBuf;

    #[derive(Debug)]
    pub(super) struct Paths {
        disk: Option<CString>,
        /// `(scaling_cur_freq, cpuinfo_max_freq)` per cpufreq policy.
        clocks: Vec<(PathBuf, PathBuf)>,
        fan_rpm: Option<PathBuf>,
        fan_pwm: Option<PathBuf>,
        voltage_alarms: Vec<PathBuf>,
        pub(super) gpu_driver: Option<String>,
        gpu_stats: Option<PathBuf>,
    }

    impl Paths {
        pub(super) fn discover(disk_path: &Path) -> Self {
            use std::os::unix::ffi::OsStrExt;
            let mut paths = Self {
                disk: CString::new(disk_path.as_os_str().as_bytes()).ok(),
                clocks: Vec::new(),
                fan_rpm: None,
                fan_pwm: None,
                voltage_alarms: Vec::new(),
                gpu_driver: None,
                gpu_stats: None,
            };
            // The first render node is the GPU that renders; display-only
            // devices (vc4 on a Pi) have a card node but no render node.
            if let Some(render) = sorted_entries(Path::new("/sys/class/drm"), "renderD")
                .into_iter()
                .next()
            {
                let device = render.join("device");
                paths.gpu_driver = std::fs::read_link(device.join("driver"))
                    .ok()
                    .and_then(|driver| Some(driver.file_name()?.to_string_lossy().into_owned()));
                let stats = device.join("gpu_stats");
                paths.gpu_stats = stats.exists().then_some(stats);
            }
            for policy in sorted_entries(Path::new("/sys/devices/system/cpu/cpufreq"), "policy") {
                let current = policy.join("scaling_cur_freq");
                let max = policy.join("cpuinfo_max_freq");
                if current.exists() && max.exists() {
                    paths.clocks.push((current, max));
                }
            }
            for hwmon in sorted_entries(Path::new("/sys/class/hwmon"), "hwmon") {
                if paths.fan_rpm.is_none() && hwmon.join("fan1_input").exists() {
                    paths.fan_rpm = Some(hwmon.join("fan1_input"));
                    let pwm = hwmon.join("pwm1");
                    paths.fan_pwm = pwm.exists().then_some(pwm);
                }
                for alarm in sorted_entries(&hwmon, "in") {
                    let name = alarm.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    if name.ends_with("_lcrit_alarm") {
                        paths.voltage_alarms.push(alarm);
                    }
                }
            }
            paths
        }
    }

    fn sorted_entries(dir: &Path, prefix: &str) -> Vec<PathBuf> {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(prefix))
            .map(|entry| entry.path())
            .collect();
        entries.sort();
        entries
    }

    impl SystemSampler {
        pub(super) fn sample_linux(&mut self, out: &mut SystemSample) {
            let parsed = read_into(&mut self.buf, Path::new("/proc/stat"))
                && parse_proc_stat(&self.buf, &mut self.current);
            self.update_cpu(parsed, &mut out.cpu);

            out.memory = read_into(&mut self.buf, Path::new("/proc/meminfo"))
                .then(|| parse_meminfo(&self.buf))
                .flatten();
            out.load_average = read_into(&mut self.buf, Path::new("/proc/loadavg"))
                .then(|| parse_loadavg(&self.buf))
                .flatten();
            out.uptime = read_into(&mut self.buf, Path::new("/proc/uptime"))
                .then(|| parse_uptime(&self.buf))
                .flatten();
            let gpu = match self.linux.gpu_stats.as_deref() {
                Some(path) if read_into(&mut self.buf, path) => GpuTimes::parse_v3d(&self.buf),
                _ => None,
            };
            out.gpu = self.update_gpu(gpu);
            out.disk = self.linux.disk.as_deref().and_then(disk_usage);
            out.clock = self.read_clock();
            out.fan = self.read_fan();
            out.undervoltage = self.read_undervoltage();
        }

        fn read_clock(&mut self) -> Option<CpuClock> {
            let (buf, clocks) = (&mut self.buf, &self.linux.clocks);
            let mut current_khz = 0;
            let mut max_khz = 0;
            for (current, max) in clocks {
                current_khz += read_u64(buf, current)?;
                max_khz = max_khz.max(read_u64(buf, max)?);
            }
            let policies = clocks.len() as u64;
            (policies > 0).then(|| CpuClock {
                current_hz: current_khz / policies * 1000,
                max_hz: max_khz * 1000,
            })
        }

        fn read_fan(&mut self) -> Option<Fan> {
            let (buf, paths) = (&mut self.buf, &self.linux);
            let rpm = read_u64(buf, paths.fan_rpm.as_deref()?)
                .map(|rpm| rpm.min(u64::from(u32::MAX)) as u32);
            let duty = paths
                .fan_pwm
                .as_deref()
                .and_then(|pwm| read_u64(buf, pwm))
                .map(|value| value.min(255) as f32 / 255.0);
            Some(Fan { rpm, duty })
        }

        fn read_undervoltage(&mut self) -> Option<bool> {
            let (buf, alarms) = (&mut self.buf, &self.linux.voltage_alarms);
            let mut seen = false;
            let mut alarm = false;
            for path in alarms {
                if let Some(value) = read_u64(buf, path) {
                    seen = true;
                    alarm |= value != 0;
                }
            }
            seen.then_some(alarm)
        }
    }

    fn read_u64(buf: &mut String, path: &Path) -> Option<u64> {
        read_into(buf, path).then(|| parse_u64(buf)).flatten()
    }

    /// Reads a whole procfs/sysfs file into `buf`, reusing its capacity.
    fn read_into(buf: &mut String, path: &Path) -> bool {
        buf.clear();
        std::fs::File::open(path)
            .and_then(|mut file| file.read_to_string(buf))
            .is_ok()
    }

    fn disk_usage(path: &std::ffi::CStr) -> Option<DiskUsage> {
        let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        // SAFETY: `path` is NUL-terminated and `stats` is a valid out-pointer that
        // statvfs fully initialises when it returns 0.
        let stats = unsafe {
            if libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) != 0 {
                return None;
            }
            stats.assume_init()
        };
        let fragment = stats.f_frsize as u64;
        Some(DiskUsage {
            total_bytes: stats.f_blocks as u64 * fragment,
            available_bytes: stats.f_bavail as u64 * fragment,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Recorded on agnes (Pi 4) on 2026-09-25, trimmed to three CPUs.
    const PROC_STAT: &str = "\
cpu  332766 28374 85059 35377950 11881784 0 2131 0 0 0
cpu0 82397 3618 23153 10439837 1371702 0 1317 0 0 0
cpu1 86929 5735 22198 9547698 2266261 0 356 0 0 0
intr 123 4 5
ctxt 999
";

    #[test]
    fn proc_stat_splits_busy_iowait_and_idle() {
        let mut times = Vec::new();
        assert!(parse_proc_stat(PROC_STAT, &mut times));
        assert_eq!(times.len(), 3);
        assert_eq!(
            times[0],
            CpuTimes {
                busy: 332766 + 28374 + 85059 + 2131,
                iowait: 11881784,
                total: 332766 + 28374 + 85059 + 2131 + 35377950 + 11881784,
            }
        );
        assert_eq!(times[2].iowait, 2266261);
    }

    #[test]
    fn proc_stat_rejects_a_missing_aggregate_or_bad_numbers() {
        let mut times = Vec::new();
        assert!(!parse_proc_stat("cpu0 1 2 3 4\n", &mut times));
        assert!(!parse_proc_stat("cpu  1 2 x 4\n", &mut times));
        assert!(!parse_proc_stat("intr 1\n", &mut times));
    }

    #[test]
    fn load_is_the_share_of_the_interval() {
        let earlier = CpuTimes {
            busy: 100,
            iowait: 50,
            total: 1000,
        };
        let later = CpuTimes {
            busy: 150,
            iowait: 75,
            total: 1100,
        };
        let load = CpuLoad::between(earlier, later);
        assert!((load.busy - 0.5).abs() < 1e-6);
        assert!((load.iowait - 0.25).abs() < 1e-6);
        assert_eq!(CpuLoad::between(later, earlier), CpuLoad::default());
    }

    #[test]
    fn first_sample_has_no_usage_and_the_second_does() {
        let mut sampler = SystemSampler::new(Path::new("/"));
        let mut usage = None;
        assert!(parse_proc_stat(PROC_STAT, &mut sampler.current));
        sampler.update_cpu(true, &mut usage);
        assert_eq!(usage, None);

        let later = PROC_STAT
            .replace("cpu  332766", "cpu  332866")
            .replace("35377950", "35378050");
        assert!(parse_proc_stat(&later, &mut sampler.current));
        sampler.update_cpu(true, &mut usage);
        let usage = usage.expect("second sample measures an interval");
        assert!((usage.all.busy - 0.5).abs() < 1e-6);
        assert_eq!(usage.cores.len(), 2);
        assert_eq!(usage.cores[0], CpuLoad::default());
    }

    #[test]
    fn meminfo_loadavg_and_uptime_parse() {
        let meminfo = "MemTotal:        3886832 kB\nMemFree:  1 kB\nMemAvailable:    3383036 kB\n";
        let memory = parse_meminfo(meminfo).unwrap();
        assert_eq!(memory.total_bytes, 3886832 * 1024);
        assert_eq!(memory.used_bytes(), (3886832 - 3383036) * 1024);
        assert_eq!(parse_meminfo("MemTotal: 1 kB\n"), None);

        assert_eq!(
            parse_loadavg("0.06 0.23 0.59 1/396 36926\n"),
            Some([0.06, 0.23, 0.59])
        );
        assert_eq!(
            parse_uptime("119536.99 353779.51\n"),
            Some(Duration::from_secs_f64(119536.99))
        );
        assert_eq!(parse_uptime("-1 0"), None);
    }

    // Recorded on dolores (Pi 5) on 2026-09-25.
    const V3D_GPU_STATS: &str = "\
queue\ttimestamp\tjobs\truntime
bin\t661782989550184\t1971412\t141142694696
render\t661782989550184\t1971412\t1425315226538
tfu\t661782989550184\t238197\t265510641434
csd\t661782989550184\t0\t0
cache_clean\t661782989550184\t0\t0
cpu\t661782989550184\t0\t0
";

    #[test]
    fn gpu_busy_is_the_busiest_queue_share() {
        let earlier = GpuTimes::parse_v3d(V3D_GPU_STATS).unwrap();
        assert_eq!(earlier.queues, 6);
        let mut later = earlier;
        later.timestamp_ns += 2_000_000_000;
        later.runtime_ns[0] += 100_000_000; // bin 5%
        later.runtime_ns[1] += 500_000_000; // render 25%
        let busy = GpuTimes::busy_between(&earlier, &later).unwrap();
        assert!((busy - 0.25).abs() < 1e-6);
        assert_eq!(GpuTimes::busy_between(&later, &earlier), None);
        assert_eq!(
            GpuTimes::parse_v3d("queue\ttimestamp\tjobs\truntime\n"),
            None
        );
    }

    #[test]
    fn first_gpu_reading_has_no_usage() {
        let mut sampler = SystemSampler::new(Path::new("/"));
        let reading = GpuTimes::parse_v3d(V3D_GPU_STATS);
        assert_eq!(sampler.update_gpu(reading), None);
        assert_eq!(sampler.update_gpu(reading), None); // no time passed
        assert_eq!(sampler.update_gpu(None), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_sample_reads_the_running_system() {
        let mut sampler = SystemSampler::new(Path::new("/"));
        let mut sample = SystemSample::default();
        sampler.sample(&mut sample);
        sampler.sample(&mut sample);
        assert!(sample.cpu.is_some());
        assert!(sample.memory.unwrap().total_bytes > 0);
        assert!(sample.disk.unwrap().total_bytes > 0);
        assert!(sample.uptime.is_some());
    }
}
