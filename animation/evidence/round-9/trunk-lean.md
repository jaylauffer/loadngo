# Trunk lean on a game's bodies, round 7 against round 8 (round 9)

A browser game measured its walkers' trunk lean by part of a walk (most forward lean of a walk's start, worst 2 s window
and stop) at 8.3 degrees in round 8 (limit 10) against 4.7 in round 7. Round 8 changed two things that could move it:
the chest's rest alignment (`retarget.rs`: Spine2 aligned on its Neck child, not its first child, which on an avatar is
LeftShoulder) and levelled heads (`clips::level_head`). Measured here, `walk-core.json`'s walking clips, on the canonical
humanoid and on a man's and a woman's body from a browser game (as `--target`, never copied): round 7's chest alignment
(`dc247914`'s rule) against round 8's, with and without levelled heads.

## The cause

- **Levelled heads move nothing** here: the trunk is the line from the hips joint to the neck joint, and levelling turns
  only the neck and head. Every number below is the same with and without it.
- **On the canonical humanoid the two alignments are the same**: its Spine2 lists Neck first.
- **On the game's bodies round 8 follows the capture; round 7 did not.** Measured on each capture's own skeleton (its
  source bones, mapped by name, forward kinematics of the parsed clip, the same measure), round 8's retargeted trunk leans
  within about a degree of the capture's own on average (3.2 at most); round 7's leaned 2.3 (a man's body) to 6.1 degrees
  (a woman's) less on average, and 4.3-6.0 degrees more to the side, because aligning the chest on the left shoulder tipped it back and over.
- So the rise is the capture being truthful: CMU subjects 16 and 69 walk leaning forward (by part 6.6-9.2 degrees on
  their own skeletons), and the game's 8.3 is where those clips play. **Not a fault; nothing changed.** A game that wants
  its walkers more upright can choose clips (100STYLE's walk leans 4.3 by part).

## Whole clips (median forward lean, 90th percentile of side lean, degrees; 1 s edges left out)

Retargeted less the capture's own, mean (least, most) over the 19 walking clips:

| Body | Round 7 forward | Round 8 forward | Round 7 side | Round 8 side |
| --- | --- | --- | --- | --- |
| the canonical humanoid | -0.64 (-1.62, +1.76) | -0.64 (-1.62, +1.76) | +0.23 | +0.23 |
| a man's body | -2.29 (-3.23, -0.50) | +1.07 (+0.29, +3.17) | +4.29 | +0.40 |
| a woman's body | -6.05 (-7.54, -3.41) | -1.16 (-2.40, +1.90) | +6.03 | +0.72 |

## By part (`eval::walk_parts`, the game's measure; most of any part of the clip's walks of 4 s or more)

The capture's own, on its source skeleton (a scratch probe: the parsed clip's bones renamed through the map, its root the
hips' 0.5 s average, its facing the hips' smoothed lateral axis):

| Clip | Forward lean | Side lean p90 |
| --- | --- | --- |
| 100style-neutral-fw | 4.3 | 4.9 |
| cmu-69-13 | 9.2 | 2.7 |
| cmu-69-14 | 7.7 | 2.2 |
| cmu-69-20 | 6.9 | 1.5 |
| cmu-91-27-slow | 2.5 | 5.4 |
| cmu-113-25-walk | 4.3 | 2.6 |
| cmu-08-04-slow | 3.7 | 5.2 |
| cmu-104-35-slow | 2.7 | 4.7 |
| cmu-104-36-slow | 5.7 | 6.9 |
| cmu-69-21 | 6.6 | 1.2 |
| cmu-16-17 | 6.8 | 1.1 |
| cmu-16-18 | 7.7 | 1.3 |

Played as recorded on the game's bodies (`motion_eval --capture`), round 7's chest alignment against round 8's
(lean / side lean / neck side tilt, degrees):

```

a man's body              round 7: lean / side / neck   round 8: lean / side / neck
  100style-neutral-fw         1.6    9.9   25.0               4.8    4.9   12.7
  cmu-69-14                   4.6    5.4   11.6               7.9    2.0    4.0
  cmu-69-20                   4.1    6.1   16.0               7.5    1.7    8.3
  cmu-91-27-slow              0.3    9.0   20.0               3.6    4.5   20.3
  cmu-08-04-slow              2.7    9.5   14.2               5.9    5.0    7.4
  cmu-104-35-slow             0.7    6.3   18.9               4.3    4.3   21.8
  cmu-104-36-slow             4.1   10.3   27.2               7.4    7.3   26.6
  cmu-69-21                   3.7    6.4   15.5               7.1    2.1    6.0
  cmu-16-17                   4.5    6.8   13.2               7.8    2.5    3.3
  cmu-16-18                   4.9    6.4   13.2               8.6    2.3    3.6
  most: forward lean round 7 4.9, round 8 8.6; side lean round 7 10.3, round 8 7.3

a woman's body              round 7: lean / side / neck   round 8: lean / side / neck
  100style-neutral-fw         0.0    6.7   17.5               4.3    4.9   11.5
  cmu-69-14                   0.5    7.4   20.3               5.2    2.0    6.3
  cmu-69-20                  -0.2    8.3   24.2               4.8    2.5    8.6
  cmu-91-27-slow              0.0   10.6   27.9               1.7    4.7   20.1
  cmu-113-25-walk             0.2    7.6   17.9               4.6    2.3    3.9
  cmu-69-21                  -0.6    8.5   23.5               4.5    2.9    7.2
  most: forward lean round 7 0.5, round 8 5.2; side lean round 7 10.6, round 8 4.9
```

Round 7 to round 8 on a man's body: most forward lean by part 4.9 -> 8.6 (the game measured 4.7 -> 8.3); against the
capture's own by part, round 8 is +0.2 to +2.2 (mean +1.0) and round 7 -3.1 to -1.0. On a woman's body round 8 is -2.5
to +0.3 against the capture's own; round 7 leaned 2.5-7.2 degrees less (0.5 at most by part).
