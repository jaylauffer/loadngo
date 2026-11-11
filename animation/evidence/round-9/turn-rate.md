# The root's turn rate across transitions (round 9)

## Cause

At a transition `MotionMatcher::transition` gave the turn rate's inertialization the rate the root had turned last step as
the animation's, `InertialVec3::transition(root_yaw_rate, .., yaw_new, ..)`, which adds the offset already held to it:

- **The offset counted twice.** `root_yaw_rate` already holds the offset; adding it again doubles it at every transition.
  Transitions more often than the offset decays (half-life 0.1 s) make it grow without bound: on straight synthetic walks
  made to transition every 0.05 s the root spun at 188 rad/s; at the usual 0.1 s searches a turn the clips cannot make
  was spun up to 0.99-1.73 rad/s (`synthetic probe` below).
- **The pull kept as the clip's turn.** `root_yaw_rate` is also the root's turn after its pull towards the simulation's
  facing (and an action's warp). Round 8 took the velocity "as shown" (`offset = shown - new`); taken that way for the turn
  rate too, each transition kept the pull as the next clip's own turn: wound up, not doubled. Round 8's synthetic sit test
  then turned at 0.31 rad/s steadily and veered on past the way wanted (its "0.6 m off its way, not understood"): its walk
  after standing up asked for a 90 degree turn the synthetic clips (two straight walks and an idle) cannot make, and it
  ping-ponged between two clips' ends every 7 frames, each transition keeping the last pull. With the doubled offset (the
  committed code) the same walk spun up to 0.97 rad/s, reached the goal by that, and then circled on at 0.6 m/s.

## Fix (`animation/src/controller.rs`)

`anim_yaw_rate` holds the turn rate the animation gave last step (the clip's own with its offset, before the pull and any
warp; zero while an action pins the root). At a transition the offset is `anim_yaw_rate - yaw_new`: the animation's turn
stays continuous, the pull goes on acting from where it is and is never kept as momentum.

Test: `controller::tests::quick_transitions_neither_double_nor_wind_up_the_roots_turn` (straight walks, a transition every
0.05 s, asked to turn 0.5 rad): the root turns at most 0.26 rad/s with the fix; 188 rad/s with the committed code; 0.62
rad/s with the offset taken as shown. The sit test's walk after standing now goes the way it faces (the synthetic clips
cannot turn 90 degrees) and asserts it does not veer (`off_way < 0.15 m`). `a_planted_foot_keeps_its_heading_when_asked`
walks a 0.6 rad/s circle on straight clips: the root turned at 0.46-0.76 rad/s only through the doubled offset; fixed it
turns at what its pull allows (0.21-0.25 rad/s) and a free held foot 0.005 rad a step (was 0.011), so its floor is 0.001.

## Synthetic probe (controller tests' database: straight walks at 1.0 and 0.7 m/s, an idle; 12 s at 60 Hz)

| Setting | Committed: most turn rate | As shown | Fixed |
| --- | --- | --- | --- |
| search 0.1 s, cost 0.5, asked 0.5 rad | 0.991 rad/s | 0.459 | 0.270 |
| search 0.1 s, every search a transition, asked 0.5 rad | 0.991 | 0.459 | 0.271 |
| search 0.1 s, cost 0.5, asked 1.57 rad | 1.725 | 0.488 | 0.274 |
| search 0.05 s, every search a transition, asked 0.5 rad | 188.352 (ends facing -2.49 rad) | 0.622 | 0.257 |

Each ends facing the way asked within 0.02 rad but the 188 rad/s run.

## On real capture (`motion_eval`, release build, this Mac; databases from `animation/data/walk-core.json`, `--compact`)

Per scenario: the worst and 95th-percentile contact slide (cm), the largest turn rate of the root (rad/s), the largest
change of its turn rate in a frame (rad/s), and how far it ends from its goal (m). Committed code against the fix; the
root's velocity is round 8's in both.

### the canonical humanoid (every clip)

| Scenario | Slide worst / p95, committed | fixed | Turn rate most, committed | fixed | Turn change in a frame, committed | fixed | End off, committed | fixed |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| straight | 30.54 / 18.74 | 30.55 / 18.74 | 0.16 | 0.13 | 0.084 | 0.050 | 0.31 | 0.31 |
| start-stop | 30.78 / 2.75 | 30.78 / 2.75 | 0.16 | 0.13 | 0.107 | 0.057 | 0.27 | 0.27 |
| turn-left-90 | 5.42 / 3.81 | 5.21 / 3.75 | 1.61 | 1.59 | 0.208 | 0.103 | 0.31 | 0.31 |
| turn-right-90 | 25.35 / 6.06 | 25.35 / 6.06 | 2.31 | 2.30 | 0.099 | 0.085 | 0.15 | 0.15 |
| curve | 2.58 / 1.85 | 2.59 / 1.87 | 0.29 | 0.32 | 0.123 | 0.109 | 0.36 | 0.36 |
| turn-off-80 | 3.35 / 3.35 | 4.27 / 4.27 | 2.60 | 2.33 | 0.934 | 0.212 | 0.18 | 0.18 |
| turn-off-180 | 7.94 / 7.94 | 8.30 / 8.30 | 3.82 | 3.38 | 0.762 | 0.268 | 0.13 | 0.14 |
| alongside-slow | 2.11 / 2.11 | 2.27 / 2.27 | 0.25 | 0.25 | 0.090 | 0.021 | 0.01 | 0.01 |

### a man's body from a browser game (--skip-tags woman)

| Scenario | Slide worst / p95, committed | fixed | Turn rate most, committed | fixed | Turn change in a frame, committed | fixed | End off, committed | fixed |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| straight | 3.19 / 3.15 | 3.19 / 2.04 | 0.13 | 0.12 | 0.105 | 0.056 | 0.33 | 0.32 |
| start-stop | 2.11 / 1.53 | 1.79 / 1.77 | 0.14 | 0.15 | 0.105 | 0.057 | 0.33 | 0.36 |
| turn-left-90 | 3.86 / 3.23 | 3.88 / 3.33 | 1.61 | 1.60 | 0.216 | 0.102 | 0.34 | 0.34 |
| turn-right-90 | 11.99 / 3.57 | 21.19 / 4.38 | 2.31 | 2.32 | 0.126 | 0.084 | 0.19 | 0.20 |
| curve | 1.95 / 1.95 | 2.04 / 2.04 | 0.28 | 0.35 | 0.114 | 0.056 | 0.18 | 0.18 |
| turn-off-80 | 15.54 / 15.54 | 3.98 / 3.98 | 2.31 | 2.36 | 0.934 | 0.212 | 0.32 | 0.28 |
| turn-off-180 | 20.75 / 20.75 | 12.08 / 12.08 | 4.56 | 3.53 | 1.064 | 0.300 | 0.37 | 0.36 |
| alongside-slow | 14.10 / 13.37 | 15.53 / 1.80 | 0.08 | 0.33 | 0.101 | 0.239 | 0.19 | 0.44 |

### a woman's body from a browser game (--skip-tags man)

| Scenario | Slide worst / p95, committed | fixed | Turn rate most, committed | fixed | Turn change in a frame, committed | fixed | End off, committed | fixed |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| straight | 18.61 / 5.81 | 21.22 / 5.82 | 0.37 | 0.38 | 0.348 | 0.347 | 0.48 | 0.47 |
| start-stop | 17.62 / 10.32 | 17.63 / 10.33 | 0.16 | 0.13 | 0.091 | 0.050 | 0.36 | 0.36 |
| turn-left-90 | 20.56 / 4.70 | 11.96 / 4.17 | 1.64 | 1.62 | 0.348 | 0.075 | 0.38 | 0.25 |
| turn-right-90 | 26.00 / 7.04 | 26.02 / 7.04 | 1.81 | 1.84 | 0.205 | 0.075 | 0.34 | 0.34 |
| curve | 26.00 / 2.80 | 26.02 / 2.80 | 0.28 | 0.28 | 0.113 | 0.050 | 0.14 | 0.14 |
| turn-off-80 | 3.72 / 3.72 | 3.69 / 3.69 | 2.26 | 2.33 | 0.936 | 0.212 | 0.23 | 0.23 |
| turn-off-180 | 13.09 / 13.09 | 24.48 / 24.48 | 4.50 | 3.41 | 1.051 | 0.268 | 0.40 | 0.43 |
| alongside-slow | 1.10 / 1.10 | 1.09 / 1.09 | 0.12 | 0.13 | 0.064 | 0.050 | 0.06 | 0.06 |

The root's turn changes 2-4 times less in a frame in turns from standing (turn-off-80: 0.93 -> 0.21; turn-off-180:
0.76-1.06 -> 0.27-0.30 rad/s), and less in every other scenario but a man's slow walk alongside (0.10 -> 0.24 rad/s).
There the run reaches a clip's last frame while turning at 0.18 rad/s, and playback holds that frame for one step before
the search moves on (`at_clip_end`), the root's own motion stopping with it: a stop the committed code makes too wherever
a clip ends, not changed here. That run also ends 0.44 m from its goal (0.19 committed) and 0.128 rad off the facing
asked (0.006); every other run ends facing within 0.07 rad of the committed code's end facing (a man's turn off 80
degrees the most: 1.407 -> 1.345 rad) and stands still as soon or up to 0.5 s sooner.

Single worst contacts move both ways (`docs/ANIMATION.md`, "Which frame wins a near tie"). A woman's about-turn from
standing now plays CMU 69_25's turning start (its capture slides 20.7 cm on its own: `motion_eval --capture`) where it
played 69_24's (24.1 cm on its own): 13.1 -> 24.5 cm. A man's turn right: the same contact (CMU 16_17's mirror image,
the left heel at 9.47 s) is 12.0 cm committed and 21.2 cm fixed, with the joints the same within 1 mm on every fourth
frame through it: the measure's contact runs 39 frames instead of 4, a millimetre at its height threshold deciding where
it ends. A man's slow walk alongside: 13.37 -> 1.80 cm at p95.

The canonical humanoid's straight walk's 30.5 cm worst contact is in all runs, committed or fixed: two contacts of
CMU 69_24's mirror image (its capture's own toe slides 20.9-24.1 cm).
