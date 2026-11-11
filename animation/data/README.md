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
  `talk`, `gesture`, `man`, `woman` (`database::tags`). The runtime searches and plays by them.
- `mirror`: add the clip's mirror image (default: `anim_build`'s `--no-mirror` decides).

Build one database per body build, keeping the other build's captures out:

```
anim_build --target man.glb --manifest animation/data/walk-core.json --skip-tags woman --out man.lmm
anim_build --target woman.glb --manifest animation/data/walk-core.json --skip-tags man --out woman.lmm
```
