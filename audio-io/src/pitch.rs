//! Platform-agnostic monophonic pitch detection (the YIN algorithm) and
//! equal-temperament note naming. No I/O here -- callers push captured
//! samples in and pull note readings out, so this half of the crate is
//! plain, directly unit-testable Rust with no `cpal`/device dependency.

use std::collections::VecDeque;

/// A4 concert pitch in Hz. Every note name/frequency in this module is
/// derived from this single reference via equal temperament.
pub const A4_HZ: f32 = 440.0;

const NOTE_NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

/// Standard 4-string bass tuning (low to high), with each string's exact
/// equal-tempered frequency. Used to label "closest string" in a tuner UI;
/// `nearest_note` itself is chromatic and not limited to this set, so a
/// 5-string B or a guitar plugged into the same input still reads sensibly.
pub const BASS_STANDARD_TUNING: [(&str, f32); 4] = [
    ("E1", 41.203_45),
    ("A1", 55.0),
    ("D2", 73.416_19),
    ("G2", 97.998_86),
];

/// Upper bound on fundamentals this module will report. Bass fundamentals
/// top out well under this even on a high fret; capping it keeps YIN's
/// search window away from spurious very-short lag detections.
const MAX_DETECTABLE_FREQUENCY_HZ: f32 = 1_000.0;

/// One YIN detection result before note naming.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PitchEstimate {
    pub frequency_hz: f32,
    /// `1.0` minus the normalized difference at the chosen lag: closer to
    /// `1.0` is a cleaner, more periodic signal. Useful for a UI to fade
    /// out the needle on noise/silence rather than jitter it.
    pub clarity: f32,
}

/// A `PitchEstimate` resolved to the nearest chromatic note.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NoteReading {
    pub name: &'static str,
    /// Scientific pitch octave (A4 -> `4`).
    pub octave: i32,
    /// Signed distance from the nearest note's exact frequency, in cents.
    /// Negative is flat, positive is sharp.
    pub cents_offset: f32,
    pub frequency_hz: f32,
    /// The exact equal-tempered frequency of the nearest note.
    pub target_frequency_hz: f32,
}

/// Maps a detected fundamental to the nearest chromatic note and its cents
/// offset. Returns `None` for a non-finite or non-positive frequency.
#[must_use]
pub fn nearest_note(frequency_hz: f32) -> Option<NoteReading> {
    if !frequency_hz.is_finite() || frequency_hz <= 0.0 {
        return None;
    }
    let semitones_from_a4 = 12.0 * (frequency_hz / A4_HZ).log2();
    let nearest_semitone = semitones_from_a4.round();
    let cents_offset = (semitones_from_a4 - nearest_semitone) * 100.0;
    let midi_number = 69 + nearest_semitone as i32;
    let name = NOTE_NAMES[midi_number.rem_euclid(12) as usize];
    let octave = midi_number.div_euclid(12) - 1;
    let target_frequency_hz = A4_HZ * 2f32.powf(nearest_semitone / 12.0);
    Some(NoteReading {
        name,
        octave,
        cents_offset,
        frequency_hz,
        target_frequency_hz,
    })
}

/// Picks the entry in `BASS_STANDARD_TUNING` closest to `frequency_hz` on a
/// log-frequency (i.e. perceptual/semitone) scale, returning its label and
/// exact frequency.
#[must_use]
pub fn closest_bass_string(frequency_hz: f32) -> Option<(&'static str, f32)> {
    if !frequency_hz.is_finite() || frequency_hz <= 0.0 {
        return None;
    }
    BASS_STANDARD_TUNING
        .iter()
        .map(|&(name, target)| (name, target, (frequency_hz / target).log2().abs()))
        .min_by(|a, b| a.2.total_cmp(&b.2))
        .map(|(name, target, _)| (name, target))
}

/// Accumulates pushed samples into a fixed-size sliding window and runs YIN
/// on demand. Not thread-safe by itself -- callers on a real-time capture
/// thread should hand samples off (e.g. via a channel or ring buffer) to
/// whatever owns this detector rather than share it directly across
/// threads.
pub struct PitchDetector {
    sample_rate: f32,
    window_size: usize,
    buffer: VecDeque<f32>,
    /// YIN's cumulative-mean-normalized-difference absolute threshold: the
    /// first lag whose value drops below this is accepted directly (with
    /// local-minimum refinement) instead of falling back to the global
    /// minimum. `0.15` is the value from the original YIN paper.
    threshold: f32,
}

impl PitchDetector {
    /// `window_size` must be even and large enough that
    /// `sample_rate / (window_size / 2)` is below the lowest fundamental to
    /// detect -- e.g. `4096` at `44_100.0` Hz resolves down to about
    /// `21.5` Hz, comfortably below a 4-string bass's low E (`~41.2` Hz).
    #[must_use]
    pub fn new(sample_rate: f32, window_size: usize) -> Self {
        assert!(window_size >= 8 && window_size.is_multiple_of(2));
        Self {
            sample_rate,
            window_size,
            buffer: VecDeque::with_capacity(window_size),
            threshold: 0.15,
        }
    }

    /// Appends mono samples to the sliding window, discarding the oldest
    /// samples once `window_size` is exceeded.
    pub fn push_samples(&mut self, samples: &[f32]) {
        for &sample in samples {
            if self.buffer.len() == self.window_size {
                self.buffer.pop_front();
            }
            self.buffer.push_back(sample);
        }
    }

    /// Runs YIN over the current window. Returns `None` until the window
    /// has filled once.
    pub fn detect(&mut self) -> Option<PitchEstimate> {
        if self.buffer.len() < self.window_size {
            return None;
        }
        let window = self.buffer.make_contiguous();
        yin_pitch(
            window,
            self.sample_rate,
            self.threshold,
            MAX_DETECTABLE_FREQUENCY_HZ,
        )
    }
}

fn yin_pitch(
    window: &[f32],
    sample_rate: f32,
    threshold: f32,
    max_frequency_hz: f32,
) -> Option<PitchEstimate> {
    let half = window.len() / 2;
    if half < 4 {
        return None;
    }

    // Below this RMS, the window is effectively silence/noise-floor: YIN's
    // normalized difference function is degenerate at all-zero input
    // (0/0 clamped to 0 by the `max(f32::EPSILON)` below reads as perfect
    // periodicity), so treat near-silence as "no pitch" up front rather
    // than let that show up as a spuriously confident reading.
    const SILENCE_RMS_THRESHOLD: f32 = 1e-4;
    let rms = (window.iter().map(|&s| s * s).sum::<f32>() / window.len() as f32).sqrt();
    if rms < SILENCE_RMS_THRESHOLD {
        return None;
    }

    // Difference function: d(tau) = sum_j (x[j] - x[j+tau])^2.
    let mut diff = vec![0.0f32; half];
    for (tau, slot) in diff.iter_mut().enumerate().skip(1) {
        let mut sum = 0.0f32;
        for j in 0..half {
            let delta = window[j] - window[j + tau];
            sum += delta * delta;
        }
        *slot = sum;
    }

    // Cumulative mean normalized difference function.
    let mut cmnd = vec![1.0f32; half];
    let mut running_sum = 0.0f32;
    for tau in 1..half {
        running_sum += diff[tau];
        cmnd[tau] = diff[tau] * tau as f32 / running_sum.max(f32::EPSILON);
    }

    let min_tau = ((sample_rate / max_frequency_hz).ceil() as usize).max(2);
    if min_tau >= half - 1 {
        return None;
    }

    let mut chosen_tau = None;
    let mut tau = min_tau;
    while tau < half - 1 {
        if cmnd[tau] < threshold {
            while tau + 1 < half && cmnd[tau + 1] < cmnd[tau] {
                tau += 1;
            }
            chosen_tau = Some(tau);
            break;
        }
        tau += 1;
    }
    let tau = match chosen_tau {
        Some(tau) => tau,
        None => (min_tau..half - 1).min_by(|&a, &b| cmnd[a].total_cmp(&cmnd[b]))?,
    };

    // Parabolic interpolation around `tau` for sub-sample precision.
    let refined_tau = if tau > min_tau && tau + 1 < half {
        let (y0, y1, y2) = (cmnd[tau - 1], cmnd[tau], cmnd[tau + 1]);
        let denom = y0 - 2.0 * y1 + y2;
        if denom.abs() > f32::EPSILON {
            tau as f32 + 0.5 * (y0 - y2) / denom
        } else {
            tau as f32
        }
    } else {
        tau as f32
    };

    if refined_tau <= 0.0 {
        return None;
    }
    Some(PitchEstimate {
        frequency_hz: sample_rate / refined_tau,
        clarity: 1.0 - cmnd[tau].clamp(0.0, 1.0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine_wave(frequency_hz: f32, sample_rate: f32, len: usize) -> Vec<f32> {
        (0..len)
            .map(|i| {
                let t = i as f32 / sample_rate;
                (2.0 * std::f32::consts::PI * frequency_hz * t).sin()
            })
            .collect()
    }

    fn detect_frequency(frequency_hz: f32) -> f32 {
        let sample_rate = 44_100.0;
        let window_size = 4096;
        let mut detector = PitchDetector::new(sample_rate, window_size);
        detector.push_samples(&sine_wave(frequency_hz, sample_rate, window_size));
        detector
            .detect()
            .unwrap_or_else(|| panic!("expected a pitch estimate for {frequency_hz} Hz"))
            .frequency_hz
    }

    #[test]
    fn detects_a_mid_range_sine_within_half_a_percent() {
        let detected = detect_frequency(220.0);
        assert!(
            (detected - 220.0).abs() < 220.0 * 0.005,
            "detected {detected} Hz, expected ~220 Hz"
        );
    }

    #[test]
    fn detects_the_bass_low_e_string_within_one_percent() {
        let target = 41.203_45;
        let detected = detect_frequency(target);
        assert!(
            (detected - target).abs() < target * 0.01,
            "detected {detected} Hz, expected ~{target} Hz"
        );
    }

    #[test]
    fn detects_the_bass_g_string_within_half_a_percent() {
        let target = 97.998_86;
        let detected = detect_frequency(target);
        assert!(
            (detected - target).abs() < target * 0.005,
            "detected {detected} Hz, expected ~{target} Hz"
        );
    }

    #[test]
    fn silence_produces_no_pitch_estimate() {
        let sample_rate = 44_100.0;
        let window_size = 4096;
        let mut detector = PitchDetector::new(sample_rate, window_size);
        detector.push_samples(&vec![0.0f32; window_size]);
        assert_eq!(detector.detect(), None);
    }

    #[test]
    fn nearest_note_identifies_concert_a4_with_no_offset() {
        let reading = nearest_note(440.0).unwrap();
        assert_eq!(reading.name, "A");
        assert_eq!(reading.octave, 4);
        assert!(reading.cents_offset.abs() < 0.01);
    }

    #[test]
    fn nearest_note_identifies_bass_low_e_string() {
        let reading = nearest_note(41.203_45).unwrap();
        assert_eq!(reading.name, "E");
        assert_eq!(reading.octave, 1);
        assert!(reading.cents_offset.abs() < 1.0);
    }

    #[test]
    fn nearest_note_reports_sharp_and_flat_signs_correctly() {
        // A quarter-tone sharp of A4.
        let sharp = nearest_note(440.0 * 2f32.powf(0.25 / 12.0)).unwrap();
        assert_eq!(sharp.name, "A");
        assert!(sharp.cents_offset > 0.0, "{}", sharp.cents_offset);

        // A quarter-tone flat of A4.
        let flat = nearest_note(440.0 * 2f32.powf(-0.25 / 12.0)).unwrap();
        assert_eq!(flat.name, "A");
        assert!(flat.cents_offset < 0.0, "{}", flat.cents_offset);
    }

    #[test]
    fn nearest_note_rejects_non_positive_and_non_finite_input() {
        assert_eq!(nearest_note(0.0), None);
        assert_eq!(nearest_note(-10.0), None);
        assert_eq!(nearest_note(f32::NAN), None);
        assert_eq!(nearest_note(f32::INFINITY), None);
    }

    #[test]
    fn closest_bass_string_picks_the_nearest_open_string() {
        let (name, target) = closest_bass_string(56.0).unwrap();
        assert_eq!(name, "A1");
        assert!((target - 55.0).abs() < 0.01);

        let (name, _) = closest_bass_string(40.0).unwrap();
        assert_eq!(name, "E1");

        let (name, _) = closest_bass_string(100.0).unwrap();
        assert_eq!(name, "G2");
    }
}
