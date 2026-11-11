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

/// The bones a posture is measured on, found by their Mixamo-style names.
#[derive(Debug, Clone, Copy)]
pub struct PostureBones {
    pub hips: usize,
    pub neck: usize,
    /// Per side (left, right): the thigh's root (hip joint), shoulder, elbow and wrist.
    pub up_leg: [usize; 2],
    pub arm: [usize; 2],
    pub fore_arm: [usize; 2],
    pub hand: [usize; 2],
}

impl PostureBones {
    /// Found in `names`, or `None` if any is missing.
    #[must_use]
    pub fn find(names: &[String]) -> Option<Self> {
        let f = |n: &str| {
            names
                .iter()
                .position(|x| crate::humanoid::base_name(x) == n)
        };
        Some(Self {
            hips: f("Hips")?,
            neck: f("Neck")?,
            up_leg: [f("LeftUpLeg")?, f("RightUpLeg")?],
            arm: [f("LeftArm")?, f("RightArm")?],
            fore_arm: [f("LeftForeArm")?, f("RightForeArm")?],
            hand: [f("LeftHand")?, f("RightHand")?],
        })
    }
}

/// How a walker carries themselves over the steady part of a walk: the measures a person
/// sees as "walking like a robot" when they are off (forearms held out, a crouch).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Posture {
    /// The forearm's angle from hanging straight down (elbow to wrist against the vertical,
    /// degrees), median and 90th percentile over both arms and every steady frame.
    pub forearm_p50_deg: f32,
    pub forearm_p90_deg: f32,
    /// The wrist's height below the hip joint (m, positive below), median over both sides.
    pub wrist_below_hip_m: f32,
    /// The trunk's lean from the vertical (hips to neck, degrees, positive forward along the
    /// walker's facing), median.
    pub trunk_lean_deg: f32,
    /// The knee's bend through mid-stance (degrees): at the middle frame of each foot
    /// contact, median over contacts.
    pub knee_midstance_deg: f32,
}

/// Measures the posture of `rec` over `[steady.0, steady.1)`, mid-stance from the feet's
/// contacts (as [`foot_contacts`] finds them).
#[must_use]
pub fn posture(
    rec: &Recording,
    humanoid: &Humanoid,
    b: &PostureBones,
    steady: (usize, usize),
) -> Posture {
    let (a, z) = (steady.0, steady.1.min(rec.frames()));
    if z <= a {
        return Posture::default();
    }
    let mut fore = Vec::with_capacity(2 * (z - a));
    let mut wrist = Vec::with_capacity(2 * (z - a));
    let mut lean = Vec::with_capacity(z - a);
    for f in a..z {
        for s in 0..2 {
            let e = rec.at(f, b.fore_arm[s]);
            let w = rec.at(f, b.hand[s]);
            let d = (w - e).normalize_or_zero();
            fore.push((-d.y).clamp(-1.0, 1.0).acos().to_degrees());
            wrist.push(rec.at(f, b.up_leg[s]).y - w.y);
        }
        let (_, facing) = rec.root[f];
        let fwd = Vec3::new(facing.sin(), 0.0, facing.cos());
        let t = (rec.at(f, b.neck) - rec.at(f, b.hips)).normalize_or_zero();
        lean.push(t.dot(fwd).atan2(t.y).to_degrees());
    }
    let mut knees = Vec::new();
    for side in 0..2 {
        for c in foot_contacts(rec, humanoid, side) {
            let mid = c.start + c.frames / 2;
            if mid >= a && mid < z {
                knees.push(knee_bend(rec, mid, humanoid, side));
            }
        }
    }
    Posture {
        forearm_p50_deg: percentile(&mut fore.clone(), 0.5),
        forearm_p90_deg: percentile(&mut fore, 0.9),
        wrist_below_hip_m: percentile(&mut wrist, 0.5),
        trunk_lean_deg: percentile(&mut lean, 0.5),
        knee_midstance_deg: percentile(&mut knees, 0.5),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::humanoid::Leg;

    /// A figure standing still for 2 s, its forearms at `fore` degrees from hanging down,
    /// leaning `lean` degrees forward (+Z), the knees straight.
    fn figure(fore: f32, lean: f32) -> (Recording, Humanoid, PostureBones) {
        let (f, l) = (fore.to_radians(), lean.to_radians());
        let mut p = vec![Vec3::ZERO; 16];
        p[0] = Vec3::new(0.0, 1.0, 0.0); // hips
        p[1] = p[0] + Vec3::new(0.0, l.cos(), l.sin()) * 0.5; // neck
        for s in 0..2 {
            let x = if s == 0 { 0.1 } else { -0.1 };
            p[2 + s] = Vec3::new(x, 0.95, 0.0); // hip joint
            p[4 + s] = p[1] + Vec3::new(x * 1.8, 0.0, 0.0); // shoulder
            p[6 + s] = p[4 + s] - Vec3::new(0.0, 0.3, 0.0); // elbow
            p[8 + s] = p[6 + s] + Vec3::new(0.0, -f.cos(), f.sin()) * 0.25; // wrist
            p[10 + 3 * s] = Vec3::new(x, 0.5, 0.0); // knee
            p[11 + 3 * s] = Vec3::new(x, 0.08, 0.0); // ankle
            p[12 + 3 * s] = Vec3::new(x, 0.02, 0.14); // toe
        }
        let mut rec = Recording::new(60.0, 16);
        for _ in 0..120 {
            rec.push(&p, Vec3::ZERO, 0.0, 0);
        }
        let leg = |s: usize| Leg {
            up: 2 + s,
            knee: 10 + 3 * s,
            ankle: 11 + 3 * s,
            toe: 12 + 3 * s,
        };
        let h = Humanoid {
            hips: 0,
            legs: [leg(0), leg(1)],
            arms: [4, 5],
        };
        let b = PostureBones {
            hips: 0,
            neck: 1,
            up_leg: [2, 3],
            arm: [4, 5],
            fore_arm: [6, 7],
            hand: [8, 9],
        };
        (rec, h, b)
    }

    #[test]
    fn posture_reads_forearms_lean_and_knees() {
        let (rec, h, b) = figure(0.0, 0.0);
        let p = posture(&rec, &h, &b, (0, 120));
        assert!(p.forearm_p50_deg < 0.5, "{p:?}");
        assert!(p.trunk_lean_deg.abs() < 0.5);
        assert!(p.knee_midstance_deg < 0.5);
        let (rec, h, b) = figure(60.0, 12.0);
        let p = posture(&rec, &h, &b, (0, 120));
        assert!((p.forearm_p50_deg - 60.0).abs() < 0.5, "{p:?}");
        assert!((p.trunk_lean_deg - 12.0).abs() < 0.5);
        // The wrist 0.3 + 0.25 cos 60 below the shoulder, the shoulder 0.5 cos 12 over the hips.
        let wrist = 1.0 + 0.5 * 12f32.to_radians().cos() - 0.3 - 0.125;
        assert!((p.wrist_below_hip_m - (0.95 - wrist)).abs() < 1e-3);
    }
}
