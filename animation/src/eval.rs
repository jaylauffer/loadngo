//! Measures of how natural a recorded walk is, from joint positions alone.
//!
//! Contacts here are found from the output geometry (a foot joint within 1 cm of the
//! height it stands at), never from the database's labels, so a held foot that still slides
//! shows. Every per-frame measure
//! is stated per 1/60 s frame whatever the recording's rate, like a 60 Hz check.

use crate::humanoid::Humanoid;
use crate::math::Vec3;

/// A recording of world joint positions, one entry a frame.
#[derive(Debug, Clone, Default)]
pub struct Recording {
    pub hz: f32,
    pub bones: usize,
    /// `frames * bones` world positions.
    pub positions: Vec<Vec3>,
    /// The root on the ground and its facing (radians) each frame.
    pub root: Vec<(Vec3, f32)>,
    /// Database transitions so far, each frame.
    pub transitions: Vec<u64>,
}

impl Recording {
    #[must_use]
    pub fn new(hz: f32, bones: usize) -> Self {
        Self {
            hz,
            bones,
            ..Self::default()
        }
    }

    pub fn push(&mut self, positions: &[Vec3], root: Vec3, facing: f32, transitions: u64) {
        self.positions.extend_from_slice(&positions[..self.bones]);
        self.root.push((root, facing));
        self.transitions.push(transitions);
    }

    #[must_use]
    pub fn frames(&self) -> usize {
        self.root.len()
    }

    #[must_use]
    pub fn at(&self, frame: usize, bone: usize) -> Vec3 {
        self.positions[frame * self.bones + bone]
    }
}

/// One foot contact found in a recording.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Contact {
    pub left: bool,
    /// The joint was the toe (ball) rather than the ankle (heel).
    pub ball: bool,
    pub start: usize,
    pub frames: usize,
    /// The farthest the joint moved over the ground from where it touched down (m).
    pub slide: f32,
}

/// What a walk measured.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Metrics {
    /// Worst and 95th-percentile foot slide over contacts (cm).
    pub slide_worst_cm: f32,
    pub slide_p95_cm: f32,
    pub contacts: usize,
    /// Largest per-frame acceleration (second difference) of the torso joints and of the
    /// limb joints (cm per 60 Hz frame²), and the largest out-and-back jog (cm).
    pub torso_acc_cm: f32,
    pub limb_acc_cm: f32,
    pub jog_cm: f32,
    /// Root-mean-square jerk (third difference) over all joints (cm per frame³).
    pub jerk_rms_cm: f32,
    /// Over the steady window: steps a minute, metres a step, metres a second.
    pub cadence: f32,
    pub step_length: f32,
    pub speed: f32,
    pub steps: usize,
    /// Hips' side-to-side travel about their own line, 3rd to 97th percentile (cm).
    pub hip_sway_cm: f32,
    /// Knee bend (0 = straight) at heel strike, median, and at its most in each swing,
    /// median (degrees).
    pub knee_at_strike_deg: f32,
    pub knee_swing_deg: f32,
    /// Transitions, and per second.
    pub transitions: u64,
    pub transitions_per_s: f32,
}

fn percentile(values: &mut [f32], p: f32) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(f32::total_cmp);
    let i = ((values.len() as f32 * p) as usize).min(values.len() - 1);
    values[i]
}

/// How high a joint stands when it is on the floor: the 5th percentile of its height over
/// the recording (a walk spends about 60% of its time in stance).
#[must_use]
pub fn floor_height(rec: &Recording, bone: usize) -> f32 {
    let mut h: Vec<f32> = (0..rec.frames()).map(|f| rec.at(f, bone).y).collect();
    percentile(&mut h, 0.05)
}

/// The band over a joint's floor height that counts as on the floor (m): a sole touching,
/// not a foot skimming the floor in its swing (real toe clearance is 1-2 cm).
pub const FLOOR_BAND: f32 = 0.01;

/// Contacts of one joint: runs of at least 4 frames within `band` of `floor`.
#[must_use]
pub fn contacts(
    rec: &Recording,
    bone: usize,
    floor: f32,
    band: f32,
    left: bool,
    ball: bool,
) -> Vec<Contact> {
    let (rest_height, height) = (floor, band);
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    for f in 0..=rec.frames() {
        let low = f < rec.frames() && rec.at(f, bone).y - rest_height < height;
        match (low, start) {
            (true, None) => start = Some(f),
            (false, Some(s)) => {
                if f - s >= 4 {
                    let p0 = rec.at(s, bone).flat();
                    let slide = (s..f)
                        .map(|k| (rec.at(k, bone).flat() - p0).length())
                        .fold(0.0, f32::max);
                    out.push(Contact {
                        left,
                        ball,
                        start: s,
                        frames: f - s,
                        slide,
                    });
                }
                start = None;
            }
            _ => {}
        }
    }
    out
}

/// A foot's contacts by the point on the floor: the ball (toe joint) while it is within the
/// band of its floor height, otherwise the heel (ankle) while it is. A contact runs while
/// either is down; its slide is how far the point down has moved since it touched (when
/// the ball comes down after the heel, it is measured from where the ball came down).
/// This is what a planted foot keeps still: an ankle rolling forward over a planted ball as
/// the heel lifts is not a slide.
#[must_use]
pub fn foot_contacts(rec: &Recording, humanoid: &Humanoid, side: usize) -> Vec<Contact> {
    let leg = humanoid.legs[side];
    let fa = floor_height(rec, leg.ankle);
    let ft = floor_height(rec, leg.toe);
    let mut out = Vec::new();
    // (start, the point now: ball?, its touch-down position, slide so far)
    let mut cur: Option<(usize, bool, Vec3, f32)> = None;
    for f in 0..=rec.frames() {
        let down = if f < rec.frames() {
            let ball = rec.at(f, leg.toe).y - ft < FLOOR_BAND;
            let heel = rec.at(f, leg.ankle).y - fa < FLOOR_BAND;
            if ball {
                Some(true)
            } else if heel {
                Some(false)
            } else {
                None
            }
        } else {
            None
        };
        match (down, cur.as_mut()) {
            (Some(ball), None) => {
                let p = rec.at(f, if ball { leg.toe } else { leg.ankle }).flat();
                cur = Some((f, ball, p, 0.0));
            }
            (Some(ball), Some(c)) => {
                let bone = if ball { leg.toe } else { leg.ankle };
                if ball != c.1 {
                    // The ball has come down (or the heel taken over): measure from here.
                    c.1 = ball;
                    c.2 = rec.at(f, bone).flat();
                }
                c.3 = c.3.max((rec.at(f, bone).flat() - c.2).length());
            }
            (None, Some(c)) => {
                if f - c.0 >= 4 {
                    out.push(Contact {
                        left: side == 0,
                        ball: c.1,
                        start: c.0,
                        frames: f - c.0,
                        slide: c.3,
                    });
                }
                cur = None;
            }
            (None, None) => {}
        }
    }
    out
}

fn knee_bend(rec: &Recording, f: usize, h: &Humanoid, side: usize) -> f32 {
    let l = h.legs[side];
    let a = rec.at(f, l.up);
    let b = rec.at(f, l.knee);
    let c = rec.at(f, l.ankle);
    let t = (b - a).normalize_or_zero();
    let s = (c - b).normalize_or_zero();
    t.dot(s).clamp(-1.0, 1.0).acos().to_degrees()
}

/// Measures `rec` (bones indexed as `humanoid` says). Each foot joint's floor height is
/// found from the recording ([`floor_height`]). Gait measures use only frames
/// `[steady.0, steady.1)`.
#[must_use]
pub fn measure(
    rec: &Recording,
    humanoid: &Humanoid,
    torso: &[usize],
    steady: (usize, usize),
) -> Metrics {
    let mut m = Metrics::default();
    let n = rec.frames();
    if n < 4 {
        return m;
    }
    let sv = rec.hz / 60.0;
    let sa = sv * sv;
    let sj = sa * sv;
    // Contacts and slide, by the point on the floor; heel strikes by the ankle alone.
    let mut all = Vec::new();
    let mut strikes_all = Vec::new();
    let mut floors = [[0.0f32; 2]; 2];
    for (side, floor) in floors.iter_mut().enumerate() {
        let l = humanoid.legs[side];
        *floor = [floor_height(rec, l.ankle), floor_height(rec, l.toe)];
        all.extend(foot_contacts(rec, humanoid, side));
        strikes_all.extend(contacts(
            rec,
            l.ankle,
            floor[0],
            FLOOR_BAND,
            side == 0,
            false,
        ));
    }
    let mut slides: Vec<f32> = all.iter().map(|c| c.slide * 100.0).collect();
    m.contacts = slides.len();
    m.slide_p95_cm = percentile(&mut slides, 0.95);
    m.slide_worst_cm = slides.last().copied().unwrap_or(0.0);
    // Accelerations, jogs and jerk.
    let mut jerk_sum = 0.0f64;
    let mut jerk_n = 0usize;
    for b in 0..rec.bones {
        let is_torso = torso.contains(&b);
        for f in 1..n - 1 {
            let (p0, p1, p2) = (rec.at(f - 1, b), rec.at(f, b), rec.at(f + 1, b));
            let acc = (p2 - p1 * 2.0 + p0).length() * 100.0 * sa;
            if is_torso {
                m.torso_acc_cm = m.torso_acc_cm.max(acc);
            } else {
                m.limb_acc_cm = m.limb_acc_cm.max(acc);
            }
            let (a, c) = (p1 - p0, p2 - p1);
            let (la, lc) = (a.length(), c.length());
            if la > 0.0 && lc > 0.0 && a.dot(c) / (la * lc) < -0.5 {
                m.jog_cm = m.jog_cm.max(la.min(lc) * 100.0 * sv);
            }
            if f + 2 < n {
                let p3 = rec.at(f + 2, b);
                let j = (p3 - p2 * 3.0 + p1 * 3.0 - p0).length() * 100.0 * sj;
                jerk_sum += f64::from(j * j);
                jerk_n += 1;
            }
        }
    }
    m.jerk_rms_cm = if jerk_n > 0 {
        (jerk_sum / jerk_n as f64).sqrt() as f32
    } else {
        0.0
    };
    // Gait over the steady window: heel strikes alternate.
    let (s0, s1) = (steady.0.min(n), steady.1.min(n));
    let mut strikes: Vec<(usize, usize)> = strikes_all
        .iter()
        .filter(|c| c.start >= s0 && c.start < s1)
        .map(|c| (c.start, usize::from(!c.left)))
        .collect();
    strikes.sort_unstable();
    let mut steps_t = 0.0f32;
    let mut steps_m = 0.0f32;
    let mut steps = 0usize;
    let mut knees = Vec::new();
    for w in strikes.windows(2) {
        let ((f0, side0), (f1, side1)) = (w[0], w[1]);
        knees.push(knee_bend(rec, f1, humanoid, side1));
        if side0 == side1 {
            continue;
        }
        let a = rec.at(f0, humanoid.legs[side0].ankle).flat();
        let b = rec.at(f1, humanoid.legs[side1].ankle).flat();
        steps_t += (f1 - f0) as f32 / rec.hz;
        steps_m += (b - a).length();
        steps += 1;
    }
    if let Some(&(f, s)) = strikes.first() {
        knees.push(knee_bend(rec, f, humanoid, s));
    }
    if steps > 0 {
        m.steps = steps;
        m.cadence = 60.0 * steps as f32 / steps_t;
        m.step_length = steps_m / steps as f32;
    }
    m.knee_at_strike_deg = percentile(&mut knees, 0.5);
    // Knee swing peaks: the most bend between a foot's toe-off and its next strike, per side.
    let mut swing_peaks = Vec::new();
    for (side, floor) in floors.iter().enumerate() {
        let mut peak = 0.0f32;
        let mut in_swing = false;
        for f in s0..s1 {
            let ankle = rec.at(f, humanoid.legs[side].ankle);
            let toe = rec.at(f, humanoid.legs[side].toe);
            let low = ankle.y - floor[0] < FLOOR_BAND || toe.y - floor[1] < FLOOR_BAND;
            if !low {
                in_swing = true;
                peak = peak.max(knee_bend(rec, f, humanoid, side));
            } else if in_swing {
                swing_peaks.push(peak);
                peak = 0.0;
                in_swing = false;
            }
        }
    }
    m.knee_swing_deg = percentile(&mut swing_peaks, 0.5);
    // Speed over the steady window.
    if s1 > s0 + 1 {
        let mut d = 0.0;
        for f in s0 + 1..s1 {
            d += (rec.root[f].0 - rec.root[f - 1].0).flat().length();
        }
        m.speed = d / ((s1 - s0 - 1) as f32 / rec.hz);
    }
    // Hip sway: hips about their own line (a moving average over 1.2 s), sideways to it.
    if s1 > s0 + 10 {
        let half = (0.6 * rec.hz) as usize;
        let mut lateral = Vec::new();
        for f in s0 + half..s1.saturating_sub(half) {
            let mut mean = Vec3::ZERO;
            for k in f - half..=f + half {
                mean += rec.at(k, humanoid.hips).flat();
            }
            mean = mean / (2 * half + 1) as f32;
            let dir = (rec.at(f + half, humanoid.hips) - rec.at(f - half, humanoid.hips))
                .flat()
                .normalize_or_zero();
            let side = Vec3::new(dir.z, 0.0, -dir.x);
            lateral.push((rec.at(f, humanoid.hips).flat() - mean).dot(side) * 100.0);
        }
        if !lateral.is_empty() {
            let mut l2 = lateral.clone();
            let lo = percentile(&mut lateral, 0.03);
            let hi = percentile(&mut l2, 0.97);
            m.hip_sway_cm = hi - lo;
        }
    }
    m.transitions = rec.transitions.last().copied().unwrap_or(0)
        - rec.transitions.first().copied().unwrap_or(0);
    m.transitions_per_s = m.transitions as f32 / (n as f32 / rec.hz);
    m
}
