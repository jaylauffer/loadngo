//! `motion_eval`: drives a character through scripted walks with a motion-matching
//! database and reports how natural the result is: metrics, per-frame cost, allocations,
//! and frame strips beside the capture itself. `--help` says how.

use std::alloc::{GlobalAlloc, Layout, System};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use data::cli::{read_args, ArgDoc, Usage};
use loadngo_animation::character::{Character, Seat, State};
use loadngo_animation::controller::{Config, MotionMatcher};
use loadngo_animation::database::Database;
use loadngo_animation::eval::{self, Metrics, Recording};
use loadngo_animation::math::{Transform, Vec3};
use loadngo_animation_build::io;
use loadngo_animation_build::scenarios::{self, Leg, Scenario, Script};
use loadngo_animation_build::strip::{self, Canvas, PanelPose, TopView};
use loadngo_proactor::{PlatformPort, Proactor};

/// Counts allocations while `COUNTING` is set, to prove the step allocates nothing.
struct Counting;
static COUNTING: AtomicBool = AtomicBool::new(false);
static ALLOCS: AtomicU64 = AtomicU64::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        if COUNTING.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        if COUNTING.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.realloc(p, l, n) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

const USAGE: Usage = Usage {
    bin: "motion_eval",
    invocation: "cargo run --release -p loadngo-animation-build --bin motion_eval --",
    about: "drives a character through scripted walks (straight, start and stop, 90-degree turns, a curve, a slower walk alongside) with a motion-matching database, and reports foot slide, jerk, cadence, step length, hip sway, knees, transitions, per-frame cost and allocations, with side-on frame strips beside the capture played as recorded.",
    args: &[
        ArgDoc::required("--db", "<database.lmm>", "the database anim_build wrote"),
        ArgDoc::required("--out-dir", "<directory>", "where the report (report.json, report.md) and strips (<scenario>.png) go; created if missing"),
        ArgDoc::repeated("--scenario", "<name>", "run only these (straight, start-stop, turn-left-90, turn-right-90, turn-off-80, turn-off-180, curve, alongside-slow, sit-bench, sit-chair); default all"),
        ArgDoc::optional("--hz", "<rate>", "steps a second (default 60)"),
        ArgDoc::optional("--repeat", "<n>", "run every scenario n times for timing (default 1)"),
        ArgDoc::switch("--no-strips", "skip the PNG strips"),
        ArgDoc::switch("--no-foot-lock", "turn the foot IK off, to see what it does"),
        ArgDoc::repeated("--set", "<name=value>", "override a runtime setting (controller::Config field names, e.g. transition_cost=0.5)"),
        ArgDoc::repeated("--path", "<path.json>", "also run a scenario along a path: {name, about, points: [[x, z], ...], speed, facing, wait_before, wait_after, steady: [from_s, to_s]}"),
        ArgDoc::repeated("--tracks", "<tracks.json>", "also judge and draw recorded joint tracks with the same measures: {name, about, hz, names, parents, positions: [[x, y, z] per joint per frame], root: [[x, z, facing] per frame], steady: [from_s, to_s]}; Mixamo joint names"),
        ArgDoc::switch("--capture", "also measure each original clip played as recorded (no matching, no IK): the floor the data sets"),
        ArgDoc::switch("--dump-feet", "also write <scenario>-feet.csv: each frame's ankle and toe heights and ground speeds, and the contacts the runtime held"),
    ],
    examples: &[
        "cargo run --release -p loadngo-animation-build --bin motion_eval -- --db /tmp/walk.lmm --out-dir /tmp/walk-eval",
        "cargo run --release -p loadngo-animation-build --bin motion_eval -- --db body.lmm --out-dir out --path route.json --tracks recorded.json",
    ],
    notes: &[
        "Foot slide is measured from the output's joints, never from the database's labels: per contact, how far the point on the floor (the ball while within 1 cm of its floor height, else the heel) moved since it touched down.",
        "Per-frame figures are per 1/60 s frame whatever --hz is, as a 60 Hz check states them.",
        "Allocations are counted only inside MotionMatcher::step (Character::step for the seat scenarios); anything but 0 is a defect.",
        "MOTION_EVAL_VERBOSE=1 also prints the largest accelerations with the clip playing; MOTION_EVAL_STATES=1 prints a seat scenario's state, root and hips four times a second.",
    ],
};

struct Args {
    db: PathBuf,
    out: PathBuf,
    only: Vec<String>,
    hz: f32,
    repeat: usize,
    strips: bool,
    foot_lock: bool,
    dump_feet: bool,
    settings: Vec<(String, String)>,
    capture: bool,
    paths: Vec<PathBuf>,
    tracks: Vec<PathBuf>,
}

fn fail(m: &str) -> ! {
    eprintln!("motion_eval: {m}");
    eprintln!("{}", USAGE.hint());
    std::process::exit(2);
}

fn parse() -> Args {
    let argv = read_args(&USAGE, true);
    let mut a = Args {
        db: PathBuf::new(),
        out: PathBuf::new(),
        only: Vec::new(),
        hz: 60.0,
        repeat: 1,
        strips: true,
        foot_lock: true,
        dump_feet: false,
        settings: Vec::new(),
        capture: false,
        paths: Vec::new(),
        tracks: Vec::new(),
    };
    let mut it = argv.into_iter();
    while let Some(f) = it.next() {
        let mut v = || {
            it.next()
                .unwrap_or_else(|| fail(&format!("{f} needs a value")))
        };
        match f.as_str() {
            "--db" => a.db = v().into(),
            "--out-dir" => a.out = v().into(),
            "--scenario" => a.only.push(v()),
            "--hz" => {
                a.hz = v()
                    .parse()
                    .ok()
                    .filter(|h: &f32| *h >= 10.0 && *h <= 500.0)
                    .unwrap_or_else(|| fail("--hz needs 10 to 500"))
            }
            "--repeat" => {
                a.repeat = v()
                    .parse()
                    .ok()
                    .filter(|n: &usize| *n >= 1)
                    .unwrap_or_else(|| fail("--repeat needs a positive whole number"))
            }
            "--no-strips" => a.strips = false,
            "--no-foot-lock" => a.foot_lock = false,
            "--dump-feet" => a.dump_feet = true,
            "--capture" => a.capture = true,
            "--path" => a.paths.push(v().into()),
            "--tracks" => a.tracks.push(v().into()),
            "--set" => {
                let kv = v();
                let (k, val) = kv
                    .split_once('=')
                    .unwrap_or_else(|| fail(&format!("--set {kv:?}: expected name=value")));
                let mut probe = Config::default();
                probe.set(k, val).unwrap_or_else(|e| fail(&e));
                a.settings.push((k.to_string(), val.to_string()));
            }
            other => fail(&format!("unknown argument {other:?}")),
        }
    }
    if a.db.as_os_str().is_empty() || a.out.as_os_str().is_empty() {
        fail("--db and --out-dir are required");
    }
    a
}

struct Run {
    rec: Recording,
    sim: Vec<Vec3>,
    step_ns: Vec<u64>,
    searched: Vec<bool>,
    allocs: u64,
    /// Feet held by the runtime each frame: bit 0 left, bit 1 right.
    held: Vec<u8>,
    /// Database frame and its contact bits each frame.
    db_frame: Vec<(usize, u8)>,
    searches: u64,
    unlocks: u64,
}

fn run(
    db: &Arc<Database>,
    s: &Scenario,
    hz: f32,
    foot_lock: bool,
    settings: &[(String, String)],
) -> Run {
    let mut cfg = Config {
        foot_lock,
        ..Config::default()
    };
    for (k, v) in settings {
        cfg.set(k, v).unwrap_or_else(|e| fail(&e));
    }
    let mut m = MotionMatcher::new(Arc::clone(db), cfg, s.start, s.facing);
    let mut script = Script::new(s);
    let frames = (s.seconds * hz) as usize;
    let mut rec = Recording::new(hz, db.bone_count());
    let mut sim = Vec::with_capacity(frames);
    let mut step_ns = Vec::with_capacity(frames);
    let mut searched = Vec::with_capacity(frames);
    let dt = 1.0 / hz;
    let mut allocs = 0;
    let mut held = Vec::with_capacity(frames);
    let mut db_frame = Vec::with_capacity(frames);
    for _ in 0..frames {
        let simulation = m.simulation();
        script.update(simulation.translation, 0.0f32.max(speed_of(&m)), dt);
        let before = m.stats().searches;
        ALLOCS.store(0, Ordering::Relaxed);
        COUNTING.store(true, Ordering::Relaxed);
        let t0 = Instant::now();
        m.step(dt, &script);
        let ns = t0.elapsed().as_nanos() as u64;
        COUNTING.store(false, Ordering::Relaxed);
        allocs += ALLOCS.load(Ordering::Relaxed);
        step_ns.push(ns);
        searched.push(m.stats().searches > before);
        let r = m.root();
        rec.push(
            m.world_positions(),
            r.translation,
            r.rotation.yaw(),
            m.stats().transitions,
        );
        sim.push(m.simulation().translation);
        let fh = m.feet_held();
        held.push(u8::from(fh[0]) | (u8::from(fh[1]) << 1));
        db_frame.push((m.frame(), m.contacts()));
    }
    Run {
        rec,
        sim,
        step_ns,
        searched,
        allocs,
        held,
        db_frame,
        searches: m.stats().searches,
        unlocks: m.stats().unlocks_by_radius,
    }
}

/// A scenario with seats, played through the character controller: each leg is a command,
/// ending when the character is where the command leaves it.
fn run_character(
    db: &Arc<Database>,
    s: &Scenario,
    hz: f32,
    foot_lock: bool,
    settings: &[(String, String)],
) -> Run {
    let mut cfg = Config {
        foot_lock,
        ..Config::default()
    };
    for (k, v) in settings {
        cfg.set(k, v).unwrap_or_else(|e| fail(&e));
    }
    let mut c = Character::new(Arc::clone(db), cfg, s.start, s.facing);
    let frames = (s.seconds * hz) as usize;
    let mut rec = Recording::new(hz, db.bone_count());
    let mut sim = Vec::with_capacity(frames);
    let mut step_ns = Vec::with_capacity(frames);
    let mut searched = Vec::with_capacity(frames);
    let mut held = Vec::with_capacity(frames);
    let mut db_frame = Vec::with_capacity(frames);
    let dt = 1.0 / hz;
    let mut allocs = 0;
    let (mut leg, mut leg_time, mut entered) = (0usize, 0.0f32, false);
    for _ in 0..frames {
        if !entered {
            match &s.legs[leg] {
                Leg::Walk(p, speed) => c.go_to(p, *speed, None),
                Leg::Wait(_) => {}
                Leg::Sit(hips, facing) => c.sit(Seat {
                    hips: *hips,
                    facing: *facing,
                }),
                Leg::Stand => c.stand(),
            }
            entered = true;
            leg_time = 0.0;
        }
        let before = c.matcher().stats().searches;
        ALLOCS.store(0, Ordering::Relaxed);
        COUNTING.store(true, Ordering::Relaxed);
        let t0 = Instant::now();
        c.step(dt);
        let ns = t0.elapsed().as_nanos() as u64;
        COUNTING.store(false, Ordering::Relaxed);
        allocs += ALLOCS.load(Ordering::Relaxed);
        step_ns.push(ns);
        searched.push(c.matcher().stats().searches > before);
        leg_time += dt;
        let r = c.root();
        rec.push(
            c.world_positions(),
            r.translation,
            r.rotation.yaw(),
            c.matcher().stats().transitions,
        );
        sim.push(c.matcher().simulation().translation);
        let fh = c.matcher().feet_held();
        held.push(u8::from(fh[0]) | (u8::from(fh[1]) << 1));
        db_frame.push((c.matcher().frame(), c.matcher().contacts()));
        if std::env::var_os("MOTION_EVAL_STATES").is_some()
            && rec.frames().is_multiple_of((hz as usize / 4).max(1))
        {
            let fr = c.matcher().frame();
            eprintln!(
                "  {:.2}s {:?} root ({:.2}, {:.2}) facing {:.2} approach ({:.2}, {:.2}) {:.2} hips ({:.2}, {:.2}, {:.2}) playing {}",
                rec.frames() as f32 / hz,
                c.state(),
                r.translation.x,
                r.translation.z,
                r.rotation.yaw(),
                c.approach_spot().translation.x,
                c.approach_spot().translation.z,
                c.approach_spot().rotation.yaw(),
                c.world_positions()[0].x,
                c.world_positions()[0].y,
                c.world_positions()[0].z,
                db.clip_of(fr).map_or("?".to_string(), |k| format!("{}{} @{}", k.name, if k.mirrored { "-m" } else { "" }, fr - k.start))
            );
        }
        let done = match &s.legs[leg] {
            Leg::Wait(t) => leg_time >= *t,
            Leg::Walk(..) | Leg::Stand => leg_time > 0.25 && c.state() == State::Standing,
            Leg::Sit(..) => c.state() == State::Seated,
        };
        if done && leg + 1 < s.legs.len() {
            if std::env::var_os("MOTION_EVAL_VERBOSE").is_some() {
                eprintln!(
                    "  {}: leg {leg} done at {:.2}s",
                    s.name,
                    rec.frames() as f32 / hz
                );
            }
            leg += 1;
            entered = false;
        }
    }
    let m = c.matcher();
    Run {
        rec,
        sim,
        step_ns,
        searched,
        allocs,
        held,
        db_frame,
        searches: m.stats().searches,
        unlocks: m.stats().unlocks_by_radius,
    }
}

fn speed_of(m: &MotionMatcher) -> f32 {
    // The simulation's speed: how far the next predicted point is over its horizon.
    let t = m.trajectory();
    (t[0].0 - m.simulation().translation).flat().length() * 3.0
}

fn torso_of(db: &Database) -> Vec<usize> {
    ["Hips", "Spine2", "Neck", "Head"]
        .iter()
        .filter_map(|n| loadngo_animation::humanoid::find_bone(&db.skeleton, n))
        .collect()
}

fn pct(v: &mut [u64], p: f64) -> u64 {
    if v.is_empty() {
        return 0;
    }
    v.sort_unstable();
    v[((v.len() as f64 * p) as usize).min(v.len() - 1)]
}

/// World positions of the database skeleton at `frame`, played as recorded.
fn db_pose(db: &Database, frame: usize, out: &mut [Vec3]) -> (Vec3, f32) {
    let b = db.bone_count();
    let root = Transform::new(db.root_pos[frame], db.root_rot[frame]);
    let hips = root.compose(Transform::new(db.hips_pos[frame], db.rotations[frame * b]));
    let mut rot = vec![hips.rotation; b];
    out[0] = hips.translation;
    for i in 1..b {
        let p = db.skeleton.parents[i].unwrap_or(0);
        out[i] = out[p] + rot[p].rotate(db.skeleton.rest[i].translation);
        rot[i] = rot[p] * db.rotations[frame * b + i];
    }
    (root.translation, root.rotation.yaw())
}

/// A stretch of an original (unmirrored) clip walking straight at about `pace`.
fn capture_segment(db: &Database, pace: f32, frames: usize) -> Option<usize> {
    let w = (0.6 * db.fps) as usize;
    let mut best: Option<(f32, usize)> = None;
    for c in db.clips.iter().filter(|c| !c.mirrored) {
        if c.end - c.start < frames + 2 * w + 2 {
            continue;
        }
        let mut f = c.start + w;
        while f + frames + w < c.end {
            let speed = (db.root_pos[f + w] - db.root_pos[f - w]).flat().length()
                / (2.0 * w as f32 / db.fps);
            let end_speed = (db.root_pos[f + frames + w] - db.root_pos[f + frames - w])
                .flat()
                .length()
                / (2.0 * w as f32 / db.fps);
            let turn = loadngo_animation::math::wrap_angle(
                db.root_rot[f + frames].yaw() - db.root_rot[f].yaw(),
            )
            .abs();
            let err = (speed - pace).abs() + (end_speed - pace).abs() + turn;
            if best.is_none_or(|(e, _)| err < e) {
                best = Some((err, f));
            }
            f += 5;
        }
    }
    best.map(|(_, f)| f)
}

/// A pose to draw, owned.
struct Owned {
    pos: Vec<Vec3>,
    root: Vec3,
    facing: f32,
    label: String,
    on_floor: [bool; 2],
}

fn panels(owned: &[Owned]) -> Vec<PanelPose<'_>> {
    owned
        .iter()
        .map(|o| PanelPose {
            positions: &o.pos,
            root: o.root,
            facing: o.facing,
            label: o.label.clone(),
            on_floor: o.on_floor,
        })
        .collect()
}

fn floor_flags(db: &Database, p: &[Vec3]) -> [bool; 2] {
    let h = db.humanoid;
    let low = |leg: usize| {
        let l = h.legs[leg];
        p[l.ankle].y - db.foot_heights[0] < 0.02 || p[l.toe].y - db.foot_heights[1] < 0.02
    };
    [low(0), low(1)]
}

#[allow(clippy::too_many_lines)]
fn strips(
    proactor: &Proactor<PlatformPort>,
    db: &Database,
    s: &Scenario,
    r: &Run,
    out: &Path,
) -> Result<(), String> {
    let hz = r.rec.hz;
    let n = r.rec.frames();
    let side = strip::sides(&db.skeleton.names);
    let parents = &db.skeleton.parents;
    let feet = [db.humanoid.legs[0].ankle, db.humanoid.legs[1].ankle];
    // Which stretch to show.
    let walk_start = {
        let mut t = 0.0;
        let mut found = 0.0;
        for l in &s.legs {
            match l {
                Leg::Wait(w) => t += w,
                Leg::Walk(..) | Leg::Sit(..) | Leg::Stand => {
                    found = t;
                    break;
                }
            }
        }
        found
    };
    let (from, step, count): (f32, f32, usize) = match (s.name.as_str(), s.steady) {
        ("start-stop", _) => (walk_start - 0.2, 0.3, 30),
        _ if s.uses_seats() => (walk_start, (s.seconds - walk_start) / 45.0, 45),
        (_, Some((t0, _))) => (t0 + 1.0, 0.1, 14),
        _ => {
            // Around the turn: when the facing changes fastest.
            let mut best = (0.0f32, 0usize);
            for f in 30..n.saturating_sub(30) {
                let d = loadngo_animation::math::wrap_angle(
                    r.rec.root[f + 15].1 - r.rec.root[f - 15].1,
                )
                .abs();
                if d > best.0 {
                    best = (d, f);
                }
            }
            (best.1 as f32 / hz - 1.4, 0.2, 15)
        }
    };
    let panel_w = 120.0;
    let panel_h = 300.0;
    let px = 130.0;
    let per_row = 15usize;
    let rows_result = count.div_ceil(per_row);
    let compare = s.steady.is_some();
    let rows = rows_result + usize::from(compare);
    let top_h = 420.0;
    let width = panel_w * per_row as f32 + 40.0;
    let height = 60.0 + rows as f32 * (panel_h + 26.0) + top_h + 40.0;
    let mut c = Canvas::new(width as u32, height as u32);
    c.text(
        20.0,
        14.0,
        &format!("{}: {}", s.name, s.about),
        2,
        strip::INK,
    );
    let mut y = 50.0;
    let sample = |t: f32, label: String, r: &Run| -> Option<Owned> {
        let f = (t * hz).round() as usize;
        if f >= n {
            return None;
        }
        let pos: Vec<Vec3> = (0..r.rec.bones).map(|b| r.rec.at(f, b)).collect();
        let (root, facing) = r.rec.root[f];
        let on_floor = floor_flags(db, &pos);
        Some(Owned {
            pos,
            root,
            facing,
            label,
            on_floor,
        })
    };
    for row in 0..rows_result {
        let owned: Vec<Owned> = (row * per_row..((row + 1) * per_row).min(count))
            .filter_map(|i| {
                let t = from + step * i as f32;
                sample(t, format!("{t:.1}s"), r)
            })
            .collect();
        c.text(20.0, y, if row == 0 { "motion matching (this runtime), side-on from the right; blue = left, orange = right" } else { "" }, 1, strip::INK);
        strip::side_row(
            &mut c,
            20.0,
            y + 10.0,
            panel_w,
            panel_h,
            px,
            parents,
            &side,
            feet,
            &panels(&owned),
        );
        y += panel_h + 26.0;
    }
    if compare {
        if let Some(f0) = capture_segment(db, s.pace, (count as f32 * step * db.fps) as usize) {
            let mut buf = vec![Vec3::ZERO; db.bone_count()];
            let mut owned = Vec::new();
            for i in 0..count {
                let f = f0 + (i as f32 * step * db.fps) as usize;
                let (root, facing) = db_pose(db, f, &mut buf);
                let on_floor = floor_flags(db, &buf);
                owned.push(Owned {
                    pos: buf.clone(),
                    root,
                    facing,
                    label: format!("{:.1}s", i as f32 * step),
                    on_floor,
                });
            }
            let clip = db.clip_of(f0).map_or("?", |c| c.name.as_str());
            c.text(20.0, y, &format!("the capture itself at the same pace, retargeted, played as recorded ({clip}, frame {f0})"), 1, strip::INK);
            strip::side_row(
                &mut c,
                20.0,
                y + 10.0,
                panel_w,
                panel_h,
                px,
                parents,
                &side,
                feet,
                &panels(&owned),
            );
        }
        y += panel_h + 26.0;
    }
    // Top view.
    let planned: Vec<Vec3> = s
        .legs
        .iter()
        .filter_map(|l| {
            if let Leg::Walk(p, _) = l {
                Some(p.clone())
            } else {
                None
            }
        })
        .flatten()
        .collect();
    let root: Vec<Vec3> = r.rec.root.iter().map(|x| x.0).collect();
    let lp = db.humanoid.legs;
    let mut prints = Vec::new();
    for (side_i, leg) in lp.iter().enumerate() {
        let fa = eval::floor_height(&r.rec, leg.ankle);
        for ct in eval::contacts(&r.rec, leg.ankle, fa, eval::FLOOR_BAND, side_i == 0, false) {
            prints.push((
                side_i == 0,
                r.rec.at(ct.start, leg.ankle),
                r.rec.at(ct.start, leg.toe),
            ));
        }
    }
    c.text(20.0, y, "from above: planned path (green), simulation (grey), root (black), footprints at touch-down; 1 m grid", 1, strip::INK);
    strip::top_view(
        &mut c,
        20.0,
        y + 12.0,
        width - 40.0,
        top_h,
        &TopView {
            planned: &planned,
            simulation: &r.sim,
            root: &root,
            prints: &prints,
        },
    );
    let png = c.png()?;
    io::write_file(proactor, &out.join(format!("{}.png", s.name)), &png).map_err(|e| e.to_string())
}

/// Recorded joint tracks, judged by the same measures and drawn the same way.
fn judge_tracks(
    proactor: &Proactor<PlatformPort>,
    path: &Path,
    out: &Path,
    draw: bool,
) -> Result<(String, serde_json::Value), String> {
    use loadngo_animation::skeleton::Skeleton;
    let bytes = io::read_file(proactor, path).map_err(|e| e.to_string())?;
    let v: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|e| format!("tracks JSON: {e}"))?;
    let strs = |k: &str| -> Vec<String> {
        v.get(k)
            .and_then(serde_json::Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    let name = v
        .get("name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("tracks")
        .to_string();
    let about = v
        .get("about")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("recorded tracks")
        .to_string();
    let hz = v
        .get("hz")
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(60.0) as f32;
    let names = strs("names");
    let parents: Vec<Option<usize>> = v
        .get("parents")
        .and_then(serde_json::Value::as_array)
        .map(|a| {
            a.iter()
                .map(|x| x.as_i64().filter(|p| *p >= 0).map(|p| p as usize))
                .collect()
        })
        .unwrap_or_default();
    let skeleton = Skeleton::new(
        names.clone(),
        parents.clone(),
        vec![loadngo_animation::math::Transform::IDENTITY; names.len()],
    )
    .map_err(|e| format!("tracks: {e}"))?;
    let humanoid = loadngo_animation::humanoid::Humanoid::find(&skeleton)
        .map_err(|e| format!("tracks: {e}"))?;
    let triple = |x: &serde_json::Value| -> Option<[f32; 3]> {
        let a = x.as_array()?;
        Some([
            a.first()?.as_f64()? as f32,
            a.get(1)?.as_f64()? as f32,
            a.get(2)?.as_f64()? as f32,
        ])
    };
    let pos: Vec<[f32; 3]> = v
        .get("positions")
        .and_then(serde_json::Value::as_array)
        .map(|a| a.iter().filter_map(triple).collect())
        .unwrap_or_default();
    let root: Vec<[f32; 3]> = v
        .get("root")
        .and_then(serde_json::Value::as_array)
        .map(|a| a.iter().filter_map(triple).collect())
        .unwrap_or_default();
    let n = names.len();
    if n == 0 || pos.len() != root.len() * n || root.is_empty() {
        return Err(format!(
            "{} positions for {} joints and {} frames",
            pos.len(),
            n,
            root.len()
        ));
    }
    let mut rec = Recording::new(hz, n);
    let mut frame = vec![Vec3::ZERO; n];
    for (f, r) in root.iter().enumerate() {
        for (j, p) in frame.iter_mut().enumerate() {
            let q = pos[f * n + j];
            *p = Vec3::new(q[0], q[1], q[2]);
        }
        rec.push(&frame, Vec3::new(r[0], 0.0, r[1]), r[2], 0);
    }
    let steady = v
        .get("steady")
        .and_then(serde_json::Value::as_array)
        .and_then(|a| Some((a.first()?.as_f64()? as f32, a.get(1)?.as_f64()? as f32)));
    let (s0, s1) = steady.map_or((0, rec.frames()), |(a, b)| {
        ((a * hz) as usize, (b * hz) as usize)
    });
    let torso: Vec<usize> = ["Hips", "Spine2", "Neck", "Head"]
        .iter()
        .filter_map(|t| skeleton.find(t))
        .collect();
    let m = eval::measure(&rec, &humanoid, &torso, (s0, s1));
    eprintln!(
        "tracks {:<12} slide {:.2}/{:.2} cm, acc {:.2}/{:.2}, jog {:.2}, cadence {:.1} x {:.3} m at {:.2} m/s, sway {:.1} cm, knee {:.1}/{:.1}",
        name, m.slide_worst_cm, m.slide_p95_cm, m.torso_acc_cm, m.limb_acc_cm, m.jog_cm, m.cadence, m.step_length, m.speed, m.hip_sway_cm, m.knee_at_strike_deg, m.knee_swing_deg
    );
    if draw {
        let side = strip::sides(&names);
        let feet = [humanoid.legs[0].ankle, humanoid.legs[1].ankle];
        let panel_w = 120.0;
        let mut c = Canvas::new((panel_w * 15.0 + 40.0) as u32, 60 + 326 + 470);
        c.text(20.0, 14.0, &format!("{name}: {about}"), 2, strip::INK);
        let floors: Vec<f32> = humanoid
            .legs
            .iter()
            .flat_map(|l| {
                [
                    eval::floor_height(&rec, l.ankle),
                    eval::floor_height(&rec, l.toe),
                ]
            })
            .collect();
        let owned: Vec<Owned> = (0..14)
            .filter_map(|i| {
                let t = s0 as f32 / hz + 1.0 + 0.1 * i as f32;
                let f = (t * hz).round() as usize;
                (f < rec.frames()).then(|| {
                    let p: Vec<Vec3> = (0..n).map(|b| rec.at(f, b)).collect();
                    let low = |k: usize| {
                        let l = humanoid.legs[k];
                        p[l.ankle].y - floors[2 * k] < eval::FLOOR_BAND
                            || p[l.toe].y - floors[2 * k + 1] < eval::FLOOR_BAND
                    };
                    let on_floor = [low(0), low(1)];
                    Owned {
                        pos: p,
                        root: rec.root[f].0,
                        facing: rec.root[f].1,
                        label: format!("{t:.1}s"),
                        on_floor,
                    }
                })
            })
            .collect();
        c.text(
            20.0,
            50.0,
            "recorded tracks, side-on from the right; blue = left, orange = right",
            1,
            strip::INK,
        );
        strip::side_row(
            &mut c,
            20.0,
            60.0,
            panel_w,
            300.0,
            130.0,
            &parents,
            &side,
            feet,
            &panels(&owned),
        );
        let rootp: Vec<Vec3> = rec.root.iter().map(|x| x.0).collect();
        let mut prints = Vec::new();
        for (k, leg) in humanoid.legs.iter().enumerate() {
            for ct in eval::contacts(
                &rec,
                leg.ankle,
                floors[2 * k],
                eval::FLOOR_BAND,
                k == 0,
                false,
            ) {
                prints.push((
                    k == 0,
                    rec.at(ct.start, leg.ankle),
                    rec.at(ct.start, leg.toe),
                ));
            }
        }
        c.text(
            20.0,
            388.0,
            "from above: root (black), footprints at touch-down; 1 m grid",
            1,
            strip::INK,
        );
        strip::top_view(
            &mut c,
            20.0,
            400.0,
            panel_w * 15.0,
            440.0,
            &TopView {
                planned: &[],
                simulation: &[],
                root: &rootp,
                prints: &prints,
            },
        );
        io::write_file(proactor, &out.join(format!("{name}.png")), &c.png()?)
            .map_err(|e| e.to_string())?;
    }
    let row = format!(
        "| {} (recorded) | {:.2} / {:.2} | {:.2} / {:.2} | {:.2} | {:.1} | {:.3} | {:.2} | {:.1} | {:.1} / {:.1} | - | - | - |\n",
        name, m.slide_worst_cm, m.slide_p95_cm, m.torso_acc_cm, m.limb_acc_cm, m.jog_cm, m.cadence, m.step_length, m.speed, m.hip_sway_cm, m.knee_at_strike_deg, m.knee_swing_deg
    );
    Ok((
        row,
        serde_json::json!({ "tracks": name, "about": about, "metrics": metrics_json(&m) }),
    ))
}

fn metrics_json(m: &Metrics) -> serde_json::Value {
    serde_json::json!({
        "slide_worst_cm": m.slide_worst_cm, "slide_p95_cm": m.slide_p95_cm, "contacts": m.contacts,
        "torso_acc_cm": m.torso_acc_cm, "limb_acc_cm": m.limb_acc_cm, "jog_cm": m.jog_cm, "jerk_rms_cm": m.jerk_rms_cm,
        "cadence_steps_per_min": m.cadence, "step_length_m": m.step_length, "speed_m_s": m.speed, "steps": m.steps,
        "hip_sway_cm": m.hip_sway_cm, "knee_at_strike_deg": m.knee_at_strike_deg, "knee_swing_deg": m.knee_swing_deg,
        "transitions": m.transitions, "transitions_per_s": m.transitions_per_s,
    })
}

#[allow(clippy::too_many_lines)]
fn main() {
    let a = parse();
    let proactor = loadngo_proactor::new_platform_proactor()
        .unwrap_or_else(|e| fail(&format!("proactor: {e}")));
    let bytes = io::read_file(&proactor, &a.db)
        .unwrap_or_else(|e| fail(&format!("{}: {e}", a.db.display())));
    let t_load = Instant::now();
    let db = Arc::new(Database::from_bytes(&bytes).unwrap_or_else(|e| fail(&e.to_string())));
    let load_ms = t_load.elapsed().as_secs_f64() * 1000.0;
    std::fs::create_dir_all(&a.out).unwrap_or_else(|e| fail(&format!("{}: {e}", a.out.display())));
    if a.capture {
        let mut buf = vec![Vec3::ZERO; db.bone_count()];
        for c in db.clips.iter().filter(|c| !c.mirrored) {
            let mut rec = Recording::new(db.fps, db.bone_count());
            for f in c.start..c.end {
                let (root, facing) = db_pose(&db, f, &mut buf);
                rec.push(&buf, root, facing, 0);
            }
            let m = eval::measure(&rec, &db.humanoid, &torso_of(&db), (0, rec.frames()));
            let mut worst: Vec<eval::Contact> = Vec::new();
            for side_i in 0..2 {
                worst.extend(eval::foot_contacts(&rec, &db.humanoid, side_i));
            }
            worst.sort_by(|x, y| y.slide.total_cmp(&x.slide));
            let mut slides: Vec<f32> = worst.iter().map(|w| w.slide * 100.0).collect();
            slides.sort_by(f32::total_cmp);
            let median = slides.get(slides.len() / 2).copied().unwrap_or(0.0);
            eprintln!(
                "capture {:<14} slide worst {:.2} p95 {:.2} median {:.2} cm over {} contacts; acc {:.2}/{:.2}; cadence {:.1} x {:.3} at {:.2}; sway {:.1}; knee {:.1}/{:.1}",
                c.name, m.slide_worst_cm, m.slide_p95_cm, median, m.contacts, m.torso_acc_cm, m.limb_acc_cm, m.cadence, m.step_length, m.speed, m.hip_sway_cm, m.knee_at_strike_deg, m.knee_swing_deg
            );
            for w in worst.iter().take(3) {
                eprintln!(
                    "    {} {} at {:.2}s for {} frames: {:.1} cm",
                    if w.left { "left" } else { "right" },
                    if w.ball { "toe" } else { "ankle" },
                    w.start as f32 / db.fps,
                    w.frames,
                    w.slide * 100.0
                );
            }
        }
    }
    // The standard scenarios, or with --path only the paths given (and any --scenario named).
    let mut all = if a.paths.is_empty() || !a.only.is_empty() {
        scenarios::standard()
    } else {
        Vec::new()
    };
    let mut path_names = Vec::new();
    for p in &a.paths {
        let bytes =
            io::read_file(&proactor, p).unwrap_or_else(|e| fail(&format!("{}: {e}", p.display())));
        let sc =
            Scenario::from_json(&bytes).unwrap_or_else(|e| fail(&format!("{}: {e}", p.display())));
        path_names.push(sc.name.clone());
        all.push(sc);
    }
    let chosen: Vec<&Scenario> = all
        .iter()
        .filter(|s| a.only.is_empty() || a.only.contains(&s.name) || path_names.contains(&s.name))
        .collect();
    if chosen.is_empty() && a.tracks.is_empty() {
        fail("no scenario matched --scenario");
    }
    let torso = torso_of(&db);
    let mut report: Vec<serde_json::Value> = Vec::new();
    let mut md = String::new();
    md.push_str(&format!(
        "# motion_eval\n\nDatabase `{}`: {} frames ({:.1} min) at {} fps, {} clips, {} bones; read in {:.1} ms. Steps at {} Hz; foot lock {}.\n\n",
        a.db.file_name().map_or(String::new(), |f| f.to_string_lossy().into_owned()),
        db.frame_count(),
        db.frame_count() as f32 / db.fps / 60.0,
        db.fps,
        db.clips.len(),
        db.bone_count(),
        load_ms,
        a.hz,
        if a.foot_lock { "on" } else { "off" }
    ));
    md.push_str("| scenario | slide worst / p95 (cm) | torso / limb acc (cm/f²) | jog (cm) | cadence (steps/min) | step (m) | speed (m/s) | hip sway (cm) | knee at strike / swing (°) | transitions (/s) | step µs p50 / p99 / max | allocs |\n|---|---|---|---|---|---|---|---|---|---|---|---|\n");
    let mut all_ns: Vec<u64> = Vec::new();
    let mut search_ns: Vec<u64> = Vec::new();
    let mut plain_ns: Vec<u64> = Vec::new();
    for t in &a.tracks {
        let (row, json) = judge_tracks(&proactor, t, &a.out, a.strips)
            .unwrap_or_else(|e| fail(&format!("{}: {e}", t.display())));
        md.push_str(&row);
        report.push(json);
    }
    for s in chosen {
        let play = if s.uses_seats() { run_character } else { run };
        let mut r = play(&db, s, a.hz, a.foot_lock, &a.settings);
        for _ in 1..a.repeat {
            let again = play(&db, s, a.hz, a.foot_lock, &a.settings);
            r.step_ns.extend_from_slice(&again.step_ns);
            r.searched.extend_from_slice(&again.searched);
            r.allocs += again.allocs;
        }
        let steady = s.steady.map_or((0, 0), |(t0, t1)| {
            ((t0 * a.hz) as usize, (t1 * a.hz) as usize)
        });
        let m = eval::measure(&r.rec, &db.humanoid, &torso, steady);
        for (ns, sr) in r.step_ns.iter().zip(&r.searched) {
            if *sr {
                search_ns.push(*ns);
            } else {
                plain_ns.push(*ns);
            }
        }
        all_ns.extend_from_slice(&r.step_ns);
        let mut worst: Vec<eval::Contact> = Vec::new();
        for side_i in 0..2 {
            worst.extend(eval::foot_contacts(&r.rec, &db.humanoid, side_i));
        }
        worst.sort_by(|x, y| y.slide.total_cmp(&x.slide));
        let worst_json: Vec<serde_json::Value> = worst
            .iter()
            .take(5)
            .map(|c| {
                let clip = r.db_frame.get(c.start).and_then(|f| db.clip_of(f.0)).map_or(String::new(), |k| {
                    format!("{}{}", k.name, if k.mirrored { "-m" } else { "" })
                });
                serde_json::json!({ "side": if c.left { "left" } else { "right" }, "joint": if c.ball { "toe" } else { "ankle" }, "at_s": c.start as f32 / a.hz, "frames": c.frames, "slide_cm": c.slide * 100.0, "clip": clip })
            })
            .collect();
        if std::env::var_os("MOTION_EVAL_VERBOSE").is_some() {
            // The largest accelerations, and the clip playing there.
            let mut accs: Vec<(f32, usize, usize)> = Vec::new();
            for bone in 0..r.rec.bones {
                for f in 1..r.rec.frames() - 1 {
                    let acc = (r.rec.at(f + 1, bone) - r.rec.at(f, bone) * 2.0
                        + r.rec.at(f - 1, bone))
                    .length()
                        * 100.0;
                    accs.push((acc, f, bone));
                }
            }
            accs.sort_by(|x, y| y.0.total_cmp(&x.0));
            for (acc, f, bone) in accs.iter().take(4) {
                let clip = db.clip_of(r.db_frame[*f].0).map_or("?".to_string(), |c| {
                    format!("{}{}", c.name, if c.mirrored { "-m" } else { "" })
                });
                eprintln!(
                    "  acc {acc:.2} cm at {:.2}s {} playing {clip} (transitions {}->{})",
                    *f as f32 / a.hz,
                    db.skeleton.names[*bone],
                    r.rec.transitions[f - 1],
                    r.rec.transitions[f + 1]
                );
            }
            eprintln!(
                "  worst contacts: {}",
                serde_json::to_string(&worst_json).unwrap_or_default()
            );
        }
        let mut ns = r.step_ns.clone();
        let (p50, p99, max) = (
            pct(&mut ns, 0.5),
            pct(&mut ns, 0.99),
            *ns.last().unwrap_or(&0),
        );
        let end = r.rec.root.last().map_or(Vec3::ZERO, |x| x.0);
        let goal = s
            .legs
            .iter()
            .rev()
            .find_map(|l| {
                if let Leg::Walk(p, _) = l {
                    p.last().copied()
                } else {
                    None
                }
            })
            .unwrap_or(Vec3::ZERO);
        eprintln!(
            "{:<15} slide {:.2}/{:.2} cm, acc {:.2}/{:.2}, jog {:.2}, cadence {:.1} x {:.3} m at {:.2} m/s, sway {:.1} cm, knee {:.1}/{:.1}, trans {} ({:.2}/s), end off {:.2} m, step {:.1}/{:.1}/{:.1} us, allocs {}",
            s.name, m.slide_worst_cm, m.slide_p95_cm, m.torso_acc_cm, m.limb_acc_cm, m.jog_cm, m.cadence, m.step_length, m.speed,
            m.hip_sway_cm, m.knee_at_strike_deg, m.knee_swing_deg, m.transitions, m.transitions_per_s, (end - goal).flat().length(),
            p50 as f64 / 1e3, p99 as f64 / 1e3, max as f64 / 1e3, r.allocs
        );
        md.push_str(&format!(
            "| {} | {:.2} / {:.2} | {:.2} / {:.2} | {:.2} | {} | {} | {} | {} | {} | {} ({:.2}) | {:.1} / {:.1} / {:.1} | {} |\n",
            s.name,
            m.slide_worst_cm,
            m.slide_p95_cm,
            m.torso_acc_cm,
            m.limb_acc_cm,
            m.jog_cm,
            if m.steps > 0 { format!("{:.1}", m.cadence) } else { "-".into() },
            if m.steps > 0 { format!("{:.3}", m.step_length) } else { "-".into() },
            if s.steady.is_some() { format!("{:.2}", m.speed) } else { "-".into() },
            if s.steady.is_some() { format!("{:.1}", m.hip_sway_cm) } else { "-".into() },
            if m.steps > 0 { format!("{:.1} / {:.1}", m.knee_at_strike_deg, m.knee_swing_deg) } else { "-".into() },
            m.transitions,
            m.transitions_per_s,
            p50 as f64 / 1e3,
            p99 as f64 / 1e3,
            max as f64 / 1e3,
            r.allocs
        ));
        report.push(serde_json::json!({
            "scenario": s.name, "about": s.about, "metrics": metrics_json(&m),
            "end_offset_m": (end - goal).flat().length(), "searches": r.searches, "unlocks_by_radius": r.unlocks,
            "step_us": { "p50": p50 as f64 / 1e3, "p99": p99 as f64 / 1e3, "max": max as f64 / 1e3 }, "allocations_in_step": r.allocs,
            "worst_contacts": worst_json,
        }));
        if a.dump_feet {
            let mut csv = String::from("frame,l_ankle_y,l_toe_y,r_ankle_y,r_toe_y,l_ankle_v,l_toe_v,r_ankle_v,r_toe_v,held,db_frame,contacts,root_v,trans\n");
            let lg = db.humanoid.legs;
            for f in 1..r.rec.frames() {
                let h = |b: usize| r.rec.at(f, b).y;
                let v = |b: usize| (r.rec.at(f, b) - r.rec.at(f - 1, b)).flat().length() * a.hz;
                csv.push_str(&format!(
                    "{f},{:.4},{:.4},{:.4},{:.4},{:.3},{:.3},{:.3},{:.3},{},{},{},{:.3},{}\n",
                    h(lg[0].ankle),
                    h(lg[0].toe),
                    h(lg[1].ankle),
                    h(lg[1].toe),
                    v(lg[0].ankle),
                    v(lg[0].toe),
                    v(lg[1].ankle),
                    v(lg[1].toe),
                    r.held[f],
                    r.db_frame[f].0,
                    r.db_frame[f].1,
                    (r.rec.root[f].0 - r.rec.root[f - 1].0).flat().length() * a.hz,
                    r.rec.transitions[f]
                ));
            }
            io::write_file(
                &proactor,
                &a.out.join(format!("{}-feet.csv", s.name)),
                csv.as_bytes(),
            )
            .unwrap_or_else(|e| fail(&e.to_string()));
        }
        if a.strips {
            strips(&proactor, &db, s, &r, &a.out).unwrap_or_else(|e| fail(&format!("strip: {e}")));
        }
    }
    let (p50, p99) = (pct(&mut all_ns, 0.5), pct(&mut all_ns, 0.99));
    let (s50, s99) = (pct(&mut search_ns, 0.5), pct(&mut search_ns, 0.99));
    let (q50, q99) = (pct(&mut plain_ns, 0.5), pct(&mut plain_ns, 0.99));
    md.push_str(&format!(
        "\nPer step over all scenarios: p50 {:.1} µs, p99 {:.1} µs; steps that searched p50 {:.1} µs, p99 {:.1} µs ({} of {}); steps that did not p50 {:.1} µs, p99 {:.1} µs.\n",
        p50 as f64 / 1e3, p99 as f64 / 1e3, s50 as f64 / 1e3, s99 as f64 / 1e3, search_ns.len(), search_ns.len() + plain_ns.len(), q50 as f64 / 1e3, q99 as f64 / 1e3
    ));
    eprintln!("all steps p50 {:.1} us p99 {:.1} us; searching p50 {:.1} p99 {:.1}; plain p50 {:.1} p99 {:.1}", p50 as f64 / 1e3, p99 as f64 / 1e3, s50 as f64 / 1e3, s99 as f64 / 1e3, q50 as f64 / 1e3, q99 as f64 / 1e3);
    let json = serde_json::json!({
        "tool": "motion_eval", "db": a.db.file_name().map_or(String::new(), |f| f.to_string_lossy().into_owned()), "frames": db.frame_count(), "hz": a.hz, "foot_lock": a.foot_lock,
        "load_ms": load_ms, "scenarios": report,
        "step_us": { "p50": p50 as f64 / 1e3, "p99": p99 as f64 / 1e3, "search_p50": s50 as f64 / 1e3, "search_p99": s99 as f64 / 1e3, "plain_p50": q50 as f64 / 1e3, "plain_p99": q99 as f64 / 1e3 },
    });
    io::write_file(
        &proactor,
        &a.out.join("report.json"),
        serde_json::to_string_pretty(&json)
            .unwrap_or_default()
            .as_bytes(),
    )
    .unwrap_or_else(|e| fail(&e.to_string()));
    io::write_file(&proactor, &a.out.join("report.md"), md.as_bytes())
        .unwrap_or_else(|e| fail(&e.to_string()));
}
