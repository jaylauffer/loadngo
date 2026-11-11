# motion_eval

Database `neutral-canonical.lmm`: 28978 frames (8.0 min) at 60 fps, 6 clips, 22 bones; read in 28.1 ms. Steps at 60 Hz; foot lock on.

| scenario | slide worst / p95 (cm) | torso / limb acc (cm/f²) | jog (cm) | cadence (steps/min) | step (m) | speed (m/s) | hip sway (cm) | knee at strike / swing (°) | transitions (/s) | step µs p50 / p99 / max | allocs |
|---|---|---|---|---|---|---|---|---|---|---|---|
| straight | 4.17 / 3.32 | 0.59 / 3.97 | 1.76 | 86.9 | 0.605 | 0.86 | 4.4 | 12.7 / 62.8 | 33 (1.65) | 3.2 / 40.1 / 81.8 | 0 |
| start-stop | 7.80 / 7.80 | 0.60 / 3.95 | 1.76 | - | - | - | - | - | 41 (2.16) | 2.5 / 35.8 / 56.1 | 0 |
| turn-left-90 | 7.44 / 4.17 | 0.59 / 3.97 | 1.76 | - | - | - | - | - | 35 (1.84) | 2.5 / 31.9 / 52.2 | 0 |
| turn-right-90 | 4.96 / 4.17 | 0.59 / 5.05 | 1.82 | - | - | - | - | - | 41 (2.16) | 2.5 / 27.6 / 50.5 | 0 |
| curve | 4.17 / 4.17 | 0.59 / 3.97 | 1.73 | 84.2 | 0.618 | 0.86 | 2.3 | 12.9 / 61.7 | 39 (2.17) | 4.6 / 33.0 / 55.0 | 0 |
| alongside-slow | 4.64 / 4.07 | 0.53 / 4.20 | 1.45 | 113.5 | 0.354 | 0.64 | 4.7 | 20.3 / 55.9 | 47 (1.88) | 2.3 / 34.3 / 54.3 | 0 |

Per step over all scenarios: p50 2.6 µs, p99 33.1 µs; steps that searched p50 5.7 µs, p99 52.0 µs (3156 of 21600); steps that did not p50 2.5 µs, p99 5.2 µs.
