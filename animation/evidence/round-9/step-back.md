# A half step back (round 9)

A game's walker stopping for someone who appears 1.1 m in front of him needs about 0.4 m to stop and ends with his face
0.64 m from theirs; people then take a half step back. The walk set had no backward stepping.

## What was added

- `database::tags::BACK` (`"back"` in manifests): walking or stepping backwards. Never searched unless asked for:
  `Config::step_back` (settable by name, `step_back=true`) adds it to the searched tags, and without it every search
  (the main search, the stuck recovery, a take-over, the first frame) excludes it, and a frame of it is never carried on.
  `Database::build` normalises the features over every frame but those of clips tagged `back`.
- `Character::step_back(distance, speed)` (`anim_character_step_back`, `motion.js` `stepBack(distance, speed = 0.4)`): a
  path straight back keeping the facing, `back` frames wanted for it until another command
  (`MotionMatcher::want_back`), setting off on the trajectory alone once the simulation leads by 0.12 m
  (`MotionMatcher::search_on_trajectory`), and landing on its end (`MotionMatcher::want_arrival`, as `arrive_exact`).
- `walk-core.json` appends 100STYLE Neutral's backward walk (`Neutral_BW.bvh`, CC BY 4.0): `100style-neutral-bw`, 8.5-12.8
  s (a straight stretch, 3.1 m back), and `100style-neutral-bw-stop`, 136.9-139.5 s (its one stop: 0.5 m/s to standing
  over about 1.8 s, drifting to one side), both mirrored, tagged `back`, appended last.

## Measured (this Mac, release build; databases from `walk-core.json` with the two clips, `--compact`)

A body standing 2 s, then `step_back(d, 0.4)`, 5 s (a scratch probe over `Character`; slides from `eval::foot_contacts`):

```
man: step 0.2 m at 0.4: ends 0.202 m back (0.211 most), -0.001 m aside, turned at most 0.094 rad, 246 frames tagged back, set off after 54 steps, worst slide 0.9 cm, state Standing
man: step 0.3 m at 0.4: ends 0.302 m back (0.328 most), -0.001 m aside, turned at most 0.098 rad, 251 frames tagged back, set off after 49 steps, worst slide 1.2 cm, state Standing
man: step 0.4 m at 0.4: ends 0.402 m back (0.428 most), -0.001 m aside, turned at most 0.104 rad, 251 frames tagged back, set off after 49 steps, worst slide 1.5 cm, state Standing
man (no back): path 0.3 m at 0.4: ends 0.080 m back (0.080 most), -0.000 m aside, turned at most 0.011 rad, 0 frames tagged back, worst slide 0.0 cm, state Walking
woman: step 0.2 m at 0.4: ends 0.203 m back (0.210 most), 0.001 m aside, turned at most 0.092 rad, 246 frames tagged back, set off after 54 steps, worst slide 1.0 cm, state Standing
woman: step 0.3 m at 0.4: ends 0.302 m back (0.328 most), 0.001 m aside, turned at most 0.097 rad, 251 frames tagged back, set off after 49 steps, worst slide 0.3 cm, state Standing
woman: step 0.4 m at 0.4: ends 0.402 m back (0.428 most), 0.001 m aside, turned at most 0.106 rad, 251 frames tagged back, set off after 49 steps, worst slide 0.9 cm, state Standing
woman (no back): path 0.3 m at 0.4: ends 0.082 m back (0.082 most), -0.006 m aside, turned at most 0.036 rad, 0 frames tagged back, worst slide 0.0 cm, state Walking
```

"(no back)" is the same 0.3 m path asked with the facing held (`face_from` 1.3) and `step_back` off: it never plays a
`back` frame and shuffles 0.08 m on turns in place. It sets off after 49-54 steps (0.8-0.9 s): from a standing pose no
frame leads into a step back, so it waits for the simulation to lead. Tried and dropped (same probe, the first two before
arriving on the goal):

- Searching on the trajectory at once: it jumped into mid-stride frames and went 0.20-0.82 m back for 0.2-0.5 m asked,
  sliding up to 22 cm.
- The stuck recovery as it was (0.4 s after a 0.12 m lead): 1.07 s standing first, 0.38 m back for 0.3.
- Letting `back` go once the steering stops: in 5 of 6 runs the matcher left the captured stop for clips that drifted
  0.36-0.42 m aside, sliding up to 23 cm.
- Without landing on the goal: 0.29 m back for 0.3 on both bodies but 0.18 for 0.2, and 0.38 (a man's body) to 0.60 (a
  woman's, a 20 cm slide) for 0.4.
- Playing the stop as an action warped onto the goal (as sits are): its last 0.35 m runs more aside than back (0.22 m back,
  0.35 aside from 136.9 s), so the warp would drag it sideways.

## Sizes

The two clips add 828 frames (13.8 s with their mirrors) and 138,418 bytes to either body's `--compact` build: a man's
19,519 -> 20,347 frames, 3,262,063 -> 3,400,481 bytes; a woman's 15,329 -> 16,157 frames, 2,561,939 -> 2,700,357 bytes.

## Nothing else changes

`motion_eval --dump-feet` (every frame's database frame, contacts, foot heights and speeds) over all ten standard
scenarios, on a man's and a woman's body: the committed code (`f840dd11`) on the databases without the backward clips,
this code on the same databases, and this code on the databases with them appended: byte-identical CSVs in all 20.
