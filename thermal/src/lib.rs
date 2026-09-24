//! Portable thermal awareness: the model, governor and providers specified in
//! `docs/THERMAL_AWARENESS.md` (implementation sequence steps 1 and 4, macOS/iOS part).
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

/// For platforms without a proven provider (Windows, NetBSD, and Linux until its
/// sysfs provider lands): always `Unavailable`.
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

/// The best provider this platform has.
#[must_use]
pub fn platform_provider() -> Box<dyn ThermalProvider> {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        Box::new(NativeProvider)
    }
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
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

    #[cfg(target_os = "macos")]
    #[test]
    fn this_mac_reports_a_native_pressure() {
        let o = NativeProvider.observe();
        assert_eq!(o.source, ThermalSource::NativePressure);
        assert!(o.pressure.severity().is_some());
    }
}
