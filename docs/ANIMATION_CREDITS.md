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

## Checked for the next rounds (not used yet)

### CMU Graphics Lab Motion Capture Database

- <http://mocap.cs.cmu.edu/>. Its FAQ (checked 2026-11-11): "The motion capture data may be
  copied, modified, or redistributed without permission." The lab asks to be credited: "The data
  used in this project was obtained from mocap.cs.cmu.edu. The database was created with funding
  from NSF EIA-0196217."
- Wanted for: slow steady walking, more starts and stops, standing idles, sitting down and
  standing up. The importer reads its BVH conversions (`anim_build --map cmu`).

### Mesh2Motion animation library

- <https://github.com/Mesh2Motion/mesh2motion-assets>: CC0 1.0 (the repository's licence, checked
  2026-11-11).
- Wanted for: sitting, standing up, idles and talking. Its animations are glTF; a glTF animation
  importer is next-round work.
