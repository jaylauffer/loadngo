# motion_eval

Database `walk-core-canonical.lmm`: 26834 frames (7.5 min) at 60 fps, 52 clips, 22 bones; read in 10.3 ms. Steps at 60 Hz; foot lock on.

| scenario | slide worst / p95 (cm) | torso / limb acc (cm/f²) | jog (cm) | cadence (steps/min) | step (m) | speed (m/s) | hip sway (cm) | knee at strike / swing (°) | transitions (/s) | step µs p50 / p99 / max | allocs |
|---|---|---|---|---|---|---|---|---|---|---|---|
| straight | 3.15 / 3.15 | 0.41 / 4.08 | 1.76 | 86.9 | 0.602 | 0.85 | 4.3 | 12.6 / 62.5 | 38 (1.90) | 2.3 / 21.5 / 51.1 | 0 |
| start-stop | 4.82 / 2.29 | 1.34 / 4.87 | 1.76 | - | - | - | - | - | 39 (2.05) | 2.4 / 21.6 / 37.1 | 0 |
| turn-left-90 | 3.60 / 2.59 | 0.41 / 4.08 | 1.79 | - | - | - | - | - | 40 (2.11) | 2.5 / 18.8 / 49.0 | 0 |
| turn-right-90 | 28.32 / 5.16 | 0.41 / 5.20 | 2.06 | - | - | - | - | - | 38 (2.00) | 2.5 / 20.8 / 48.7 | 0 |
| curve | 23.94 / 17.88 | 1.50 / 4.08 | 1.73 | 88.3 | 0.585 | 0.84 | 4.3 | 12.0 / 61.8 | 27 (1.50) | 4.5 / 21.3 / 49.8 | 0 |
| turn-off-80 | 53.93 / 53.93 | 0.68 / 5.40 | 2.61 | - | - | - | - | - | 15 (1.25) | 2.4 / 19.2 / 35.5 | 0 |
| turn-off-180 | 2.77 / 2.77 | 1.55 / 5.47 | 2.61 | - | - | - | - | - | 16 (1.33) | 2.4 / 22.6 / 34.1 | 0 |
| alongside-slow | 4.52 / 2.29 | 0.41 / 4.09 | 1.70 | 87.8 | 0.529 | 0.74 | 3.8 | 11.4 / 62.2 | 51 (2.04) | 2.4 / 24.0 / 65.2 | 0 |

Per step over all scenarios: p50 2.5 µs, p99 21.3 µs; steps that searched p50 5.2 µs, p99 37.1 µs (1270 of 8640); steps that did not p50 2.4 µs, p99 5.1 µs.
