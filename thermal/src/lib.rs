//! Portable thermal awareness: the model, governor and providers specified in
//! `docs/THERMAL_AWARENESS.md` (implementation sequence steps 1 and 4: macOS/iOS native
//! pressure and the Linux sysfs thermal zone).
//!
//! A provider reports what the operating system says; the [`ThermalGovernor`] turns
//! those observations into published [`ThermalSnapshot`]s with hysteresis; consumers
//! act on the snapshot's pressure and recommendation at their own safe checkpoints.
//! Nothing here owns a thread, a timer or a polling loop: a consumer samples when it
//! reaches a checkpoint, or schedules its own proactor deadline if it must wait.
//!
//! `Unavailable` is never reported as `Nominal`: without a provider, the consumer keeps
//! its normal bounded policy and must not claim thermal behaviour was verified.
#![forbid(unsafe_code)]

use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Portable pressure. No derived ordering: `Unavailable` is not a severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThermalPressure {
    Unavailable,
    Nominal,
    Fair,
    Serious,
    Critical,
}

impl ThermalPressure {
    /// Severity for an available pressure; `None` for `Unavailable`.
    #[must_use]
    pub const fn severity(self) -> Option<u8> {
        match self {
            Self::Unavailable => None,
            Self::Nominal => Some(0),
            Self::Fair => Some(1),
            Self::Serious => Some(2),
            Self::Critical => Some(3),
        }
    }

    const fn from_severity(level: u8) -> Self {
        match level {
            0 => Self::Nominal,
            1 => Self::Fair,
            2 => Self::Serious,
            _ => Self::Critical,
        }
    }

    /// How often a polling provider may be sampled at this pressure
    /// (`THERMAL_AWARENESS.md`, "Proactor And Event-Loop Integration").
    #[must_use]
    pub const fn sample_interval(self) -> Duration {
        match self {
            Self::Unavailable | Self::Nominal => Duration::from_secs(5),
            Self::Fair | Self::Serious => Duration::from_secs(2),
            Self::Critical => Duration::from_secs(1),
        }
    }

    /// True for `Serious` and `Critical`.
    #[must_use]
    pub fn at_least_serious(self) -> bool {
        self.severity().is_some_and(|s| s >= 2)
    }
}

impl fmt::Display for ThermalPressure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Unavailable => "unavailable",
            Self::Nominal => "nominal",
            Self::Fair => "fair",
            Self::Serious => "serious",
            Self::Critical => "critical",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThermalSource {
    NativePressure,
    ThermalZone,
    External,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThermalRecommendation {
    Baseline,
    ReduceOptional,
    MinimizeOptional,
    PauseOptional,
}

impl ThermalRecommendation {
    /// The mapping table in `THERMAL_AWARENESS.md`.
    #[must_use]
    pub const fn for_pressure(pressure: ThermalPressure) -> Self {
        match pressure {
            ThermalPressure::Unavailable | ThermalPressure::Nominal => Self::Baseline,
            ThermalPressure::Fair => Self::ReduceOptional,
            ThermalPressure::Serious => Self::MinimizeOptional,
            ThermalPressure::Critical => Self::PauseOptional,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ThermalObservation {
    pub pressure: ThermalPressure,
    pub temperature_c: Option<f32>,
    pub throttling: Option<bool>,
    pub source: ThermalSource,
}

impl ThermalObservation {
    #[must_use]
    pub const fn unavailable() -> Self {
        Self {
            pressure: ThermalPressure::Unavailable,
            temperature_c: None,
            throttling: None,
            source: ThermalSource::Unavailable,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ThermalSnapshot {
    pub pressure: ThermalPressure,
    pub recommendation: ThermalRecommendation,
    pub temperature_c: Option<f32>,
    pub throttling: Option<bool>,
    /// Changes only when the published pressure changes.
    pub sequence: u64,
}

/// Recovery hysteresis; escalation is always immediate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GovernorConfig {
    /// Consecutive lower observations required before lowering a band.
    pub recovery_samples: u32,
    /// Minimum time in the current band before lowering it.
    pub recovery_dwell: Duration,
}

impl Default for GovernorConfig {
    fn default() -> Self {
        Self {
            recovery_samples: 3,
            recovery_dwell: Duration::from_secs(15),
        }
    }
}

/// Deterministic state machine from observations to snapshots. Time is passed in,
/// so tests drive it with a fake clock.
#[derive(Debug, Clone)]
pub struct ThermalGovernor {
    config: GovernorConfig,
    snapshot: ThermalSnapshot,
    since: Option<Instant>,
    lower_streak: u32,
    streak_peak: u8,
}

impl ThermalGovernor {
    #[must_use]
    pub fn new(config: GovernorConfig) -> Self {
        Self {
            config,
            snapshot: ThermalSnapshot {
                pressure: ThermalPressure::Unavailable,
                recommendation: ThermalRecommendation::Baseline,
                temperature_c: None,
                throttling: None,
                sequence: 0,
            },
            since: None,
            lower_streak: 0,
            streak_peak: 0,
        }
    }

    #[must_use]
    pub fn snapshot(&self) -> &ThermalSnapshot {
        &self.snapshot
    }

    fn publish(&mut self, pressure: ThermalPressure, now: Instant) {
        if pressure != self.snapshot.pressure {
            self.snapshot.pressure = pressure;
            self.snapshot.recommendation = ThermalRecommendation::for_pressure(pressure);
            self.snapshot.sequence += 1;
            self.since = Some(now);
        }
        self.lower_streak = 0;
        self.streak_peak = 0;
    }

    /// Feeds one observation; returns the new snapshot when the published pressure
    /// changed, `None` when it did not.
    pub fn observe(
        &mut self,
        observation: &ThermalObservation,
        now: Instant,
    ) -> Option<ThermalSnapshot> {
        let before = self.snapshot.sequence;
        self.snapshot.temperature_c = observation.temperature_c;
        self.snapshot.throttling = observation.throttling;
        // An explicit throttling signal is at least Serious.
        let observed = match (observation.pressure.severity(), observation.throttling) {
            (None, _) => None,
            (Some(level), Some(true)) => Some(level.max(2)),
            (Some(level), _) => Some(level),
        };
        match (observed, self.snapshot.pressure.severity()) {
            (None, _) => self.publish(ThermalPressure::Unavailable, now),
            (Some(new), None) => self.publish(ThermalPressure::from_severity(new), now),
            (Some(new), Some(current)) if new > current => {
                self.publish(ThermalPressure::from_severity(new), now);
            }
            (Some(new), Some(current)) if new == current => {
                self.lower_streak = 0;
                self.streak_peak = 0;
            }
            (Some(new), Some(_)) => {
                self.lower_streak += 1;
                self.streak_peak = self.streak_peak.max(new);
                let dwelt = self
                    .since
                    .is_none_or(|since| now.duration_since(since) >= self.config.recovery_dwell);
                if self.lower_streak >= self.config.recovery_samples && dwelt {
                    // Lower to the highest level seen during the streak, not the last.
                    self.publish(ThermalPressure::from_severity(self.streak_peak), now);
                }
            }
        }
        (self.snapshot.sequence != before).then(|| self.snapshot.clone())
    }
}

/// A source of observations. Sampling must be cheap and non-blocking.
pub trait ThermalProvider {
    fn observe(&mut self) -> ThermalObservation;
}

/// For platforms without a proven provider (Windows, NetBSD), and Linux machines with
/// no usable CPU thermal zone: always `Unavailable`.
#[derive(Debug, Default)]
pub struct UnavailableProvider;

impl ThermalProvider for UnavailableProvider {
    fn observe(&mut self) -> ThermalObservation {
        ThermalObservation::unavailable()
    }
}

/// Scripted observations for tests; repeats the last one.
#[derive(Debug, Default)]
pub struct FakeProvider {
    script: Vec<ThermalPressure>,
    at: usize,
}

impl FakeProvider {
    #[must_use]
    pub fn new(script: Vec<ThermalPressure>) -> Self {
        Self { script, at: 0 }
    }
}

impl ThermalProvider for FakeProvider {
    fn observe(&mut self) -> ThermalObservation {
        let pressure = self
            .script
            .get(self.at)
            .or(self.script.last())
            .copied()
            .unwrap_or(ThermalPressure::Unavailable);
        self.at += 1;
        ThermalObservation {
            pressure,
            temperature_c: None,
            throttling: None,
            source: ThermalSource::External,
        }
    }
}

/// macOS/iOS: the process's native thermal state (`NSProcessInfo.thermalState`),
/// available to an unprivileged process. No temperature is exposed or inferred.
#[cfg(any(target_os = "macos", target_os = "ios"))]
#[derive(Debug, Default)]
pub struct NativeProvider;

#[cfg(any(target_os = "macos", target_os = "ios"))]
impl ThermalProvider for NativeProvider {
    fn observe(&mut self) -> ThermalObservation {
        use objc2_foundation::{NSProcessInfo, NSProcessInfoThermalState as S};
        let pressure = match NSProcessInfo::processInfo().thermalState() {
            S::Nominal => ThermalPressure::Nominal,
            S::Fair => ThermalPressure::Fair,
            S::Serious => ThermalPressure::Serious,
            S::Critical => ThermalPressure::Critical,
            _ => ThermalPressure::Unavailable,
        };
        ThermalObservation {
            pressure,
            temperature_c: None,
            throttling: None,
            source: if pressure == ThermalPressure::Unavailable {
                ThermalSource::Unavailable
            } else {
                ThermalSource::NativePressure
            },
        }
    }
}

/// Temperatures at which a temperature-derived provider enters each pressure band.
///
/// These are platform facts (kernel trip points, or a board's documented firmware
/// limits), never a universal engine threshold. Fields are public so a deployment
/// with better evidence can override them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TripPoints {
    pub fair_c: Option<f32>,
    /// Where the platform starts throttling.
    pub serious_c: Option<f32>,
    pub critical_c: f32,
    /// A band is left only once the temperature falls this far below its entry point.
    pub recovery_margin_c: f32,
}

impl TripPoints {
    /// Raspberry Pi 4 and 5. The firmware, not the kernel, throttles these boards: it
    /// caps the ARM clock from 80 C (soft limit) and throttles fully at 85 C, and
    /// the kernel's only trip point is a 110 C shutdown. `Fair` starts 10 C before
    /// the soft limit.
    pub const RASPBERRY_PI_4_5: Self = Self {
        fair_c: Some(70.0),
        serious_c: Some(80.0),
        critical_c: 85.0,
        recovery_margin_c: 3.0,
    };

    /// Default recovery margin when the kernel gives no hysteresis.
    const DEFAULT_MARGIN_C: f32 = 3.0;
    /// `Fair` begins this far below the throttling trip.
    const FAIR_LEAD_C: f32 = 10.0;

    /// From a zone's kernel trip points, as `(type, temperature C, hysteresis C)`.
    /// The lowest `passive` trip (where the kernel throttles) enters `Serious`; the
    /// lowest `hot` or `critical` trip enters `Critical`. `None` without a
    /// `hot`/`critical` trip.
    #[must_use]
    pub fn from_kernel_trips(trips: &[(&str, f32, f32)]) -> Option<Self> {
        let lowest = |kinds: &[&str]| {
            trips
                .iter()
                .filter(|(kind, _, _)| kinds.contains(kind))
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .copied()
        };
        let critical_c = lowest(&["hot", "critical"])?.1;
        let passive = lowest(&["passive"]).filter(|(_, temp, _)| *temp < critical_c);
        let serious_c = passive.map(|(_, temp, _)| temp);
        let recovery_margin_c = passive
            .map(|(_, _, hyst)| hyst)
            .filter(|hyst| *hyst > 0.0)
            .unwrap_or(Self::DEFAULT_MARGIN_C);
        Some(Self {
            fair_c: serious_c.map(|serious| serious - Self::FAIR_LEAD_C),
            serious_c,
            critical_c,
            recovery_margin_c,
        })
    }

    fn entry_c(&self, severity: u8) -> Option<f32> {
        match severity {
            1 => self.fair_c,
            2 => self.serious_c,
            3 => Some(self.critical_c),
            _ => None,
        }
    }

    /// The band for `temp_c`, holding `previous` (or any band between) until the
    /// temperature has fallen `recovery_margin_c` below that band's entry point.
    #[must_use]
    pub fn pressure(&self, temp_c: f32, previous: ThermalPressure) -> ThermalPressure {
        let raw = (1..=3u8)
            .rev()
            .find(|&level| self.entry_c(level).is_some_and(|entry| temp_c >= entry))
            .unwrap_or(0);
        let held = previous.severity().map_or(raw, |previous| {
            (raw + 1..=previous)
                .rev()
                .find(|&level| {
                    self.entry_c(level)
                        .is_some_and(|entry| temp_c > entry - self.recovery_margin_c)
                })
                .unwrap_or(raw)
        });
        ThermalPressure::from_severity(held)
    }
}

/// Linux: the kernel's CPU thermal zone under `/sys/class/thermal`, chosen by type
/// rather than index. It reports temperature and a band from [`TripPoints`]. It
/// cannot see firmware throttling (on a Raspberry Pi that needs the root-only
/// mailbox), so `throttling` stays `None`.
#[derive(Debug)]
pub struct ThermalZoneProvider {
    temp_path: PathBuf,
    zone_type: String,
    trips: TripPoints,
    last: ThermalPressure,
    buf: String,
}

impl ThermalZoneProvider {
    /// Zone types that measure the CPU or SoC, most specific first.
    const PREFERRED_TYPES: [&'static str; 8] = [
        "cpu-thermal",
        "cpu_thermal",
        "x86_pkg_temp",
        "cpu0-thermal",
        "cpu0_thermal",
        "soc-thermal",
        "soc_thermal",
        "soc",
    ];

    /// The running system's CPU zone, if the kernel exposes one.
    #[must_use]
    pub fn discover() -> Option<Self> {
        Self::discover_in(
            Path::new("/sys/class/thermal"),
            Path::new("/proc/device-tree/compatible"),
        )
    }

    fn discover_in(thermal_root: &Path, device_tree_compatible: &Path) -> Option<Self> {
        let mut zones: Vec<(PathBuf, String)> = std::fs::read_dir(thermal_root)
            .ok()?
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("thermal_zone")
            })
            .filter_map(|entry| {
                let kind = std::fs::read_to_string(entry.path().join("type")).ok()?;
                Some((entry.path(), kind.trim().to_string()))
            })
            .collect();
        zones.sort();
        let (zone, zone_type) = Self::PREFERRED_TYPES
            .iter()
            .find_map(|preferred| zones.iter().find(|(_, kind)| kind == preferred))
            .or_else(|| {
                zones
                    .iter()
                    .find(|(_, kind)| ["cpu", "pkg", "soc"].iter().any(|part| kind.contains(part)))
            })?
            .clone();
        let compatible = std::fs::read_to_string(device_tree_compatible).unwrap_or_default();
        let trips = if is_raspberry_pi_4_or_5(&compatible) {
            TripPoints::RASPBERRY_PI_4_5
        } else {
            TripPoints::from_kernel_trips(
                &read_kernel_trips(&zone)
                    .iter()
                    .map(|(kind, temp, hyst)| (kind.as_str(), *temp, *hyst))
                    .collect::<Vec<_>>(),
            )?
        };
        Some(Self {
            temp_path: zone.join("temp"),
            zone_type,
            trips,
            last: ThermalPressure::Unavailable,
            buf: String::with_capacity(16),
        })
    }

    #[must_use]
    pub fn zone_type(&self) -> &str {
        &self.zone_type
    }

    #[must_use]
    pub fn trips(&self) -> TripPoints {
        self.trips
    }
}

impl ThermalProvider for ThermalZoneProvider {
    fn observe(&mut self) -> ThermalObservation {
        self.buf.clear();
        let millidegrees = std::fs::File::open(&self.temp_path)
            .and_then(|mut file| file.read_to_string(&mut self.buf))
            .ok()
            .and_then(|_| self.buf.trim().parse::<i64>().ok());
        // Outside -50..200 C is a broken sensor, not a temperature.
        let Some(temp_c) = millidegrees
            .map(|m| m as f32 / 1000.0)
            .filter(|t| (-50.0..200.0).contains(t))
        else {
            self.last = ThermalPressure::Unavailable;
            return ThermalObservation::unavailable();
        };
        self.last = self.trips.pressure(temp_c, self.last);
        ThermalObservation {
            pressure: self.last,
            temperature_c: Some(temp_c),
            throttling: None,
            source: ThermalSource::ThermalZone,
        }
    }
}

/// `(type, temperature C, hysteresis C)` for each `trip_point_N` of a zone.
fn read_kernel_trips(zone: &Path) -> Vec<(String, f32, f32)> {
    let read = |name: String| std::fs::read_to_string(zone.join(name)).ok();
    let millis = |text: Option<String>| text?.trim().parse::<i64>().ok().map(|m| m as f32 / 1000.0);
    (0..)
        .map_while(|index| {
            let kind = read(format!("trip_point_{index}_type"))?;
            let temp = millis(read(format!("trip_point_{index}_temp")));
            let hyst = millis(read(format!("trip_point_{index}_hyst"))).unwrap_or(0.0);
            Some((kind.trim().to_string(), temp, hyst))
        })
        .filter_map(|(kind, temp, hyst)| Some((kind, temp?, hyst)))
        .collect()
}

/// Whether a device tree `compatible` list (NUL-separated) names a Pi 4 or 5
/// family board (including the 400/500 and compute modules).
fn is_raspberry_pi_4_or_5(compatible: &str) -> bool {
    compatible
        .split('\0')
        .any(|entry| entry.starts_with("raspberrypi,4") || entry.starts_with("raspberrypi,5"))
}

/// The best provider this platform has.
#[must_use]
pub fn platform_provider() -> Box<dyn ThermalProvider> {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        Box::new(NativeProvider)
    }
    #[cfg(target_os = "linux")]
    {
        match ThermalZoneProvider::discover() {
            Some(provider) => Box::new(provider),
            None => Box::new(UnavailableProvider),
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "linux")))]
    {
        Box::new(UnavailableProvider)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ThermalPressure::{Critical, Fair, Nominal, Serious, Unavailable};

    fn obs(pressure: ThermalPressure) -> ThermalObservation {
        ThermalObservation {
            pressure,
            temperature_c: None,
            throttling: None,
            source: ThermalSource::External,
        }
    }

    #[test]
    fn recommendations_follow_the_documented_table() {
        assert_eq!(
            ThermalRecommendation::for_pressure(Unavailable),
            ThermalRecommendation::Baseline
        );
        assert_eq!(
            ThermalRecommendation::for_pressure(Nominal),
            ThermalRecommendation::Baseline
        );
        assert_eq!(
            ThermalRecommendation::for_pressure(Fair),
            ThermalRecommendation::ReduceOptional
        );
        assert_eq!(
            ThermalRecommendation::for_pressure(Serious),
            ThermalRecommendation::MinimizeOptional
        );
        assert_eq!(
            ThermalRecommendation::for_pressure(Critical),
            ThermalRecommendation::PauseOptional
        );
    }

    #[test]
    fn escalation_is_immediate_and_recovery_needs_samples_and_dwell() {
        let t0 = Instant::now();
        let at = |s| t0 + Duration::from_secs(s);
        let mut g = ThermalGovernor::new(GovernorConfig::default());
        assert_eq!(g.observe(&obs(Nominal), at(0)).unwrap().pressure, Nominal);
        assert_eq!(g.observe(&obs(Critical), at(1)).unwrap().pressure, Critical);
        // Three lower samples inside the 15 s dwell: no recovery yet.
        for s in 2..5 {
            assert!(g.observe(&obs(Nominal), at(s)).is_none());
        }
        // Fourth lower sample, 19 s after escalating: both rules met. The band drops to
        // the highest level seen during the streak (Fair), not the latest.
        assert_eq!(g.observe(&obs(Fair), at(20)).unwrap().pressure, Fair);
        // A sample back at the current level resets the streak.
        assert!(g.observe(&obs(Fair), at(40)).is_none());
        assert!(g.observe(&obs(Nominal), at(41)).is_none());
        assert!(g.observe(&obs(Nominal), at(42)).is_none());
        assert_eq!(g.observe(&obs(Nominal), at(43)).unwrap().pressure, Nominal);
    }

    #[test]
    fn duplicates_do_not_advance_sequence_and_unavailable_is_never_nominal() {
        let t = Instant::now();
        let mut g = ThermalGovernor::new(GovernorConfig::default());
        g.observe(&obs(Nominal), t);
        let seq = g.snapshot().sequence;
        assert!(g.observe(&obs(Nominal), t).is_none());
        assert_eq!(g.snapshot().sequence, seq);
        assert_eq!(
            g.observe(&obs(Unavailable), t).unwrap().pressure,
            Unavailable
        );
        let mut throttled = obs(Nominal);
        throttled.throttling = Some(true);
        assert_eq!(g.observe(&throttled, t).unwrap().pressure, Serious);
    }

    #[test]
    fn fake_provider_drives_the_governor_without_any_waiting() {
        let mut provider = FakeProvider::new(vec![Nominal, Serious, Serious]);
        let mut g = ThermalGovernor::new(GovernorConfig::default());
        let t = Instant::now();
        let seen: Vec<_> = (0..4)
            .filter_map(|_| g.observe(&provider.observe(), t))
            .map(|s| s.pressure)
            .collect();
        assert_eq!(seen, [Nominal, Serious]);
        assert_eq!(UnavailableProvider.observe().pressure, Unavailable);
    }

    #[test]
    fn sample_interval_follows_the_documented_cadence() {
        assert_eq!(Nominal.sample_interval(), Duration::from_secs(5));
        assert_eq!(Unavailable.sample_interval(), Duration::from_secs(5));
        assert_eq!(Serious.sample_interval(), Duration::from_secs(2));
        assert_eq!(Critical.sample_interval(), Duration::from_secs(1));
    }

    #[test]
    fn trip_points_enter_immediately_and_leave_below_the_margin() {
        let pi = TripPoints::RASPBERRY_PI_4_5;
        assert_eq!(pi.pressure(60.8, Unavailable), Nominal);
        assert_eq!(pi.pressure(70.0, Nominal), Fair);
        assert_eq!(pi.pressure(86.0, Nominal), Critical);
        // Falling from Critical: held while above 85 - 3, then Serious down to 77.
        assert_eq!(pi.pressure(83.0, Critical), Critical);
        assert_eq!(pi.pressure(79.0, Critical), Serious);
        assert_eq!(pi.pressure(77.0, Serious), Fair);
        assert_eq!(pi.pressure(66.0, Fair), Nominal);
    }

    #[test]
    fn kernel_trips_map_passive_to_serious_and_hot_or_critical_to_critical() {
        let trips = TripPoints::from_kernel_trips(&[
            ("active", 50.0, 5.0),
            ("passive", 85.0, 2.0),
            ("critical", 105.0, 0.0),
        ])
        .unwrap();
        assert_eq!(trips.serious_c, Some(85.0));
        assert_eq!(trips.fair_c, Some(75.0));
        assert_eq!(trips.critical_c, 105.0);
        assert_eq!(trips.recovery_margin_c, 2.0);

        // Only a shutdown trip (a Pi without its board profile): no Fair/Serious.
        let only_critical = TripPoints::from_kernel_trips(&[("critical", 110.0, 0.0)]).unwrap();
        assert_eq!(only_critical.serious_c, None);
        assert_eq!(only_critical.pressure(90.0, Nominal), Nominal);
        assert_eq!(only_critical.recovery_margin_c, 3.0);
        assert_eq!(
            TripPoints::from_kernel_trips(&[("active", 50.0, 0.0)]),
            None
        );
    }

    #[test]
    fn raspberry_pi_detection_needs_a_4_or_5_family_board() {
        assert!(is_raspberry_pi_4_or_5(
            "raspberrypi,4-model-b\0brcm,bcm2711\0"
        ));
        assert!(is_raspberry_pi_4_or_5(
            "raspberrypi,5-model-b\0brcm,bcm2712\0"
        ));
        assert!(!is_raspberry_pi_4_or_5(
            "raspberrypi,3-model-b\0brcm,bcm2837\0"
        ));
        assert!(!is_raspberry_pi_4_or_5(""));
    }

    #[test]
    fn zone_provider_prefers_the_cpu_zone_and_reports_its_band() {
        let root = std::env::temp_dir().join(format!("loadngo-thermal-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let write = |path: &str, text: &str| {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        write("thermal/thermal_zone0/type", "acpitz\n");
        write("thermal/thermal_zone0/temp", "30000\n");
        write("thermal/thermal_zone1/type", "x86_pkg_temp\n");
        write("thermal/thermal_zone1/temp", "91000\n");
        write("thermal/thermal_zone1/trip_point_0_type", "passive\n");
        write("thermal/thermal_zone1/trip_point_0_temp", "90000\n");
        write("thermal/thermal_zone1/trip_point_1_type", "critical\n");
        write("thermal/thermal_zone1/trip_point_1_temp", "100000\n");
        let mut provider =
            ThermalZoneProvider::discover_in(&root.join("thermal"), &root.join("no-device-tree"))
                .unwrap();
        assert_eq!(provider.zone_type(), "x86_pkg_temp");
        let observation = provider.observe();
        assert_eq!(observation.pressure, Serious);
        assert_eq!(observation.temperature_c, Some(91.0));
        assert_eq!(observation.source, ThermalSource::ThermalZone);

        write("thermal/thermal_zone1/temp", "garbage\n");
        assert_eq!(provider.observe().pressure, Unavailable);

        // The same tree on a Pi 5 uses the firmware limits instead of the kernel's.
        write("compatible", "raspberrypi,5-model-b\0brcm,bcm2712\0");
        let pi = ThermalZoneProvider::discover_in(&root.join("thermal"), &root.join("compatible"))
            .unwrap();
        assert_eq!(pi.trips(), TripPoints::RASPBERRY_PI_4_5);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn this_mac_reports_a_native_pressure() {
        let o = NativeProvider.observe();
        assert_eq!(o.source, ThermalSource::NativePressure);
        assert!(o.pressure.severity().is_some());
    }
}
