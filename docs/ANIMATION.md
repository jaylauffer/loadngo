# Animation: motion matching and the character framework

Status: round 8, 2026-11-12. A motion-matching runtime, a character framework over it, and their
pipeline, in Rust, for every loadngo target and for the browser as WebAssembly. Walking, turning,
starting, stopping and standing come from real capture (100STYLE's normal walking and idle, CMU's
slow walks, turns, stops and turns in place; Mesh2Motion's turns in place in the full set only);
sitting down on a seat of any height, sitting and standing up from CMU's sits; looking, reaching and
talking beats are layers over it. Everything is retargeted to a Mixamo-named humanoid. Round 1's and
round 2's measurements are kept below; round 3's follow them. Round 3 takes a body over from other
animation without a jump, stands calmly, measures posture against real walking and stores rotations
in 32 bits; round 4 takes over a body that is walking, so a game can start and stop a walk on its
own animation and hand the steady middle to the matcher (see "In a browser game (round 4)"); round
7 measures posture by part of a walk and standing, keeps planted feet from sliding or turning on the
floor, bounds the last unbounded search, and lets a game walk whole walks on the capture again (see
"In a browser game (round 7)"); round 8 aligns the chest along its neck, levels a capture's head, keeps the root's
speed across transitions and eases a released foot's turn (see "In a browser game (round 8)").

| Where | What |
| --- | --- |
| `animation/` (`loadngo-animation`) | The core: pure Rust, one dependency (BLAKE3). Builds for every loadngo target and `wasm32-unknown-unknown` |
| `animation/src/controller.rs` | `MotionMatcher`: search, playback, inertialization, root, foot lock; actions |
| `animation/src/character.rs` | `Character`: commands (walk, face, follow, sit, stand, gesture, look, reach), states and layers |
| `animation/src/asf.rs` | CMU's ASF skeletons and AMC motion |
| `animation/js/motion.js`, `animation/build.sh` | The browser module and its wrapper |
| `animation/bench/` | The browser benchmark page and its server |
| `animation-build/` (`loadngo-animation-build`) | The pipeline tool `anim_build` and the harness `motion_eval`, on loadngo-proactor |
| `animation/data/` | Clip manifests: `round-2.json` (everything, 25 min), `walk-core.json` (a web game's walking, 6-7.5 min) |
| `animation/evidence/round-1/`, `round-2/`, `round-3/` | Each round's reports and frame strips; round 3's real-capture posture (`round-3/mac/posture-capture.txt`) |
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

### Importing CMU's ASF/AMC, and manifests

`animation/src/asf.rs` reads an ASF skeleton (its axes, degrees of freedom and units) and AMC
frames; each bone's rotation is `C · E · C⁻¹` from its axis rotation `C` and the frame's Euler
angles `E` (Rz·Ry·Rx), lengths in metres from `units length`. Bone map `cmu-asf`.

`anim_build --manifest clips.json` takes clips with their own skeletons, rates, time ranges, tags,
mirroring and maps (`animation/data/README.md`); `--tags` and `--skip-tags` keep or drop clips by
tag (`--skip-tags woman` for a man's body). The two manifests in `animation/data/` reproduce the
databases measured here byte for byte.

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

- **Tags.** Each clip's tags (above), with each frame's and each search box's union, so a search
  limited by tag skips whole boxes.

**The file** (`.lmm`, version 2 or 3, little-endian; version 1 still reads): magic `LDGOANIM`, version,
fps, bone, frame, clip and feature counts; each bone's name, parent and rest transform; the
armature transform; the feet's rest heights; each clip's range, mirror flag, tags and name; feature
means, scales and weights; per frame the root (`x`, `z`, yaw) and hips position; every rotation in
48 bits (smallest three, at most 0.0003 rad of error), or in version 3 (`anim_build --compact`,
`BuildConfig::compact_rotations`) 32 bits (the largest component's index in 2 bits, the other three in 10 each, at
most 0.2 degree); contact bytes; features as 16-bit fixed
point; then a BLAKE3 hash of everything before it. Readers refuse another version, a wrong hash, a
short file or trailing bytes. A database built in memory is quantized the same way, so a built one
and a loaded one match exactly.

**Sizes** (60 fps; version 2 is 211 bytes a frame against round 1's 309):

| Database | Motion | File |
| --- | --- | --- |
| Round 1's Neutral (100STYLE FW, ID, TR1) | 8.0 min | 9.0 MB (version 1) |
| `round-2.json`, every clip | 25.2 min | 19.1 MB |
| `round-2.json` at 30 fps | 25.2 min | 9.5 MB (adds pops: the search's poses are coarser) |
| `walk-core.json`, a man's body (`--skip-tags woman`) | 7.5 min | 5.7 MB |
| `walk-core.json`, a woman's body (`--skip-tags man`) | 6.3 min | 4.8 MB |
| Round 3's `walk-core.json`, a man's body, `--compact` | 7.2 min | 4.3 MB (3.1 MB gzipped) |
| Round 3's `walk-core.json`, a woman's body, `--compact` | 6.1 min | 3.6 MB (2.6 MB gzipped) |

32-bit rotations make a database about a fifth smaller (a third smaller gzipped) and move no
measure in `motion_eval` (the matcher's features are stored as before, so it chooses the same frames).

A database is built for one skeleton; bodies of one build (the same rig scaled to the same hips
height, as Ready Player Me and TalkingHead bodies are) can share one, the host scaling floor
distances by the body's own scale.

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
   again until that contact ends. Planted feet (`plant_feet`) are taken up the same way, from
   where they were planted.

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

## The framework

`Character` (`animation/src/character.rs`) is what a game drives: commands in, a pose out. It
owns a `MotionMatcher` and puts three things over it.

**Commands.** `go_to(points, speed, end_facing)`, `steer(velocity, facing)`, `face(facing)`,
`follow(Follow { position, velocity, heading, offset })` (keep beside someone, called each frame),
`idle()`, `sit(Seat { hips, facing })`, `stand()`, `gesture(word_times, strength)`,
`look_at(point)`, `hand_to(side, point)`, `teleport(position, facing)` and `plant_feet(ankles)`.
A command given while seated stands first and then does it. `state()` says where it is:
standing, walking, approaching a seat, sitting down, seated, standing up.

**Search by tag.** Every clip carries tags (`database::tags`: walk, idle, turn, start-stop,
sit-down, stand-up, seated, talk, gesture, man, woman). `Config::search_tags` and `exclude_tags`
limit what the matcher may choose (a woman's body excludes the men's captures; locomotion never
searches the seated stretches).

**Actions.** `MotionMatcher::play(Action { start, end, looping, pin_root, root_target,
hips_height_target })` plays a stretch of the database as it is, with the same inertialized
transition in and out, instead of searching:

- its root is warped onto `root_target` over its length, closed-loop: each step takes a share of
  what is left between where the clip's own motion would end and the target, so it lands there
  whatever the velocity blend carried in (open-loop it missed a seat by 20 cm and 0.3 rad);
- its hips are lifted or lowered to `hips_height_target` (a seat lower or higher than the
  capture's); the foot lock grounds a foot the capture had near the floor against that lift, so
  the feet stay on the floor (they went 16 cm into it);
- a looping hold (seated) takes its lift at once, the hips' inertialization hiding the step, and
  `pin_root` keeps the root still.

**Seats.** `sit(seat)` chooses the sitting-down clip and the seated loop to follow it together:
the sit's seated hips near the seat's height, and the loop's frame the one whose feet, its hips
put on the seat, are nearest where the sit leaves them, with at least 3 s of loop after it (a
short loop shows its seam every time it restarts; clips tagged talk or gesture are not idles).
From the sit's first frame and its root motion it works out where the sit begins (the approach
spot) and walks there, stopping short by what turning round will carry the character on
(0.3 m for a half turn, scaled by the turn); turned, and within 0.35 m once the feet have
settled, it sits, the warp taking up the rest (small corrective steps look worse). `stand()`
picks the rising frame, in the first half second of each stand-up clip, whose feet from the hips
and hips height are nearest the pose held.

**Layers**, each eased in and out, over the matched pose:

- **Beats:** on each word time a small lift and turn of the forearms and a nod, at `strength`;
- **Look-at:** the chest, neck and head turn towards a point, 0.2, 0.35 and 0.45 of the turn,
  within a person's reach;
- **Reach:** two-bone IK puts a wrist at a point (a cup, a table's edge).

**Handing a body over.** A game that animates its own characters can give the body to the
framework for some moves only (a walk) and take it back. `plant_feet(ankles)` takes up the feet
where the other animation left them: the foot lock holds a foot the pose has on the ground there,
and eases one the pose lifts back from there, so nothing slides as one pose fades into the other.
`take_over(root, hips, rotations)` (round 3; `takeOver()` in the browser) goes further: it is given
the other animation's pose itself, starts playback on the database frame whose feet (relative to
the root) are most like it, among frames that stand or set off, and eases the difference away by
inertialization (`Config::takeover_halflife`, longer than a transition's for the first half
second), the feet planted where they stand. The body is never faded from one pose to another.
`take_over_moving(root, hips, rotations, Moving { velocity, ankle_velocities, planted })` (round 4;
`takeOverMoving()` in the browser) takes over a body that is walking: the frame is found among
walking, turning and starting frames on the whole feature set (these feet and their velocities, the
hips' velocity, a straight path on at this velocity), the simulation and the root carry on at that
velocity, the planted feet are held and the others left to the capture, and the pose is eased over
a transition's half-life (over the standing take-over's longer one, a swinging foot was blended
between two strides and landed sliding). Any take-over lets go of where the feet were held before,
and (round 5) finds its frame through the search's bounding boxes, as any search does (a scan of
every frame cost 10-12 ms once on a Raspberry Pi 5).

**Standing calmly** (round 3). A character nobody asks to move searches only `Config::rest_tags`
(idle and start-stop, say) and never `rest_exclude` (turn: a turn on the spot reads as fidgeting),
once it also faces about the way wanted, and (round 4) never carries on a frame outside them: the
carry-on candidate, favoured by the transition cost, had kept a body asked to stand walking on a
transitions clip for seconds; standing, transitions take at least
`Config::rest_halflife` (the pose features hold the feet and hips but not the arms, so a jump between
two idles can swing an arm a long way, and at 0.1 s it whips). A path takes its end's facing within
`Config::face_from` of its end rather than once stopped, so an arrival does not end in a turn on
the spot.

**Arriving exactly** (round 3, off by default). Within `Config::arrive_exact` of a path's end the
simulation stays on the goal and the root is pulled onto it (`arrive_ratio` of its own speed while
walking, at most `arrive_creep` m/s standing). It lands within 2 cm, but a stop's last contacts
then slide 8-21 cm: a game that needs a body on a mark is better off asking for a point short of it
and finishing with its own move (or sitting, whose warp lands exactly).

**Feet onto the floor.** When the foot lock takes a heel or ball it holds it at the floor's height;
a point taken a few centimetres up comes down over `LOCK_SETTLE_HALFLIFE` (0.05 s) rather than in
one frame (at once it pulled a knee 9 cm in a frame).

## In the browser

`animation/build.sh` builds `animation/pkg/`: `loadngo_animation.wasm` (183 KB, 86 KB gzipped;
128-bit SIMD; no imports at all) and `motion.js`. The boundary follows web3d's: plain C-ABI
functions over numbers and pointers into the module's memory, no wasm-bindgen. A character is a
`Character`, so every command is there:

```js
import { loadMotion, applyPose } from './pkg/motion.js';
const motion = await loadMotion(new URL('./pkg/loadngo_animation.wasm', import.meta.url));
const db = motion.loadDatabase(await (await fetch('walk.lmm')).arrayBuffer());
const who = db.createCharacter(0, 0, 0);       // x, z, facing
who.setPath([0, 0, 0, 6, 6, 6], 0.83);         // x, z pairs; m/s
who.sit(hx, hy, hz, facing); who.stand();      // a seat: the seated hips, the way they face
who.lookAt([x, y, z]); who.gesture([0.2, 0.7], 0.6); who.handTo('right', [x, y, z]);
who.plantFeet([lx, lz], [rx, rz]);             // taking a body over from other animation
who.takeOver(x, z, facing, pose);              // ... from its pose, laid out as step()'s (round 3)
who.takeOverMoving(x, z, facing, pose, [vx, vz], [[lvx, lvz], [rvx, rvz]], [leftDown, rightDown]); // walking (round 4)
who.takeOver(x, z, facing, pose, prevPose, dt, [px, pz, pfacing]); // ... keeping its motion: the frame before (round 9)
who.stepBack(0.3, 0.4);                        // a half step back, facing kept (clips tagged back; round 9)
db.clipName(who.frame);                        // which capture is playing, for checks
// on each frame the host asks for:
applyPose(who.step(dt), avatar, bonesInDbOrder);
who.state;                                     // 'standing', 'walking', 'seated', ...
```

`step(dt)` returns one `Float32Array` view over the character's pose buffer, rewritten in place:
`[rootX, rootZ, facing, feetHeld, hipsX, hipsY, hipsZ]` then `x, y, z, w` for each driven bone (the
hips' rotation relative to the root, the others relative to their parents, in `db.boneNames`
order), the layers included. Nothing is allocated on either side of the boundary per frame; memory
grows only when a database or character is made, and the wrapper remakes its views then.
`applyPose` writes the buffer into three.js-style objects without importing three; it assumes the
hips' parent sits at the model's root (an armature node with no transform, as Ready Player Me
avatars have).

A page that bundles its code with Vite cannot `import()` a file from its public folder in
development; fetching `motion.js` as text and importing it from a blob URL works in both
development and a build (the module has no imports of its own).

### The browser host's seam (not taken this round)

loadngo-proactor's browser backend (`BrowserHost`, branch `proactor-wasm`) runs one frame handler
per `requestAnimationFrame` callback at a `FrameDemand`. The seam is that frame handler: it calls
`Character::step` for each visible character with the frame's `dt`, and the page draws from the
pose buffers in `onFrame`. Characters out of view need not step every frame (a coarser
`FrameDemand`, catching up with one larger `dt`, which the runtime accepts up to 0.1 s). Loading a
database belongs to the host's external-operation path (`complete(id, bytes)`), so the fetch and
the hash check never run inside a frame. When both crates link into one module, the `anim_*`
exports sit beside the host's; `motion.js` takes any instance that has them.

Round 2's game does not use it: `BrowserHost` is still on an unmerged branch, and the game's frame
loop is its own (three.js and its stage pace frames and measure them against its performance
floor). The game steps each character from that loop, through its motion layer's existing
`step(dt)`, and loads the databases beside its models before its Ready screen. Moving the game onto
`BrowserHost` is a frame-loop change for the game as a whole, after that branch merges.

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
can reach. `--path` runs a scenario along any polyline, and `--tracks` judges recorded joint
tracks (another system's walk, Mixamo-named) with the same measures and draws them the same way,
so two walks on one body and route compare like for like.

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

**Cost per step** (all scenarios, `motion_eval --repeat 3`, 60 Hz): p50 2.7 µs, p99 34 µs; steps
that searched p50 5.5 µs, p99 57 µs; 0 allocations. In Chrome 157 on the same Mac
(`animation/evidence/round-1/wasm/`, cross-origin isolated so timing resolves 5 µs): p50 5 µs,
p99 50 µs; 16 characters a frame p50 40 µs, p99 270 µs; loading the 9 MB database with its hash
check 28 ms. dolores: `animation/evidence/round-1/dolores/` and the table below.

| Cost of a step (60 Hz) | p50 | p99 | Steps that searched, p99 | 16 characters a frame, p50 / p99 | Database load |
| --- | --- | --- | --- | --- | --- |
| Mac, native (M4 Pro) | 2.7 µs | 34 µs | 57 µs | | 21 ms |
| Mac, Chrome 157, wasm | 5 µs | 50 µs | | 40 / 270 µs | 28 ms |
| dolores, native (Pi 5, Cortex-A76) | 9.6 µs | 131 µs | 153 µs | | |
| dolores, Chromium 156, wasm | 15 µs | 250 µs | | 155 µs / 1.0 ms | 130 ms |

All with 0 allocations in a step. The browser's timer resolves 5 µs (cross-origin isolated); its
largest single steps (0.5 ms on the Mac, 2.5 ms on dolores) are the first ones, before the engine
optimises the module. On dolores `anim_build` built the same database in 0.3-0.5 s with 2 workers,
thermal pressure nominal throughout (the kernel's thermal zone, 49-52 °C). Its file is not
byte-identical to the Mac's (`99700c0f` against `8af43d2d`): the two platforms' maths libraries
round `sin`, `exp` and `atan2` differently in the last bit. Builds on one platform are identical,
and the motion is the same to the measures' precision, except where a near tie between two
frames tips the other way (the left and right turns trade their worst slides, as mirror images
would): read a single run's worst contact as one sample, not a property.

### Round 2 (this Mac, release build; `animation/evidence/round-2/mac/`)

The same measures, two databases on the canonical humanoid: `round-2.json` whole (25 min) and
`walk-core.json` (7.5 min, the set a web game ships). The worst contact of a run is one sample
(see above); the 95th percentile is the steadier number.

| Scenario | Every clip: slide worst / p95 (cm) | Walk set: slide worst / p95 (cm) | Every clip: gait | Walk set: gait |
| --- | --- | --- | --- | --- |
| Straight | 27.75 / 2.47 | 3.15 / 3.15 | 86.9 × 0.602 m at 0.85 m/s, sway 4.4, knee 12.8 / 62.3 | 86.9 × 0.602 m at 0.85, sway 4.3, knee 12.6 / 62.5 |
| Start and stop | 2.42 / 2.38 | 4.82 / 2.29 | | |
| 90° turn left | 25.44 / 13.38 | 3.60 / 2.59 | | |
| 90° turn right | 15.96 / 13.38 | 28.32 / 5.16 | | |
| Gentle curve | 13.38 / 4.78 | 23.94 / 17.88 | 86.9 × 0.612 m at 0.86, sway 4.7 | 88.3 × 0.585 m at 0.84, sway 4.3 |
| Off 80° from standing | 3.61 / 3.61 | 53.93 / 53.93 | | |
| Off 180° from standing | 10.17 / 10.17 | 2.77 / 2.77 | | |
| Alongside at 0.6 m/s | 21.92 / 1.19 | 4.52 / 2.29 | 85.7 × 0.477 m at 0.66 m/s, sway 4.4, knee 12.2 / 61.9 | 87.8 × 0.529 m at 0.74, sway 3.8 |
| Sit on a bench (hips 0.45 m) and stand | 25.69 / 25.69 | | | |
| Sit on a chair facing (0.5 m) and stand | 15.20 / 15.20 | | | |

- **Slow walking fixed by data:** alongside at 0.6 m/s now strolls (85.7 steps a minute of 0.48 m;
  round 1 mixed turnarounds, 113.5 × 0.35 m), from CMU's slow walks.
- **Seats:** the hips land on the seat to the centimetre and at its height (0.44-0.46 m on the
  0.45 m bench, 0.49-0.51 on the 0.5 m chair, seated still); sitting down, seated and standing
  up are frames in `sit-bench.png` and `sit-chair.png` (upright, thighs level, shins upright). The
  slide figures for the seat scenarios count a seated toe as one contact for several seconds; the
  worst is a turn-in-place step before the sit.
- **Where the walk set loses:** single contacts at turns from standing and at a curve, a raw
  capture's toe sweeping the floor (Mesh2Motion's turns) or a turning step from CMU. Which frame
  wins a near tie moves the worst contact from scenario to scenario between databases; the
  medians and the gait do not move.
- **Cost:** p50 2.5 µs and p99 21 µs a step with the walk set (p99 37 µs searching), 45 / 97 µs
  with every clip, 0 allocations in every step, seats and layers included.

### Round 3 (this Mac, release build; `animation/evidence/round-3/mac/`)

Round 3's `walk-core.json` (7.2 min, 32-bit rotations; no Mesh2Motion turns; CMU's standing 77_02, 113_21 and 137_28
added) on the canonical humanoid (`walk-core/report.md`; its sit scenarios have no sits in this set to play):

| Scenario | Slide worst / p95 (cm) | Jog (cm) | Gait |
| --- | --- | --- | --- |
| Straight | 2.64 / 2.51 | 1.82 | 86.9 × 0.602 m at 0.85 m/s, sway 4.4, knee 14.5 / 62.4 |
| Start and stop | 2.64 / 2.64 | 1.94 | |
| 90° turn left / right | 4.10 / 2.65, 5.36 / 3.44 | 1.77, 1.79 | |
| Gentle curve | 2.64 / 1.79 | 1.77 | at 0.84 m/s, sway 4.3 |
| Off 80° / 180° from standing | 3.65 / 3.65, 14.02 / 14.02 | 1.77, 1.34 | |
| Alongside at 0.6 m/s | 17.16 / 5.09 | 1.70 | 87.4 × 0.530 m at 0.73 m/s, sway 3.8 |

Round 2's turn right (28.3 cm) and curve (23.9) are gone with the raw turns; the about-turn from standing and the slow
walk alongside now carry the worst single contacts (one each). Cost: p50 2.4 µs, p99 19.5 µs a step, 0
allocations.

- **Posture of real walking** (`posture-capture.txt`, `motion_eval --capture` over every clip of `round-2.json` as
  recorded, `eval::posture`): the knee at mid-stance (median) 0-25.5° in steady and slow walks, 27.5-27.9 in CMU's slow
  stops, but 22.7-39.1° in CMU subject 16's walks with turns; the elbow's least bend in the swing (10th percentile)
  1.6° (100STYLE) to 31.1 (CMU 91_27, a slow walker); the forearm 5-31° forward of vertical (median); the trunk's lean
  -2.3 to 10.7°; the wrist from 1.5 cm below the hip joint to 9.7 cm above it. A game can hold its own animation to these ranges; the matcher
  plays them as captured.
- **A newly held foot comes down onto the floor over 0.05 s** (above): with a database built for a game's body (its
  feet's rest heights its shoes'), the knee's frame-to-frame jolt went 2.0 -> 0.3 cm walking straight and 1.9 -> 0.6
  starting and stopping; on the canonical humanoid it does not move.
- **Arriving exactly** (`arrive_exact`): lands within 2 cm, but slides the stop's contacts 8-21 cm; left off.
- **Tried and dropped:** turning on the spot before a path that sets off more than 70° off the facing (a
  `turn_first` setting): on a game body the walk set's in-place turns were not chosen, its walking turns were, and a
  toe slid 30 cm; a quicker toe clearance for a matched foot changed nothing measured. Neither is in the code.

### In a browser game (round 2)

A three.js game's people (Ready-Player-Me-style bodies under a talking-head layer, with a motion
layer of their own built on made clips) walk on the framework behind a flag: walks of 2.5 m or
more are matched on `walk-core.json`'s databases (a man's build and a woman's), the body handed
over with its feet planted and handed back at the end; standing up, sitting down and standing
about stay the game's. The game's own checks judge it, unchanged: sole vertices within 8 mm of
the ground may slide at most 3.25 cm, no shoe 1 cm into the ground, nothing 1 cm into furniture,
per-frame limits on every joint's step and acceleration, and the gait.

- **It fails them.** Every run breaks limits, at the hand-overs and the first and last steps: a
  walk that sets off turning 114° from a chair slides 8-17 cm in its first three contacts, a stop
  6-21 cm, and hand-overs in short walks pop the arms and legs (fixed in part by keeping walks
  under 2.5 m on the game's clips and a walk that follows a matched one on the matcher). Between,
  every contact slides at most 3.25 cm. The game's made walk passes every limit.
- **The steady walk:** hips 4.6 cm side to side (the made walk 3.1), 86.7 steps a minute of
  0.61 m at 0.875 m/s (asked 0.81), the knee 68° in the swing and 30° at heel strike, as the made
  walk's: lifting the capture's soles onto the bodies' own shoes bends the knee that much (12.6° on
  the capture's skeleton alone).
- **By eye:** upright, the arms hanging and swinging at the sides; the made walk holds the
  forearms out in front. The game's limits do not measure the arms.
- **What the hand-over needed:** the capture's foot lock alone under the matched body (the game's
  own foot holds over it bent the knees), only the soles lifted onto the shoes' rest heights and a
  moving toe kept clear by its speed (the capture's swing skims the ground at 0.6-1.6 cm), feet
  planted at the hand-over (`plant_feet`), the walk asked to stop 0.3 m short of its end (its stops
  carry on past), and the chest's walking pose eased in with the fade (at once, it jumped the head
  5 cm).
- **Cost:** the game's motion layer per frame p50 0.6 ms, p95 1.1 ms, as before (the matcher's
  step is microseconds; the clips still run under it). Download: 10.6 MB more (two databases and the
  module).

### In a browser game (round 3)

The same game, its checks unchanged but for four posture limits added from real walking (above): the knee at
mid-stance at most 28°, the elbow's least bend at most 31.5°, the trunk's lean at most 10°, the wrist at most 10 cm
above the hip. The body is now taken over by `take_over` (no fade), stands calmly on the matcher after a walk, is
handed back once both feet are down, and the databases load after the game's first playable.

- **Two of the game's twelve runs pass every limit** (worst of three, two tiers), both on the women's database; ten
  do not, at the men's landings (a shoe's heel within 8 mm of the ground for 4-5 frames while the foot still travels,
  counted as 9-21 cm of slide), a turning start from a chair, hand-backs while the body still moves, and the sits that
  follow (see "What is short").
- **Posture:** the game's made walk fails the knee limit in every run long enough to judge (35-37° on the men's
  bodies, 29-30 on the women's) and the men's elbow limit narrowly (32.6°); the matched walk passes them all but one
  run (28.5°). The arm limit separates the two only narrowly; the knee clearly.
- **On a Raspberry Pi 5 at the game's lowest tier** (desktop Chromium, two whole plays each way): the game's frame
  floor holds either way; the motion layer costs the same per frame (p50 1.2, p95 2.0 ms walking); the take-over adds
  one 10 ms motion frame and the walk one 83 ms frame a play.
- **Download:** 6.1 MB over the wire (module 0.18, the two databases 2.7 and 3.2), all after the first playable.

### In a browser game (round 4)

The game's made walk passes its own limits on its foot holds, starts, turns and stops; the matched walk wins on the body.
So the game now walks a hybrid: its own walk sets off (a turn from a chair, the first steps), `take_over_moving` hands
the body to the matcher at the first steady step (mid-stance), and 4.5 m from the end, at the matcher's mid-stance, its
own walk takes the body back walking and stops as it always has. While matched, the game keeps each shoe's sole off the
ground point by point (its outline and tread sampled from the mesh, skinned as the mesh is), raises the body by what the
capture's standing feet need for their shoes, and solves the legs every frame with almost no soft reach.

- **8 of the game's 12 runs pass every limit** (worst of three, both tiers), the four posture limits included; 4 do
  not, each by one limit, in the game's own stop after the hand-back (an arm's jog at 2.12 against 1.9 on one body,
  where the game's walk alone reaches 1.89; a leg's at 2.06 in some repeats sitting down).
- **The landings are solved by the shoe, not the skeleton:** a capture's heel comes down pitched while the foot still
  travels; with the sole sampled from the mesh and kept clear by speed, the 9-21 cm landing slides of round 3 became
  2-4 cm. The standing clip's rest heights had held matched feet 1.3 cm up, so their contacts went unseen.
- **Posture:** the knee at mid-stance 18-25 degrees with the game's ends included (limit 28; the made walk alone 29-37).
- **On a Raspberry Pi 5 at the lowest tier:** the frame floor holds, the walking take-over's search costs 10-12 ms once.

### In a browser game (round 5)

The last four of the game's runs failed in its own stop after the hand-back. Their causes were the game's walk, not the
matcher: a change of direction (turning to face a seat) eased at a rate began at full speed in a frame (now on a
critically damped spring), a foot set down from a swing was stopped in a frame while its clip still moved it (now it
comes to rest over 0.1 s), and a lifted foot's turn unwound from the wrong moment (a 3-4 degree turn in a frame). The
hand-back now waits for a mid-stance where the two walks agree (not turning, the planted foot along the way, the game's
walk with a moment whose feet are within 12 cm of the matched body's) [not so: see round 7's correction]. With the take-over through the bounding boxes, the
game's motion layer costs at most 8-9 ms in a frame on a Raspberry Pi 5 (was 10-12 ms). Every run passes every limit in
every repeat at the game's lowest tier; at its automatic tier one or two repeats in eighteen still fail, in the 0.6 s
the matched body fades out over the game's walk (a foot the game's walk swings is held for a frame, then lifted off
that hold while the blend moves it, and the knee jerks).

### In a browser game (round 7)

The game's sixth critic failed the hybrid walk: real only in its middle. Every start and stop was the game's own walk,
crouched (the knee 34-38 degrees through the stance), its forearms held out and the head rising and falling 7.5-9 cm a
stride, and a short walk (5.9 m) was never matched; the posture limits took a run's median, so the matched middle hid
the ends. Standing was the game's crouched idle. So this round:

- **Posture by part, and standing** (`eval::walk_parts`, `eval::standing`): each walk of 4 s or more in its first 2 s,
  its last 2 s and the worst of its 2 s windows between (the knee at mid-stance, the elbow's least bend, the trunk's lean,
  the wrist over the hip and the head's rise and fall), and still stretches of 2 s or more (the straighter knee's bend,
  the ankles apart, the hands outside the shoulders). `motion_eval --capture` prints both for every clip played as
  recorded (`animation/evidence/round-7/mac/posture-parts-capture.txt`). Real walking: the knee at mid-stance 7-30
  degrees in a start, 6-23 in a window, 7-26 in a stop; the head's rise and fall 2.2-7.1 cm in a start, 2.2-5.1 in a
  window, 1.3-6.6 in a stop. Real standing: the straighter knee 0-9.5 degrees, the hands -1 to 6.5 cm outside the
  shoulders. The game's limits are set just over these.
- **Planted feet that do not slide or turn on the floor.** A foot a take-over plants keeps how far it stood turned from
  the capture while it stays down (the take-over's ease turned planted soles on the floor about the held point); the
  capture's own turns of it still come through, a transition's jump does not. `Config::hold_until_lift` lets a planted
  point go only once it has risen (by the ball while it is down): a turning start dragged a toe round the floor after the
  capture's contact had ended. `Config::keep_turn_planted` keeps every held foot's heading until it lets go: a pivot on a
  planted foot slid its sole round the held point. Both off by default.
- **The stuck recovery** (the pose dropped, the trajectory searched) goes through the bounding boxes (`search_features`).
- **Retargeting a head on a bent neck:** a head with no end site of its own to pair (an avatar's, eyes beside its top)
  takes half of its neck's rest alignment. All of it tipped back the head of a rig whose neck stands bent forward at rest
  by the neck's straightening (21 degrees bent: about 11 degrees back); none of it pitched every head forward by the
  source's own head turn. Half put the head's top within 2 degrees of the game's own animation on its four bodies.
- **The walk set** (`walk-core.json`) leaves out 100STYLE's TR1 (turning starts that pivot a planted foot on its ball
  through up to 90 degrees; its windows take in other movement than walking) and CMU's standing 77_02, 113_21 and 137_28
  (knees bent through the stance, 18.6-33.4 degrees against 100STYLE's idle's 6.4): standing about plays 100STYLE's idle.
  3.3 MB a man's build, 2.6 MB a woman's (were 4.3 and 3.6).
- **The game** walks every walk whole on the capture again (round 3's design, with rounds 4-7's fixes): the matcher
  takes the body over as standing up ends, stands 0.3 s, sets off (turning as it goes), walks, stops facing the end's way
  (a seat too), and stands about idle on the capture; the game keeps standing up, sitting down and activities. Its own
  layer holds each matched foot by its shoe's lowest sole point while on the ground, settles the shoulders and neck
  towards its garments' pose as it does its own clips', stops for someone in the way within the room it is given, and
  plants the take-over's feet where its body has them (`plant_feet`), not where the database's body would.

**Measured in the game** (its twelve runs, worst of three at two tiers): the posture limits pass in every part of every
walk (the knee at mid-stance 12-26 degrees, the head's rise and fall at most 6.5 cm) and the standing limits pass
(the straighter knee 9-11 degrees, the hands 4-5 cm outside the shoulders); two of the six moves pass every limit at
both tiers, and the other four each break one to three of the game's older limits by small
margins (a slide 3.45-3.64 cm against 3.25, a foot's turn 14.6 degrees against 14.5, a root's acceleration 1.13 cm
against 1). On a Raspberry Pi 5 at the game's lowest tier its frame floor holds and the motion layer costs at most 3.2
ms a frame in a walk (the hybrid's walking take-over cost 8-9 ms once). Not yet: a stop within a step for someone close
in front (the capture needs most of a step), garments that fold on the women's bodies, the knee at heel strike (19-23
degrees), and the game's own rises and sits.

**Correction.** Round 5's paragraph above, and the game's round 5 notes, said the hand-back waited for "a mid-stance
where the two walks agree (not turning, the planted foot along the way, ... within 12 cm)". The game's code never had that
rule: it handed back at a one-foot stance of 0.1-0.3 s, or at the latest 3.9 m out. The game's sixth critic found it.

### In a browser game (round 8)

The game's round 7 left women's heads swinging about 22 degrees to either side as they walked, their headscarves
folding through themselves, and four of its six moves breaking a limit by a little. Traced and fixed here:

- **The chest aligned on a shoulder** (`retarget.rs`): a bone's rest alignment pairs it with a child both skeletons
  share, and the chest took the first one found. An avatar's Spine2 lists LeftShoulder before Neck, and a CMU
  capture's clavicles start where its neck does (at the thorax's end), so the chest was turned to put the avatar's
  shoulder where the capture's neck goes: it stood 12-17 degrees to one side through every walk, on both builds. The
  chest now aligns on its neck (Spine1 on Spine2, Spine on Spine1). The capture played as recorded on the game's
  bodies: the trunk's side lean (p90, `eval::Posture::trunk_side_p90_deg`, new) 1.5-2.8 degrees, as on the canonical
  skeleton, from 4-10.
- **Levelled heads** (`anim_build`, `clips::level_head`; `walk-core.json` sets `level_head`): CMU subject 113's
  skeleton zig-zags its neck at rest (the lower neck 10 degrees to one side, the upper 20 to the other) and its walk
  turns them back by 30-43 degrees; an avatar's one neck bone follows the lower neck alone, and the women's walk (the
  build's one walk tagged `woman` is 113's) held the head 20-35 degrees to the side. Each clip's average turn of the
  neck and the head from their parents is taken back to the target's rest, the motion about it kept. In the game the
  women's head tops swing -7.0 to 6.8 degrees to the side (p10, p90) where they swung -23 to 24, and walking the men's
  faces look 9-10 degrees down (median) where they looked 22-25 down; the women's 3.5 (2.5).
- **The root's speed across transitions** (`controller.rs`): a transition gave the root's velocity blend the velocity
  shown (the offset already in it) as the animation's, and added the offset again; quick transitions in a turn built
  0.9 m/s up to 2.0 and a root jolted 1.1 cm a frame. The offset is now what was shown less the new clip's. The turn
  rate's blend has the same fault and is left as it was: taken as shown, a synthetic walk after standing up veered
  0.6 m off its way, not understood. The synthetic sit test no longer expects its walk after standing up to stop on
  its goal (its clips have no stop; it stopped there only through the doubled offset).
- **A released foot's turn** (`controller.rs`, `TWIST_RELEASE_HALFLIFE` 0.15 s): a foot kept turned while planted
  (`keep_turn_planted`) eases to the capture's in the air as a released critically damped spring from the rate it had,
  not from its full rate at once (a foot let go 50 degrees off swung 14 degrees in a frame).

**Measured in the game** (its twelve runs, worst of three at two tiers): 24 of 36 runs pass, four of its six moves in
every repeat at both tiers (round 7: two). Still over: a foot's turn changing 12.9 degrees a frame (limit 12) as it
lands turned at a bench, and a shoe 1.1-1.2 cm into a chair (limit 1). Its fold check on garments still finds cloth
turned through itself on all four bodies, far less on the women's headscarves. On a Raspberry Pi 5 at the game's
lowest tier its frame floor holds (3.9-4.5% of frames over 33.4 ms after the first minute; at most 3.3 ms of motion a
frame in the walk with the learner, and 9 ms once as someone rises and sets off, not yet traced).

### Round 9 (loadngo)

On the `motion-matching` branch, this Mac, release builds; evidence in `animation/evidence/round-9/`. Measured on the
canonical humanoid and on a man's and a woman's body from a browser game (as `anim_build` targets).

- **The root's turn rate across transitions** (`34f22a3a`, `turn-rate.md`). A transition gave the turn rate's
  inertialization the rate the root had turned as the animation's, so the offset already in it was counted again:
  transitions every 0.05 s on straight synthetic walks spun a root at 188 rad/s, and at the usual 0.1 s a turn the clips
  cannot make was spun up to 1 rad/s. Taken as shown instead (round 8's velocity form), the root's pull towards the
  simulation was kept as each next clip's own turn: that was round 8's "veer" (its sit test's walk after standing up asked
  for a 90 degree turn its synthetic clips cannot make, ping-ponged between two clip ends and turned on at 0.31 rad/s).
  Now the offset is the turn the animation gave (`anim_yaw_rate`, before the pull and any warp) less the new clip's. With
  `walk-core.json` the root's turn changes 2-4 times less in a frame in turns from standing (turn off 80 degrees 0.93 ->
  0.21 rad/s, about-turn 0.76-1.06 -> 0.27-0.30) and less in every other run but a man's slow walk alongside (0.10 ->
  0.24, at a clip's end: see below); end facings are within 0.07 rad of before but in that run (0.12). Worst single contacts move both ways
  between near-tied clips.
- **The root's velocity** (not changed). Round 8's velocity offset (`shown - new`) still keeps the pull towards the
  simulation at each transition. Taken out as the turn rate's is, the root's largest acceleration fell in 15 of 24
  walking runs (three databases, eight scenarios; a woman's about-turn from standing 0.82 -> 0.08 cm a frame squared),
  rose in one (a woman's turn left, 0.79 -> 0.93) and was the same in 8. Left for a game's checks to decide.
- **A kept foot's turn** (`ffc35868`). Round 9's `109c79bd` let a held foot pivot on its held point once its kept turn
  passed 0.45 rad (`TWIST_KEPT_MOST`). In the game a flat planted foot then slid 3.8 cm on the floor as the body turned on
  the spot at a bench, and held feet stood 26-30 degrees off their knees at a hand-over, so that bound is taken out: a
  kept foot never turns while held (round 8's rule). Its kept turn (here 1.4-2.0 rad) is released in the air at most 5
  rad/s (`TWIST_RATE_MOST`): once a landing is in view the release is planned so its cubic peaks within that and lands
  still, and the rest stays the landed foot's kept turn (a plain clamp landed feet still turning 0.12-0.16 rad a frame).
  The test's lifted foot turns at most 0.118 rad a frame (the cap's 0.083 and the body's own 0.035) and lands turning at
  most 0.083 (round 8's form: 0.138 landing, 0.175 lifted).
- **A take-over keeps the body's motion** (`f840dd11`, `68f97037`). `take_over_from` (`takeOver(x, z, facing, pose,
  prevPose, dt, prevRoot)`) is also given the body a frame before; each bone's and the hips' offset rate is then the
  shown motion's (the offset's own rotation-vector rate, `68f97037`; at most 3 m/s and 15 rad/s) instead of zero. A body's
  hips rising at 0.6 m/s change their velocity by 0.020 m/s in the frame after (0.58 from the pose alone); through the
  wasm module on a man's database they go on at 0.576 m/s. A game reported a forward bend carried sideways in a rise
  facing about -x; not reproduced here from poses formed as `take_over` takes them (a bending spine facing 0, +-90 and
  180 degrees, and four captured rises at facings 0 and -1.471 rad, alike).
- **A half step back** (`dd54c90d`, `step-back.md`; not yet used by the game). A new tag `back`, never searched unless
  asked for (`Config::step_back`, or `Character::step_back(distance, speed)` / `stepBack` for its own walk) and left out of
  the features' normalisation; `walk-core.json` appends 100STYLE's backward walk and its stop. `step_back` sets off on
  the trajectory alone once its simulation leads by 0.12 m and lands on its end: 0.2 / 0.3 / 0.4 m asked, 0.202-0.203 /
  0.302 / 0.402 m back on both bodies, 1 mm aside, turned at most 0.106 rad, feet sliding at most 1.5 cm; it sets off
  after 0.8-0.9 s. Each body's database grows 828 frames, 138 KB; every other frame matches as before (`motion_eval`
  per-frame output byte-identical in all ten scenarios on both bodies).
- **Trunk lean** (`trunk-lean.md`; nothing changed). The game's trunk lean by part rose 4.7 -> 8.3 degrees in round 8;
  measured on the bodies 4.9 -> 8.6. It is round 8's chest alignment following the capture: on each capture's own skeleton
  the walks lean as round 8 shows them (within about a degree on average), and round 7's alignment on a shoulder had leaned them
  2.3-6.1 degrees less and 4-6 more to the side. CMU subjects 16's and 69's walks lean 6.6-9.2 degrees by part.
- **Seen in passing:** playback holds a clip's last frame for one step before the search moves on (`at_clip_end`), the
  root's own motion stopping with it (a man's slow walk alongside reached one while turning); the canonical humanoid's
  straight walk on today's `walk-core.json` has a 30.5 cm worst contact from CMU 69_24's mirror image (its capture slides
  21-24 cm on its own).
- **Not done this round:** Mesh2Motion's sits and rises (their export's UE-style names need a bone map) and the knee at
  heel strike (loadngo's share of 19-23 degrees).
- **Linux** (dolores, Raspberry Pi 5, aarch64; run after the round, on `55932122`): `cargo fmt --check`, clippy of both
  animation crates with all targets and features under `-D warnings`, and their tests (55, 1 and 8) pass.

### Round 11a (loadngo)

Stopped once at Jay's wrap-up (the work was heating the Mac) and committed as work in progress (`58612237`), then
finished on 2026-11-12: the settings below are constants, the diagnostics are asserting tests, and the gates ran
(below). The figures in the first three items were measured with scratch probes (not committed) on three databases
from `walk-core.json`: the canonical humanoid's, and a man's and a woman's body's from a browser game, with that
game's settings; they predate the finishing changes and were not measured again.

- **A clip played to its end held** (`controller.rs`, playback): its last frame but one stood for a step, the pose and
  the root still, and the end-of-clip transition then set the root's velocity going again from zero. A swinging ankle
  went about 3.5 cm a frame, then under 0.5, then on; at a path's end a walk's root went 0.6 m/s to nothing in a frame,
  and the transition often chose a walk that set off again. Playback now goes on to the last frame (`frac` 0.999) and the
  next step searches from there.
- **A path braked to its end, not to where it is done** (`path.rs`): the wanted speed was still about 0.43 m/s at the
  arrive radius and fell to nothing there in a frame. It now brakes to rest at the arrive radius, v = sqrt(2 a max(0,
  remaining - arrive_radius)). Arrivals through `Character::go_to` (60 paths a database, 1.5-6 m, bent 0-1.2 rad, 0.6-1
  m/s, at 60 Hz): the root's largest speed drop in a frame 0.53-0.72 -> 0.035-0.064 m/s, runs that stop dead a second
  time 7-17 -> 0 of 60, the head's largest acceleration 1.5-1.7 -> 0.74-0.88 cm a frame squared; at 30 Hz the root's
  drop 0.27 -> 0.13-0.14 (per 60 Hz frame). The hold's fix alone removes the dead stops; the path's alone does not (it
  takes the wanted speed's largest drop in a frame from 0.49 to 0.15-0.23 m/s).
- **A transition keeps a swinging foot up** (`search.rs` `Exclusions::swinging`, `controller.rs` `swinging()`,
  `Config::swing_agree`, on): a transition onto a frame with a foot down that the body has swinging made the foot lock
  take hold of it while it still moved, 2-4.6 cm a frame, most often at the second of two transitions 7 frames apart.
  While a foot is up (no contact in the frame playing, not held) and its shown ankle moves over 0.6 m/s over the ground,
  the search passes over frames that put it down within 0.1 s, unless the clip playing puts it down about as soon (less
  0.05 s); if no frame is left and carrying on is not a choice (a clip's end, standing calmly) it searches as before. On
  600 random turning walks a database (a game's settings, the hold and path fixes in both): transitions that put down a
  foot moving over 1.5 cm a frame 111 / 150 / 142 -> 50 / 65 / 63 (woman, man, canonical); runs whose worst leg
  acceleration while walking passes 6.6 cm a frame squared 1 / 5 / 7 -> 2 / 5 / 8; the root off the path 0.033 m on
  average either way. Measured and not chosen: the same without the speed gate, or looking ahead to the clip playing's
  own landing (more runs past 6.6 on two databases, 5 -> 12 and 7 -> 9-15: the choices it moves to land CMU 104's slow
  walks, whose own landings jolt the knee); a pause in searching for 0.25 s after a transition made mid-swing (option A:
  12% fewer such grabs, and no search, forced or not, meanwhile).
- **Not once the body is to stand** (finishing): with the rule on, `walks_where_it_is_steered` (walks and an idle, no
  stopping clip) drifted 2 m past its path's end and back, a transition every 0.1 s among walk frames with one foot up:
  a stop puts both feet down, and the rule held the search off every frame that does while carrying on stayed a
  choice. The rule now applies only while a speed is wanted (over 0.05 m/s); a stop is searched as before round 11a.
- **Settings:** `Config::swing_agree` (on) stays; the compared variants are gone (`swing_hold`, `swing_mode`), and the
  chosen values are the constants `SWING_SPEED` 0.6 m/s and `SWING_WITHIN` 0.1 s.
- **Tests** (`animation/tests/transitions_and_arrival.rs`, each failing without its change):
  `a_clips_end_does_not_jolt_a_swinging_ankle` (turning paths on the synthetic clips, which end mid-stride: the
  swinging ankle across transitions 4.86 cm a frame squared on `ec820357` at 60 Hz, 1.17 now; limits 1.6 and 1.2 at
  30 Hz); `a_transition_keeps_a_fast_swinging_foot_up` (a database whose pose features weigh almost nothing: with the
  rule off 2 fast-swinging feet put down by a transition at 60 Hz and 1 at 30 Hz, with it on none);
  `a_path_brakes_to_rest_where_it_is_done` (a point moved as the path wants: the speed wanted falls at most 0.12 m/s in
  a step; braking to the end itself, 0.478). `a_kept_foot_turning_on_the_spot_lands_without_spinning` allows 0.11 rad a
  frame at a landing (was 0.09): its turn clip ends with a foot in the air, which the idle puts down, at 0.083 after
  the old step standing still and at 0.106 at the clip's own rate.
- **Gates** (this Mac): `cargo fmt --check`; clippy `-D warnings` on both animation crates, all targets and features,
  and on wasm32; tests 55 + 1 (`no_allocation`) + 3 + 8; `cargo check --target aarch64-apple-ios`;
  `animation/build.sh` (wasm 195,179 bytes, sha256 `6ae37020...`). Not done: `motion_eval` before and after on the
  standard paths; the three databases' figures above after the stand exemption.
- **Seen in passing:** the foot lock taking hold of a landing foot still moving 1-2 cm a frame jolts the knee 7-10 cm a
  frame squared with no transition near, on CMU 104's slow walks above all; not traced.

## What is short

- **The hand-back's blend** (round 6, done in the game): a foot is newly held during the fade only if both walks have it
  planted; the game then passes 35 of its 36 runs (the last a seated hand before any walk).
- **A frame of 8-9 ms** of the game's motion layer, once a walk, on a Pi 5 (after the bounded take-over search; the
  cause of what remains is not yet traced).
- **Turning starts from standing** are the capture's again in round 7, with planted feet that keep their turn and are
  held until they lift; a game's checks still find a sole slide of 3.5-3.6 cm in some of them (against its 3.25).
- **Which frame wins a near tie** decides a run's worst contact, and it moves between databases
  (the tables above); judge a database by its 95th percentiles and gait, and by a game's checks.
- **Seated idles** come from three captures (CMU 86_15, 113_15, 143_18); the clips' talking and
  gestures while seated (tagged) are not used as idles. Mesh2Motion's seated idles and talking are
  exported from Blender as BVH (UE-style names) but need a bone map (not done).
- **A step back** (round 9) sets off after 0.8-0.9 s (no standing pose leads into one) and has one captured stop, which
  drifts sideways on its own; the game does not use it yet.
- **The root's velocity** across transitions still keeps the pull towards the simulation (round 9 measured taking it out:
  see "Round 9 (loadngo)").
- **Database size:** 2.6-3.3 MB per body build for a web game's walking with 32-bit rotations (round 7's set; 2.7-3.4 MB
  with round 9's backward clips); every clip 19 MB at 48 bits.
- **Flat ground only:** contacts are held at the floor's height (a game lifts soles onto its own
  ground).
- **One machine for the harness:** round 3's tables are this Mac's; dolores ran the tests and lints (Linux aarch64)
  and a game's calm plays, not `motion_eval`.

## Next

1. **A game's critic** of round 8; then the last turn and chair, a stop within a step for someone close in front (the
   capture needs most of a step; its stop is not braked; round 9's step back can give the room back after it, not yet in
   the game), and the rises and sits on capture (Mesh2Motion's keyed sits export from Blender as BVH, on a skeleton the
   `mesh2motion` map does not name yet).
2. **Seats in the game:** the framework's sits land to the centimetre on any seat height; a game
   with sliding chairs and hands to a table or lap needs its seat poses joined to the framework's.
3. **Reactions while standing:** the look-at, reach and beats layers in a game whose people stand and talk.
4. **The browser host:** step characters from `BrowserHost`'s frame handler once it merges.

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
