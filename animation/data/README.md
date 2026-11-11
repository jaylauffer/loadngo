# Clip manifests

`anim_build --manifest <file>` builds a database from the clips a manifest lists. The motion
itself is never here: `root` names where it was downloaded (see `docs/ANIMATION_CREDITS.md`).

| Manifest | What |
| --- | --- |
| `round-2.json` | Every clip of round 2: locomotion, idles, sitting down, seated, standing up (25 min with mirrors) |
| `walk-core.json` | A subset for a web game's walking: strolling, starts and stops, turns on the way and on the spot, a little standing (6-7.5 min) |

```json
{
  "about": "what the set is for",
  "root": "~/.loadngo/data/mocap",
  "clips": [
    {
      "name": "cmu-86-15-sit",
      "motion": "cmu/subjects/86/86_15.amc",
      "skeleton": "cmu/subjects/86/86.asf",
      "fps": 120,
      "map": "cmu-asf",
      "from": 10.6,
      "to": 12.8,
      "tags": ["sit-down"],
      "mirror": true
    }
  ]
}
```

- `motion`: a BVH, or with `skeleton` an AMC and its ASF; paths under `root` (`~/` is the home
  directory; without `root`, the manifest's own directory).
- `fps`: the source's rate where its file does not say (AMC); `from`, `to`: seconds of it to keep.
- `map`: `100style`, `cmu`, `cmu-asf`, `mesh2motion` or `name`; without it, from the joint names.
- `tags` (at least one): `walk`, `idle`, `turn`, `start-stop`, `sit-down`, `stand-up`, `seated`,
  `talk`, `gesture`, `back`, `man`, `woman` (`database::tags`). The runtime searches and plays by them.
  Clips tagged `back` (walking backwards) are searched only when asked for (`Character::step_back`,
  `Config::step_back`) and left out of the features' normalisation: list them last, and every other
  frame keeps its index and is matched as in a database without them.
- `mirror`: add the clip's mirror image (default: `anim_build`'s `--no-mirror` decides).
- `level_head` (on the manifest, or a clip to override it; default false): the neck's and head's average turn from
  their parents over the clip is taken back to the target's rest, their motion about it kept (`clips::level_head`).
  A capture's neck can carry its fitting's error as a constant turn: CMU subject 113's skeleton zig-zags its neck at
  rest and its motion turns it back by 30-43 degrees, which an avatar's one neck bone followed as a head held 20-35
  degrees to one side. `anim_build` prints what it took out per clip. `walk-core.json` levels every clip.

Build one database per body build, keeping the other build's captures out:

```
anim_build --target man.glb --manifest animation/data/walk-core.json --skip-tags woman --out man.lmm
anim_build --target woman.glb --manifest animation/data/walk-core.json --skip-tags man --out woman.lmm
```
