# Animation data: sources and licences

Motion capture used by the animation pipeline (`docs/ANIMATION.md`). The data itself is never
in this repository: it is downloaded to `~/.loadngo/data/mocap/<dataset>/` and turned into
database files (`.lmm`) by `anim_build`. A database file is an adaptation of the motion it
was built from, so whoever ships one must carry that motion's attribution with it.

Only datasets whose licences allow commercial use are used. Non-commercial datasets
(LAFAN1, AMASS, Bandai Namco's) are not, in any form.

## Used

### 100STYLE

- **What:** over 4 million frames of locomotion in 100 styles, one performer, Xsens MVN
  inertial capture, BVH at 60 fps.
- **Authors:** Ian Mason, Sebastian Starke, Taku Komura. "Real-Time Style Modelling of Human
  Locomotion via Feature-Wise Transformations and Local Motion Phases", Proceedings of the ACM
  on Computer Graphics and Interactive Techniques 5(1), 2022. DOI 10.1145/3522618.
- **Where:** <https://ianxmason.github.io/100style/>; the files from Zenodo record 8127870
  (<https://zenodo.org/record/8127870>), `100STYLE.zip`, 1,468,515,354 bytes, SHA-256
  `2c3b4017f8e2397ee6904c9b9f43551d8b426d39ddc3558d121fee9e55cb2d58` (downloaded 2026-11-11).
- **Licence:** Creative Commons Attribution 4.0 International (CC BY 4.0), as the Zenodo record
  states (`license: cc-by-4.0`, checked 2026-11-11).
- **What we used:** the `Neutral` style ("Normal walking"): `Neutral_FW` (forward walking),
  `Neutral_ID` (idling) and `Neutral_TR1` (transitions), trimmed by the dataset's own
  `Frame_Cuts.csv`.
- **Changes:** retargeted to other skeletons, resampled, floor-levelled, mirrored left to right,
  labelled with foot contacts, and stored with derived features in a motion-matching database.
- **Attribution to carry:** "Motion from the 100STYLE dataset by Ian Mason, Sebastian Starke and
  Taku Komura (CC BY 4.0), retargeted and adapted."

### CMU Graphics Lab Motion Capture Database

- **What:** optical motion capture of over a hundred subjects, ASF skeletons and AMC motion at
  120 fps.
- **Where:** <http://mocap.cs.cmu.edu/>, each trial fetched as its own `.asf` and `.amc`
  (`subjects/<subject>/<subject>_<trial>.amc`), kept under `~/.loadngo/data/mocap/cmu/subjects/`.
- **Licence:** the database's FAQ (checked 2026-11-11): "The motion capture data may be copied,
  modified, or redistributed without permission." The lab asks to be credited as below.
- **What we used** (`animation/data/round-2.json`, 47 trials, ranges and tags there): slow walks
  07_04, 07_05, 08_04, 08_11, 91_10, 91_27, 104_35, 104_36, 104_39, 113_25; walks with turns
  16_17-16_20, 16_27-16_30, 69_13-69_15, 69_20, 69_21, 69_24, 69_25, 69_28, 69_29, 69_31, 69_32;
  slow stops 16_33, 16_34; turns in place 69_16-69_19; standing idles 77_01, 77_02, 111_28,
  113_21, 137_28, 139_01, 139_02, 141_20; sitting down, seated and standing up 75_19, 86_15,
  113_15, 143_18. `animation/data/walk-core.json` is a subset of these with 100STYLE's (26 clips
  since round 3: no Mesh2Motion turns; standing 77_02, 113_21 and 137_28; 139_02 is the same file
  as 77_02 and 139_01 as 77_01).
- **Changes:** as 100STYLE's, and cut into tagged stretches (walk, turn, sit-down, seated, ...).
- **Attribution to carry:** "The data used in this project was obtained from mocap.cs.cmu.edu.
  The database was created with funding from NSF EIA-0196217."

### Mesh2Motion raw captures

- **What:** four turns in place (45 and 180 degrees, left and right), BVH at 60 fps.
- **Where:** <https://github.com/Mesh2Motion/mesh2motion-assets>, `motion-capture/raw captures/`
  (`turn-45-left`, `turn-45-right`, `turn-180-left`, `turn-180-right`).
- **Licence:** CC0 1.0 (the repository's licence, checked 2026-11-11). Only the assets
  repository is used; the Mesh2Motion application's own repository has another licence.
- **Note:** raw, uncleaned capture: their turning steps keep a toe within 1-2 cm of the floor and
  swing an arm out (a database without them chose cleaner turns in two of three turning scenarios;
  see `docs/ANIMATION.md`, "What is short"). In `round-2.json` only; `walk-core.json`, the set a
  web game ships, has left them out since round 3, so a database built from it carries no
  Mesh2Motion motion.
- **Attribution (not required):** "Turns in place from the Mesh2Motion assets (CC0)."

## Checked for the next rounds (not used yet)

### Mesh2Motion keyed animations

- The same repository's `rigs/human/` animations (sitting down and up, seated idles and talking,
  head nods), CC0 1.0. They are Blender files only: using them needs a headless Blender export
  to glTF or BVH, not done.
