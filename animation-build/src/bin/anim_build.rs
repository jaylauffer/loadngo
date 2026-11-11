//! `anim_build`: builds a motion-matching database from motion capture.
//!
//! Reads capture files (BVH) through loadngo-proactor, parses, trims, resamples and
//! retargets each on a bounded worker lane with thermal admission, builds the database
//! (root trajectory, contacts, features, mirror images) on the same lane, and writes the
//! compact binary file through the proactor. `--help` says how.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use data::cli::{read_args, ArgDoc, Usage};
use loadngo_animation::database::{tags, BuildConfig, Database, SourceClip};
use loadngo_animation::skeleton::Skeleton;
use loadngo_animation_build::clips::{self, ClipJob, ClipOut, MapKind};
use loadngo_animation_build::gltf;
use loadngo_animation_build::io::{self, ReadDone, ReadFile};
use loadngo_animation_build::lane::Lane;
use loadngo_proactor::{CompletionKind, PlatformPort, Proactor};

const USAGE: Usage = Usage {
    bin: "anim_build",
    invocation: "cargo run --release -p loadngo-animation-build --bin anim_build --",
    about: "builds a motion-matching database from BVH motion capture: retargets every clip to a Mixamo-named \
            target skeleton, labels foot contacts, computes matching features and writes the compact database file.",
    args: &[
        ArgDoc::required("--target", "<skeleton.glb|skeleton.gltf|skeleton.json|canonical[:height]>", "the skeleton to retarget to: a glTF skin with Mixamo-style bone names, a skeleton description (see docs/ANIMATION.md), or the built-in humanoid with its hips at <height> m (default 1.0)"),
        ArgDoc::repeated("--source", "<file.bvh|directory>", "capture to import; a directory is searched for .bvh files (a --source or a --manifest is required)"),
        ArgDoc::optional("--manifest", "<clips.json>", "clips to import with their tags, time ranges, skeletons (ASF for AMC motion) and maps; see animation/data/README.md"),
        ArgDoc::required("--out", "<database.lmm>", "where to write the database (written to <out>.partial, then renamed)"),
        ArgDoc::repeated("--include", "<text>", "keep only source files whose path contains one of these (e.g. Neutral_FW)"),
        ArgDoc::repeated("--exclude", "<text>", "drop source files whose path contains any of these"),
        ArgDoc::optional("--tags", "<tag,tag,...>", "with --manifest: keep only clips with any of these tags (walk, idle, turn, start-stop, sit-down, stand-up, seated, talk, gesture, back, man, woman)"),
        ArgDoc::optional("--skip-tags", "<tag,tag,...>", "with --manifest: drop clips with any of these tags (e.g. woman, for a man's body)"),
        ArgDoc::optional("--map", "<auto|100style|cmu|name>", "how source bones map to the target's (default auto, from the joint names)"),
        ArgDoc::optional("--scale", "<auto|number>", "source units to metres (default auto: from the leg length, cm and mm recognised)"),
        ArgDoc::optional("--cuts", "<auto|none|Frame_Cuts.csv>", "frames to keep per clip; auto reads a 100STYLE Frame_Cuts.csv beside the files (default auto)"),
        ArgDoc::optional("--fps", "<number>", "the database's frame rate (default 60)"),
        ArgDoc::optional("--workers", "<n>", "worker threads for parsing and building (default half the cores, at most 6)"),
        ArgDoc::switch("--no-mirror", "do not add each clip's mirror image"),
        ArgDoc::switch("--compact", "rotations in 32 bits (file version 3, at most 0.2 degree of error): a database a fifth smaller"),
        ArgDoc::optional("--smooth", "<seconds>", "Gaussian smoothing of every bone's rotation and the hips' travel, against marker jitter (default 0.02; 0 keeps the capture)"),
        ArgDoc::optional("--weights", "<fp,fv,hv,tp,td>", "feature weights: foot positions, foot velocities, hip velocity, trajectory positions, trajectory directions (default 1.5,1,1,1,1.5)"),
        ArgDoc::optional("--report", "<report.json>", "also write what was built, how, and what admission did"),
        ArgDoc::optional("--dump-target", "<skeleton.json>", "also write the target skeleton as a description"),
    ],
    examples: &[
        "cargo run --release -p loadngo-animation-build --bin anim_build -- --target canonical --source ~/.loadngo/data/mocap/100style/extracted/100STYLE/Neutral --include _FW --include _ID --include _TR1 --out /tmp/walk.lmm --report /tmp/walk.json",
        "cargo run --release -p loadngo-animation-build --bin anim_build -- --target avatar.glb --source cmu/ --map cmu --scale 0.0565 --out avatar.lmm",
    ],
    notes: &[
        "Datasets stay out of git: keep them under ~/.loadngo/data/mocap/<dataset>/ and record their licences in docs/ANIMATION_CREDITS.md.",
        "File payloads go through loadngo-proactor; parsing, retargeting and building run on a bounded lane whose admission follows the platform's thermal pressure (all workers at nominal, half at fair, one at serious, none at critical).",
        "Exit status 0 means the database was written and read back with its hash checked.",
    ],
};

struct Args {
    target: String,
    sources: Vec<PathBuf>,
    manifest: Option<PathBuf>,
    out: PathBuf,
    include: Vec<String>,
    exclude: Vec<String>,
    tags: u32,
    skip_tags: u32,
    map: Option<MapKind>,
    scale: Option<f32>,
    cuts: String,
    fps: f32,
    workers: usize,
    mirror: bool,
    compact: bool,
    smooth: f32,
    weights: [f32; 5],
    report: Option<PathBuf>,
    dump_target: Option<PathBuf>,
}

fn fail(msg: &str) -> ! {
    eprintln!("anim_build: {msg}");
    eprintln!("{}", USAGE.hint());
    std::process::exit(2);
}

fn parse_args() -> Args {
    let argv = read_args(&USAGE, true);
    let cores = std::thread::available_parallelism().map_or(2, std::num::NonZeroUsize::get);
    let mut a = Args {
        target: String::new(),
        sources: Vec::new(),
        manifest: None,
        out: PathBuf::new(),
        include: Vec::new(),
        tags: 0,
        skip_tags: 0,
        exclude: Vec::new(),
        map: None,
        scale: None,
        cuts: "auto".into(),
        fps: 60.0,
        workers: (cores / 2).clamp(1, 6),
        mirror: true,
        compact: false,
        smooth: 0.02,
        weights: BuildConfig::default().weights,
        report: None,
        dump_target: None,
    };
    let mut it = argv.into_iter();
    while let Some(flag) = it.next() {
        let mut value = || {
            it.next()
                .unwrap_or_else(|| fail(&format!("{flag} needs a value")))
        };
        match flag.as_str() {
            "--target" => a.target = value(),
            "--source" => a.sources.push(PathBuf::from(value())),
            "--manifest" => a.manifest = Some(PathBuf::from(value())),
            "--out" => a.out = PathBuf::from(value()),
            "--include" => a.include.push(value()),
            "--exclude" => a.exclude.push(value()),
            "--tags" | "--skip-tags" => {
                let mut bits = 0;
                for t in value().split(',').map(str::trim).filter(|t| !t.is_empty()) {
                    bits |= tags::parse(t)
                        .unwrap_or_else(|| fail(&format!("{flag}: unknown tag {t:?}")));
                }
                if flag == "--tags" {
                    a.tags |= bits;
                } else {
                    a.skip_tags |= bits;
                }
            }
            "--map" => {
                let v = value();
                a.map = MapKind::parse(&v).unwrap_or_else(|| {
                    fail(&format!(
                        "--map {v:?}: expected auto, 100style, cmu or name"
                    ))
                });
            }
            "--scale" => {
                let v = value();
                a.scale = if v == "auto" {
                    None
                } else {
                    Some(
                        v.parse()
                            .ok()
                            .filter(|s: &f32| *s > 0.0)
                            .unwrap_or_else(|| fail("--scale needs a positive number or auto")),
                    )
                };
            }
            "--cuts" => a.cuts = value(),
            "--fps" => {
                a.fps = value()
                    .parse()
                    .ok()
                    .filter(|f: &f32| *f >= 10.0 && *f <= 240.0)
                    .unwrap_or_else(|| fail("--fps needs 10 to 240"))
            }
            "--workers" => {
                a.workers = value()
                    .parse()
                    .ok()
                    .filter(|n: &usize| *n >= 1 && *n <= 64)
                    .unwrap_or_else(|| fail("--workers needs 1 to 64"))
            }
            "--no-mirror" => a.mirror = false,
            "--compact" => a.compact = true,
            "--smooth" => {
                a.smooth = value()
                    .parse()
                    .ok()
                    .filter(|v: &f32| (0.0..=0.2).contains(v))
                    .unwrap_or_else(|| fail("--smooth needs 0 to 0.2 seconds"))
            }
            "--weights" => {
                let v = value();
                let w: Vec<f32> = v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                if w.len() != 5 || w.iter().any(|x| x.is_nan() || *x <= 0.0) {
                    fail("--weights needs five positive numbers separated by commas");
                }
                a.weights.copy_from_slice(&w);
            }
            "--report" => a.report = Some(PathBuf::from(value())),
            "--dump-target" => a.dump_target = Some(PathBuf::from(value())),
            other => fail(&format!("unknown argument {other:?}")),
        }
    }
    if a.target.is_empty() {
        fail("--target is required");
    }
    if a.sources.is_empty() && a.manifest.is_none() {
        fail("at least one --source, or a --manifest, is required");
    }
    if a.out.as_os_str().is_empty() {
        fail("--out is required");
    }
    a
}

fn collect_bvh(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_dir() {
        let Ok(rd) = std::fs::read_dir(path) else {
            return;
        };
        let mut entries: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
        entries.sort();
        for e in entries {
            collect_bvh(&e, out);
        }
    } else if path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("bvh"))
    {
        out.push(path.to_owned());
    }
}

fn load_target(proactor: &Proactor<PlatformPort>, spec: &str) -> Result<Skeleton, String> {
    if let Some(rest) = spec.strip_prefix("canonical") {
        let h = rest
            .strip_prefix(':')
            .map_or(Ok(1.0), str::parse::<f32>)
            .map_err(|_| "canonical:<height> needs a number".to_string())?;
        return Ok(loadngo_animation::testing::humanoid_skeleton(h));
    }
    let path = Path::new(spec);
    let bytes = io::read_file(proactor, path).map_err(|e| format!("{spec}: {e}"))?;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "glb" | "gltf" => gltf::skeleton_from_gltf(&bytes).map_err(|e| format!("{spec}: {e}")),
        "json" => gltf::skeleton_from_json(&bytes).map_err(|e| format!("{spec}: {e}")),
        _ => Err(format!("{spec}: expected .glb, .gltf or .json")),
    }
}

/// `(style, kind)` of a 100STYLE-like file name `Style_KIND.bvh`.
fn style_kind(path: &Path) -> Option<(String, String)> {
    let stem = path.file_stem()?.to_str()?;
    let (s, k) = stem.rsplit_once('_')?;
    Some((s.to_string(), k.to_string()))
}

/// One clip to import.
struct Entry {
    name: String,
    motion: PathBuf,
    skeleton: Option<PathBuf>,
    source_fps: Option<f32>,
    range: Option<(f32, f32)>,
    tags: u32,
    mirror: bool,
    map: Option<MapKind>,
    cut: Option<(usize, usize)>,
    level_head: bool,
}

/// A manifest: `{"root": "~/.loadngo/data/mocap", "level_head": bool, "clips": [{"name",
/// "motion", "skeleton", "fps", "from", "to", "tags": [...], "mirror", "map", "level_head"}]}`.
/// Paths are relative to `root` (`~` is the home directory), or to the manifest's folder
/// without one. A clip's `level_head` overrides the manifest's (default false).
fn read_manifest(bytes: &[u8], path: &Path) -> Result<Vec<Entry>, String> {
    let v: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|e| format!("manifest JSON: {e}"))?;
    let root = match v.get("root").and_then(serde_json::Value::as_str) {
        Some(r) => match r.strip_prefix("~/") {
            Some(rest) => std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default()
                .join(rest),
            None => PathBuf::from(r),
        },
        None => path.parent().map(Path::to_path_buf).unwrap_or_default(),
    };
    let clips = v
        .get("clips")
        .and_then(serde_json::Value::as_array)
        .ok_or("no \"clips\" array")?;
    let level_all = v
        .get("level_head")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let mut out = Vec::with_capacity(clips.len());
    for c in clips {
        let st = |k: &str| c.get(k).and_then(serde_json::Value::as_str);
        let num = |k: &str| {
            c.get(k)
                .and_then(serde_json::Value::as_f64)
                .map(|x| x as f32)
        };
        let motion = st("motion").ok_or("a clip without \"motion\"")?;
        let name = st("name").map_or_else(
            || motion.rsplit('/').next().unwrap_or(motion).to_string(),
            str::to_string,
        );
        let mut bits = 0u32;
        for t in c
            .get("tags")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
        {
            let t = t.as_str().unwrap_or("");
            bits |= tags::parse(t).ok_or_else(|| format!("{name}: unknown tag {t:?}"))?;
        }
        if bits == 0 {
            return Err(format!("{name}: no tags"));
        }
        let map = match st("map") {
            Some(m) => MapKind::parse(m).ok_or_else(|| format!("{name}: unknown map {m:?}"))?,
            None => None,
        };
        let range = match (num("from"), num("to")) {
            (None, None) => None,
            (a, b) => Some((a.unwrap_or(0.0), b.unwrap_or(f32::MAX))),
        };
        out.push(Entry {
            name,
            motion: root.join(motion),
            skeleton: st("skeleton").map(|k| root.join(k)),
            source_fps: num("fps"),
            range,
            tags: bits,
            mirror: c
                .get("mirror")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(true),
            map,
            cut: None,
            level_head: c
                .get("level_head")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(level_all),
        });
    }
    Ok(out)
}

enum Job {
    Clip(Box<Result<ClipOut, String>>),
    Built(Box<Result<(Database, Vec<u8>), String>>),
}

#[allow(clippy::too_many_lines)]
fn main() {
    let args = parse_args();
    let started = Instant::now();
    let proactor = loadngo_proactor::new_platform_proactor()
        .unwrap_or_else(|e| fail(&format!("cannot start the proactor: {e}")));
    let target = Arc::new(load_target(&proactor, &args.target).unwrap_or_else(|e| fail(&e)));
    if let Some(p) = &args.dump_target {
        io::write_file(&proactor, p, gltf::skeleton_to_json(&target).as_bytes())
            .unwrap_or_else(|e| fail(&format!("{}: {e}", p.display())));
    }

    let mut files = Vec::new();
    let manifest_entries = args.manifest.as_ref().map(|m| {
        let bytes =
            io::read_file(&proactor, m).unwrap_or_else(|e| fail(&format!("{}: {e}", m.display())));
        let mut e =
            read_manifest(&bytes, m).unwrap_or_else(|e| fail(&format!("{}: {e}", m.display())));
        e.retain(|c| (args.tags == 0 || c.tags & args.tags != 0) && c.tags & args.skip_tags == 0);
        if e.is_empty() {
            fail("--tags and --skip-tags left no clips");
        }
        e
    });
    for s in &args.sources {
        if !s.exists() {
            fail(&format!("{} does not exist", s.display()));
        }
        collect_bvh(s, &mut files);
    }
    files.retain(|f| {
        let p = f.to_string_lossy();
        (args.include.is_empty() || args.include.iter().any(|i| p.contains(i.as_str())))
            && !args.exclude.iter().any(|x| p.contains(x.as_str()))
    });
    if files.is_empty() && manifest_entries.is_none() {
        fail("no .bvh files matched");
    }

    // Frame cuts.
    let mut cuts: Vec<((String, String), (usize, usize))> = Vec::new();
    match args.cuts.as_str() {
        "none" => {}
        "auto" => {
            let mut seen: Vec<PathBuf> = Vec::new();
            for f in &files {
                for dir in f.ancestors().skip(1).take(2) {
                    let csv = dir.join("Frame_Cuts.csv");
                    if csv.is_file() && !seen.contains(&csv) {
                        let bytes = io::read_file(&proactor, &csv)
                            .unwrap_or_else(|e| fail(&format!("{}: {e}", csv.display())));
                        cuts.extend(clips::frame_cuts(&String::from_utf8_lossy(&bytes)));
                        seen.push(csv);
                    }
                }
            }
        }
        csv => {
            let bytes = io::read_file(&proactor, Path::new(csv))
                .unwrap_or_else(|e| fail(&format!("{csv}: {e}")));
            cuts.extend(clips::frame_cuts(&String::from_utf8_lossy(&bytes)));
        }
    }

    let mut entries: Vec<Entry> = files
        .iter()
        .map(|f| {
            let name = f
                .file_stem()
                .map_or_else(|| "clip".to_string(), |s| s.to_string_lossy().into_owned());
            let cut =
                style_kind(f).and_then(|k| cuts.iter().find(|(key, _)| *key == k).map(|(_, c)| *c));
            let tags = if name.ends_with("_ID") {
                tags::IDLE
            } else {
                tags::WALK | tags::TURN | tags::START_STOP
            };
            Entry {
                name,
                motion: f.clone(),
                skeleton: None,
                source_fps: None,
                range: None,
                tags,
                mirror: true,
                map: args.map,
                cut,
                level_head: false,
            }
        })
        .collect();
    entries.extend(manifest_entries.unwrap_or_default());
    let files: Vec<PathBuf> = entries.iter().map(|e| e.motion.clone()).collect();
    let mut skeletons: std::collections::HashMap<PathBuf, Vec<u8>> =
        std::collections::HashMap::new();

    eprintln!(
        "anim_build: {} files, target {} bones, {} workers",
        files.len(),
        target.len(),
        args.workers
    );
    let mut lane: Lane<Job, PlatformPort> = Lane::new(
        args.workers,
        &proactor.handle(),
        loadngo_thermal::platform_provider(),
    );
    let (read_tx, read_rx) = mpsc::channel::<ReadDone>();
    let mut to_read: VecDeque<(usize, PathBuf)> = files.iter().cloned().enumerate().collect();
    let mut reading: Vec<(usize, ReadFile)> = Vec::new();
    let mut ready: VecDeque<(usize, PathBuf, Vec<u8>)> = VecDeque::new();
    let mut clips_out: Vec<Option<ClipOut>> = (0..files.len()).map(|_| None).collect();
    let mut done = 0usize;
    let mut built: Option<(Database, Vec<u8>)> = None;
    let mut build_submitted = false;
    let mut wake_pending = false;
    let read_window = 2usize;
    let mut bytes_read = 0usize;

    while built.is_none() {
        // Start reads while few texts wait for a worker.
        while reading.len() < read_window && ready.len() < lane.workers() {
            let Some((id, path)) = to_read.pop_front() else {
                break;
            };
            match ReadFile::start(&proactor, &path, id, &read_tx) {
                Ok(r) => reading.push((id, r)),
                Err(e) => fail(&format!("{}: {e}", path.display())),
            }
        }
        // Submit parsed work as admission allows.
        while !ready.is_empty() && lane.can_submit() {
            let (id, _path, text) = ready.pop_front().expect("ready is not empty");
            let e = &entries[id];
            let skeleton = e.skeleton.as_ref().map(|sk| {
                skeletons
                    .entry(sk.clone())
                    .or_insert_with(|| {
                        io::read_file(&proactor, sk)
                            .unwrap_or_else(|err| fail(&format!("{}: {err}", sk.display())))
                    })
                    .clone()
            });
            let job = ClipJob {
                name: e.name.clone(),
                text,
                skeleton,
                source_fps: e.source_fps,
                range: e.range,
                tags: e.tags,
                mirror: e.mirror,
                smooth: args.smooth,
                level_head: e.level_head,
                scale: args.scale,
                map: e.map,
                cut: e.cut,
                fps: args.fps,
                target: Arc::clone(&target),
            };
            lane.submit(id, move || Job::Clip(Box::new(clips::run(job))));
        }
        if done == files.len() && !build_submitted && lane.can_submit() {
            let mut list: Vec<SourceClip> = Vec::with_capacity(files.len());
            for c in &mut clips_out {
                if let Some(c) = c.as_mut() {
                    list.push(SourceClip {
                        tags: c.clip.tags,
                        mirror: c.clip.mirror,
                        name: c.clip.name.clone(),
                        local: std::mem::take(&mut c.clip.local),
                    });
                }
            }
            let cfg = BuildConfig {
                fps: args.fps,
                mirror: args.mirror,
                weights: args.weights,
                compact_rotations: args.compact,
                ..BuildConfig::default()
            };
            let t = Arc::clone(&target);
            lane.submit(usize::MAX, move || {
                Job::Built(Box::new(
                    Database::build(&t, &list, &cfg)
                        .map(|db| {
                            let bytes = db.to_bytes();
                            (db, bytes)
                        })
                        .map_err(|e| e.to_string()),
                ))
            });
            build_submitted = true;
        }
        // Wait: on I/O, on a job, or (held back by heat with nothing in flight) on a
        // deadline at the next thermal sample.
        let waiting_on_io = !reading.is_empty();
        let waiting_on_jobs = lane.in_flight() > 0;
        if !waiting_on_io && !waiting_on_jobs {
            if wake_pending {
                // A deadline is already set.
            } else {
                let delay = lane.until_next_sample().max(Duration::from_millis(50));
                proactor
                    .handle()
                    .defer_for(delay, CompletionKind::Timer, 0, |_| {})
                    .unwrap_or_else(|e| fail(&format!("proactor: {e}")));
                wake_pending = true;
            }
        }
        let report = proactor
            .run_once()
            .unwrap_or_else(|e| fail(&format!("proactor: {e}")));
        if report.dispatched_deferred > 0 {
            wake_pending = false;
        }
        while let Ok(d) = read_rx.try_recv() {
            if let Some(pos) = reading.iter().position(|(id, _)| *id == d.id) {
                reading[pos].1.accept(&proactor, d, &read_tx);
                if reading[pos].1.finished() {
                    let (id, r) = reading.swap_remove(pos);
                    let (path, bytes) = r
                        .finish(&proactor)
                        .unwrap_or_else(|e| fail(&format!("reading: {e}")));
                    bytes_read += bytes.len();
                    ready.push_back((id, path, bytes));
                }
            }
        }
        while let Some((id, r)) = lane.take() {
            match r {
                Err(panic) => fail(&format!("a worker panicked: {panic}")),
                Ok(Job::Clip(r)) => match *r {
                    Err(e) => fail(&e),
                    Ok(c) => {
                        eprintln!(
                        "  {:<24} {:>6} frames at {:.0} fps -> {:>6} at {:.0}; scale {} map {:?} ({} bones), legs x{:.3}{}",
                        c.clip.name,
                        c.source_frames,
                        c.source_fps,
                        c.clip.local.len() / target.len(),
                        args.fps,
                        c.scale,
                        c.map,
                        c.mapped,
                        c.position_scale,
                        if c.level_head_deg == [0.0; 2] {
                            String::new()
                        } else {
                            format!(
                                "; head levelled (neck {:.1}, head {:.1} deg off rest)",
                                c.level_head_deg[0], c.level_head_deg[1]
                            )
                        }
                    );
                        clips_out[id] = Some(c);
                        done += 1;
                    }
                },
                Ok(Job::Built(r)) => match *r {
                    Err(e) => fail(&e),
                    Ok(b) => built = Some(b),
                },
            }
        }
    }
    let (db, bytes) = built.expect("built");
    io::write_file(&proactor, &args.out, &bytes)
        .unwrap_or_else(|e| fail(&format!("{}: {e}", args.out.display())));
    // Read it back and check.
    let back = io::read_file(&proactor, &args.out)
        .unwrap_or_else(|e| fail(&format!("{}: {e}", args.out.display())));
    let check = Database::from_bytes(&back)
        .unwrap_or_else(|e| fail(&format!("reading back {}: {e}", args.out.display())));
    let hash = blake3::hash(&back);
    let secs = started.elapsed().as_secs_f64();
    eprintln!(
        "anim_build: {} frames ({:.1} min) in {} clips, {} bones; {} bytes, blake3 {}; {:.1} s; thermal {:?}, held back {}",
        check.frame_count(),
        check.frame_count() as f32 / db.fps / 60.0,
        check.clips.len(),
        check.bone_count(),
        back.len(),
        &hash.to_hex()[..16],
        secs,
        lane.pressure_seen,
        lane.held_back
    );
    if let Some(p) = &args.report {
        let clips_json: Vec<serde_json::Value> = clips_out
            .iter()
            .flatten()
            .map(|c| {
                serde_json::json!({
                    "name": c.clip.name, "source_frames": c.source_frames, "source_fps": c.source_fps,
                    "scale": c.scale, "map": format!("{:?}", c.map), "mapped_bones": c.mapped, "leg_scale": c.position_scale,
                    "level_head_deg": c.level_head_deg,
                })
            })
            .collect();
        let report = serde_json::json!({
            "tool": "anim_build", "format_version": loadngo_animation::database::FORMAT_VERSION,
            "out": args.out.file_name().map_or(String::new(), |f| f.to_string_lossy().into_owned()), "bytes": back.len(), "blake3": hash.to_hex().to_string(),
            "frames": check.frame_count(), "fps": check.fps, "clips": check.clips.len(), "bones": check.skeleton.names,
            "mirror": args.mirror, "smooth": args.smooth, "weights": args.weights, "workers": args.workers, "seconds": secs, "bytes_read": bytes_read,
            "thermal_pressures_seen": lane.pressure_seen.iter().map(ToString::to_string).collect::<Vec<_>>(),
            "admission_held_back": lane.held_back, "sources": clips_json,
        });
        io::write_file(
            &proactor,
            p,
            serde_json::to_string_pretty(&report)
                .unwrap_or_default()
                .as_bytes(),
        )
        .unwrap_or_else(|e| fail(&format!("{}: {e}", p.display())));
    }
}
