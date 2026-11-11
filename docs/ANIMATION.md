# Animation: motion matching

Status: round 1, 2026-11-11. A motion-matching runtime and its pipeline, in Rust, for every
loadngo target and for the browser as WebAssembly. Walking, turning, starting, stopping and
standing come from real captured motion (100STYLE's normal walking), retargeted to a
Mixamo-named humanoid. Measured on this Mac and on dolores (below). Sitting, standing up and the
browser host's frame loop are the next rounds.

| Where | What |
| --- | --- |
| `animation/` (`loadngo-animation`) | The core: pure Rust, one dependency (BLAKE3). Builds for every loadngo target and `wasm32-unknown-unknown` |
| `animation/js/motion.js`, `animation/build.sh` | The browser module and its wrapper |
| `animation/bench/` | The browser benchmark page and its server |
| `animation-build/` (`loadngo-animation-build`) | The pipeline tool `anim_build` and the harness `motion_eval`, on loadngo-proactor |
| `animation/evidence/round-1/` | Reports and frame strips |
| `docs/ANIMATION_CREDITS.md` | Data sources and licences |

## Why motion matching

A walk built by tuning procedural layers over a borrowed clip does not look like a person
walking: the cadence, the hips, the knees and the arms all come from different places. Motion
matching plays real capture. Every tenth of a second it looks for the moment in a database of
captured motion whose pose and coming trajectory best fit where the character is and where it
wants to go, and blends into it, keeping the pose continuous and the feet planted.

## The pipeline

```
BVH capture ──read through the proactor──▶ parse, trim, resample, retarget   (worker lane,
(~/.loadngo/data/mocap/)                     one clip a job                    thermal admission)
                                                       │
                                                       ▼
                         mirror, root trajectory, contacts, features, normalise (one job)
                                                       │
                                                       ▼
                                  database.lmm ──written through the proactor──▶ any host
```

`anim_build` does this (run it with `--help` for every flag):

```
cargo run --release -p loadngo-animation-build --bin anim_build -- \
  --target canonical \
  --source ~/.loadngo/data/mocap/100style/extracted/100STYLE/Neutral \
  --include _FW --include _ID --include _TR1 \
  --out walk.lmm --report walk.json
```

### File I/O on the proactor

Every file payload is a positioned read or write on the platform's completion port
(`animation-build/src/io.rs`): io_uring on Linux, IOCP on Windows (files opened overlapped),
kqueue on macOS and iOS, epoll on Android. Each file is registered with the port while in use and
released before it closes, as `weights::reader` does. A whole file's reads are submitted before
the first completion is awaited; short reads resume. A database is written to `<out>.partial`,
synced and renamed, then read back and its hash checked. Opening, sizing and renaming are plain
blocking calls (metadata, not payload).

### The worker lane and thermal admission

Parsing and retargeting a clip, and building the database, are CPU jobs on a bounded lane
(`animation-build/src/lane.rs`): a fixed number of threads (default half the cores, at most 6),
at most that many jobs in flight, results posted back as proactor completions so the main thread
blocks in the port for I/O and job results alike. Before each submission the lane asks the
platform's thermal provider through `ThermalGovernor`, no more often than the pressure's
sampling interval, and admits all workers at nominal or unavailable pressure, half at fair, one
at serious and none at critical; held back with nothing in flight, the main thread waits on a
proactor deadline at the next sample rather than spinning. The run's report lists the pressures
seen and how often admission held a job back. (`host_desktop::offload` suits a GUI host, not a
command-line tool: it queues without limit. The Archive CAS tools are growing a similar bounded
lane; the two should become one shared lane once that lands.)

The whole Neutral build, 12 MB of BVH to a 9 MB database of 28,978 frames, takes 0.5 s on the
M4 Pro and peaks at 99 MB resident.

### Importing BVH

`animation/src/bvh.rs` reads the hierarchy, offsets, channels (in any order on any joint) and
frames; end sites become leaf bones named `<parent>_End`. Units are guessed from the leg length
(centimetres and millimetres recognised) or given with `--scale`. 100STYLE's `Frame_Cuts.csv`
trims each clip's calibration pose (`--cuts auto`). Clips are resampled to the database's rate
(60 fps).

### Retargeting

`animation/src/retarget.rs`. Each mapped target bone takes its source bone's world-space change
from rest:

```
R_target(t) = D(t) · C · R_target_rest,   D(t) = R_source(t) · R_source_rest⁻¹
```

`C` aligns the two rest poses bone by bone: it turns the target bone's rest direction (towards a
child both skeletons share) onto the source bone's while keeping a world reference axis, so an
A-posed target follows a T-posed source and neither skeleton's local axis conventions matter. A
source whose rest faces elsewhere is first turned to face +Z. Bone lengths are the target's
own rest offsets; the hips' travel is scaled by the ratio of leg lengths, so a shorter body takes
proportionally shorter steps with its feet still planted. Unmapped target bones (fingers, face)
keep their rest rotation relative to their parent. Bone maps: `100style` (Xsens MVN names),
`cmu`, or `name` (Mixamo names on both sides); `auto` picks from the joint names.

The target is a glTF skin with Mixamo-style names (`--target avatar.glb`: the skin holding
`Hips`, at its nodes' own transforms), a skeleton description in JSON (`gltf.rs` documents it;
`--dump-target` writes one), or `canonical[:hips height]`, a built-in 1.75 m humanoid in a T pose.

### The database

`animation/src/database.rs`.

- **Driven bones.** The 22 body bones (`humanoid::BODY_BONES`: hips, three spine bones, neck,
  head, shoulders, arms, forearms, hands, thighs, shins, feet, toes). Fingers, eyes and the face
  are never touched, so a character's morph targets and finger poses stay its own.
- **Floor.** Each clip is levelled so the feet's lowest clearance over their rest heights (10th
  percentile) is the floor.
- **Root.** The hips' projection on the floor and the body's facing (hips and shoulders
  together), each smoothed with a Gaussian (0.3 s and 0.25 s), so the hips' sway stays in the pose.
- **Pose.** The hips relative to the root, every other bone's rotation relative to its parent.
- **Contacts.** Heel (ankle) and ball (toe joint) of each foot: within 4.5 cm of the joint's
  rest height and slower than 0.3 m/s over the ground, runs under 5 frames dropped and gaps under
  4 filled.
- **Features** (27 a frame, in the root's frame): both ankles' positions and velocities, the
  hips' velocity, the root's position and facing ⅓, ⅔ and 1 s ahead. Each group is normalised by
  its mean and one spread for the whole group and weighted (default 1.5, 1, 1, 1, 1.5 for foot
  positions, foot velocities, hip velocity, trajectory positions, trajectory directions; the
  foot weight is double Holden's 0.75: in a sweep it cut the worst slides several-fold, while
  2.5 froze a slow walk outright, the pose outweighing the trajectory).
- **Mirror.** Each clip's mirror image is added (left and right swapped, about the delta from
  rest so an asymmetric rest still works), doubling turns and starts on both feet.

**The file** (`.lmm`, version 1, little-endian): magic `LDGOANIM`, version, fps, bone, frame, clip
and feature counts; each bone's name, parent and rest transform; the armature transform; the feet's
rest heights; each clip's range, mirror flag and name; feature means, scales and weights; per
frame the root (`x`, `z`, yaw) and hips position; every rotation as four 16-bit fixed-point
numbers (at most 0.0002 rad of error); contact bytes; features as `f32`; then a BLAKE3 hash of
everything before it. Readers refuse another version, a wrong hash, a short file or trailing bytes.
The Neutral database is 9.0 MB, 7.9 MB gzipped: heavy for a page. Halving it is
straightforward (features as 16-bit, rotations as smallest-three, the search at 30 fps) and is
listed under "What is short".

## The runtime

`MotionMatcher::step(dt, &steering)` (`animation/src/controller.rs`) advances one character.

1. **Simulation.** A critically damped spring moves a simulated root towards the steering's
   desired velocity and facing (half-lives 0.3 s) and predicts it ⅓, ⅔ and 1 s ahead, evaluating
   the steering at the predicted positions, so a path's coming turn shapes the prediction. The
   simulation may lead a lagging character (that lead starts a walk) but never trails it along its
   travel (`simulate_from_character`): a simulation behind the character asks it to stop, it
   stops, the simulation passes, and it lunges after it.
2. **Search.** Every 0.1 s, at once when the desire changes sharply, and when a clip ends: the
   query is the current frame's pose features with the predicted trajectory; the nearest frame
   wins if it beats carrying on by `transition_cost` (0.5), so near-equal frames do not trade
   places. The search (`search.rs`) is exact brute force pruned by bounding boxes of the
   features over runs of 64 and 16 frames, with early-out distances. Frames within 20 of a clip's
   end, and the 20 either side of the current frame, are skipped.
3. **Playback.** The frame advances by `dt` at any rate (poses interpolated between database
   frames), and its root motion moves the character.
4. **Inertialization.** On a transition each bone records its difference from the new frame,
   rotation and angular velocity, hips position and velocity, root velocity and turn rate, and
   the differences decay like a released critically damped spring (half-life 0.1 s). The pose
   and its velocity stay continuous.
5. **Root.** The animation's own root motion, nudged towards the simulation at most 30% as fast
   as the root already moves, never more than 0.2 m or 0.6 rad apart.
6. **Standing.** Once nobody wants to move, the simulation settles onto the character (a few
   centimetres short of a goal is a person standing, not a cue to shuffle), and within 0.45 rad
   of the facing wanted the character keeps its own.
7. **Feet.** Where the database marks a contact, the point on the floor is held where it touched
   down: the ball while it is down, otherwise the heel. Two-bone IK (`ik.rs`) puts the ankle there
   with a soft reach (the last 3% of the leg approached ever more slowly), bending in the leg's
   own plane while it is bent and turning to the body's forward as it straightens, where the
   leg's plane is ill-defined (this cut knee pops from 9.9 to under 5 cm/frame²). A held point
   further than 0.2 m from the animated foot lets go and blends back over 0.1 s, and is not held
   again until that contact ends.

**No allocation.** Every buffer is sized when the matcher is made; `step` allocates nothing,
including searches and transitions. `animation/tests/no_allocation.rs` counts with its own
allocator over 900 steps of walking, turning and standing; `motion_eval` counts inside every
step of every scenario (0 in all).

**Demand-driven.** The runtime has no thread, timer or loop: a host calls `step` when a frame asks
for one, and nothing runs between. `MotionMatcher` exposes what a host needs to decide whether to
keep asking (`stats`, `feet_held`, the root and trajectory); a standing character still breathes,
so idling at a lower frame demand is the host's policy.

**Steering.** `ConstantSteering` (a velocity and facing: a stick or a host that steers itself) and
`PathSteering` (a polyline at a cruising speed, braking to stop at its end, done within 0.12 m of
the end or past it, so an overshoot never turns them back). Every setting is in
`controller::Config`, settable by name (`Config::set`) from tools and from the browser.

## In the browser

`animation/build.sh` builds `animation/pkg/`: `loadngo_animation.wasm` (146 KB, 70 KB gzipped; 128-bit
SIMD; no imports at all) and `motion.js`. The boundary follows web3d's: plain C-ABI functions over
numbers and pointers into the module's memory, no wasm-bindgen.

```js
import { loadMotion, applyPose } from './pkg/motion.js';
const motion = await loadMotion(new URL('./pkg/loadngo_animation.wasm', import.meta.url));
const db = motion.loadDatabase(await (await fetch('walk.lmm')).arrayBuffer());
const who = db.createCharacter(0, 0, 0);       // x, z, facing
who.setPath([0, 0, 0, 6, 6, 6], 0.83);         // x, z pairs; m/s
// on each frame the host asks for:
applyPose(who.step(dt), avatar, bonesInDbOrder);
```

`step(dt)` returns one `Float32Array` view over the character's pose buffer, rewritten in place:
`[rootX, rootZ, facing, feetHeld, hipsX, hipsY, hipsZ]` then `x, y, z, w` for each driven bone (the
hips' rotation relative to the root, the others relative to their parents, in `db.boneNames`
order). Nothing is allocated on either side of the boundary per frame; memory grows only when a
database or character is made, and the wrapper remakes its views then. `applyPose` writes the
buffer into three.js-style objects without importing three; it assumes the hips' parent sits at
the model's root (an armature node with no transform, as Ready Player Me avatars have).

### The browser host's seam (next round)

loadngo-proactor's browser backend (`BrowserHost`, branch `proactor-wasm`) runs one frame handler
per `requestAnimationFrame` callback at a `FrameDemand`. The seam is that frame handler: it calls
`MotionMatcher::step` for each visible character with the frame's `dt`, and the page draws from
the pose buffers in `onFrame`. Characters out of view need not step every frame (a coarser
`FrameDemand`, catching up with one larger `dt`, which the runtime accepts up to 0.1 s). Loading a
database belongs to the host's external-operation path (`complete(id, bytes)`), so the fetch and
the hash check never run inside a frame. When both crates link into one module (the game's), the
`anim_*` exports sit beside the host's; `motion.js` takes any instance that has them.

## Measured

`motion_eval` drives the character (here the canonical humanoid, hips at 1.0 m) through scripted
walks at 60 Hz and measures the output's joint positions, never the database's own labels:

- **Foot slide:** per foot contact, how far the point on the floor moved since it touched down
  (the ball while within 1 cm of its floor height, otherwise the heel; an ankle rolling forward over
  a planted ball as the heel lifts is not a slide). Worst and 95th percentile.
- **Pops:** each joint's largest per-frame acceleration (second difference), torso and limbs
  apart, and the largest out-and-back jog, per 60 Hz frame.
- **Gait** over the steady part: cadence, step length and speed from heel strikes; hips' sway
  about their own line (3rd to 97th percentile); knee bend at heel strike and at its most in each
  swing.
- **Cost** of each step, and allocations inside it.

`--capture` measures the capture itself, played as recorded: the floor any runtime on this data
can reach.

Round 1, this Mac (M4 Pro, macOS, release build; `animation/evidence/round-1/mac/`):

| Scenario | Slide worst / p95 (cm) | Torso / limb acc (cm/f²) | Jog (cm) | Cadence × step at speed | Hip sway (cm) | Knee at strike / swing (°) | Transitions /s |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Straight, 12 m at 0.83 m/s | 4.17 / 3.32 | 0.59 / 3.97 | 1.76 | 86.9 × 0.605 m at 0.86 m/s | 4.4 | 12.7 / 62.8 | 1.65 |
| Start and stop | 7.80 / 7.80 | 0.60 / 3.95 | 1.76 | | | | 2.16 |
| 90° turn left | 7.44 / 4.17 | 0.59 / 3.97 | 1.76 | | | | 1.84 |
| 90° turn right | 4.96 / 4.17 | 0.59 / 5.05 | 1.82 | | | | 2.16 |
| Gentle curve (5 m radius) | 4.17 / 4.17 | 0.59 / 3.97 | 1.73 | 84.2 × 0.618 m at 0.86 m/s | 2.3 | 12.9 / 61.7 | 2.17 |
| Alongside at 0.6 m/s | 4.64 / 4.07 | 0.53 / 4.20 | 1.45 | 113.5 × 0.354 m at 0.64 m/s | 4.7 | 20.3 / 55.9 | 1.88 |
| *The capture itself (Neutral FW)* | *5.24 / 4.55* | *0.16 / 0.97* | | *86.5 × 0.544 m* | | *11.0 / 63.4* | |

Against a real stroll (about 88 steps a minute of 0.55 m at 0.83 m/s; a man's hips sway about
4-5 cm; the knee near straight at heel strike and 60-65° in the swing):

- **Cadence and step:** 86.9 × 0.605 m: the cadence is a stroll's, the steps about 10% long
  (walk ratio 7.0 mm per step a minute against 6.4); the capture itself steps 0.544-0.596 m.
- **Hip sway:** 4.4 cm walking straight. **Knee:** 12.7° at heel strike, as the capture's own
  11.0°; 62.8° in the swing.
- **Slide:** at the capture's own level (its p95 4.55 cm) except single contacts at a start, a
  stop or a turn's end (7-8 cm). The game's sole-vertex measure (median sole vertex on the floor,
  limit 3.25 cm) is not this joint measure; compare in the game.
- **Pops:** torso 0.6 cm/frame² and limbs 4-5 against the game's limits of 2 and 6.6; jogs under
  1.9.
- **Slow walking is the weak case:** at 0.6 m/s the matcher reaches the speed by mixing slow
  stretches of turnarounds with full strides (113 × 0.35 m): the Neutral data holds no steady slow
  walk. See "What is short".

**Cost per step** (all scenarios, `motion_eval --repeat 3`, 60 Hz): p50 2.6 µs, p99 33 µs; steps
that searched p50 5.6 µs, p99 50 µs; 0 allocations. In Chrome 157 on the same Mac
(`animation/evidence/round-1/wasm/`, cross-origin isolated so timing resolves 5 µs): p50 5 µs,
p99 50 µs; 16 characters a frame p50 40 µs, p99 270 µs; loading the 9 MB database with its hash
check 28 ms. On dolores: see `animation/evidence/round-1/dolores/` and the table below.

<!-- dolores -->

## What is short

- **Slow steady walking.** 100STYLE's Neutral style has none: its slow stretches are
  turnarounds. Bounded time-warping (`time_warp`, off) slows faster clips but lengthens the steps
  (79 × 0.64 m at 0.83 m/s), so the fix is data: CMU's slow walks, or a short capture.
- **Stops and starts.** About 14 stops in the data (and their mirrors); a stop overshoots its mark
  by 0.1-0.4 m and the worst contacts of a run are at starts and stops (7-8 cm).
- **Arrival precision.** The character stops where its stopping motion takes it, 0.16-0.42 m from
  the path's end. A game placing someone exactly (before sitting) needs a final placement step.
- **One performer, one style**, an inertial capture (its own foot drift is the floor of these
  numbers).
- **Database size**: 9.0 MB (7.9 MB gzipped) for 8 minutes of motion at 60 fps.
- **Flat ground only**: contacts are held at the floor's height.
- **Judged on stick figures and joint measures here.** The skinned characters, their sole
  vertices and clipping are judged in the game.

## Next

1. **Into a game's motion layer**, first for a character walking beside the player: the
   character steps through `motion.js` (its database built for that body: `anim_build --target
   <avatar>.glb`), and the game's own motion checks judge it against its current walk.
2. **More data:** CMU slow walks, starts, stops and idles; Mesh2Motion's sitting, standing up and
   talking (CC0, glTF: a glTF animation importer).
3. **Sitting down and standing up:** contact-driven sequences against a seat's position, matched
   with the same features plus a target pose.
4. **The browser host:** step characters from `BrowserHost`'s frame handler at a `FrameDemand`,
   and load databases through its external operations.

## Gates

```
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test -p loadngo-animation -p loadngo-animation-build
cargo check -p loadngo-animation -p loadngo-animation-build --target aarch64-apple-ios --all-targets
cargo clippy -p loadngo-animation --target wasm32-unknown-unknown --all-targets -- -D warnings
animation/build.sh
```

and Linux on dolores (`cargo test` of both crates, `anim_build` and `motion_eval` there).

## References

- Simon Clavet, "Motion Matching and The Road to Next-Gen Animation", GDC 2016.
- Daniel Holden, "Code vs Data Driven Displacement" (2021), "Spring-It-On: The Game Developer's
  Spring-Roll-Call" (2021), and the Motion Matching demo accompanying "Learned Motion Matching"
  (Holden, Kanoun, Perepichka, Popa, SIGGRAPH 2020): the feature set, bounding-box search,
  inertialization and foot locking here follow that design.
- David Bollo, "Inertialization: High-Performance Animation Transitions in Gears of War",
  GDC 2018.
