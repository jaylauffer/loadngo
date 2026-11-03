# Phase 1 demo on dolores, 2026-11-03

**The scene.** The phase-1 demo (`web3d/demo/`) is a procedural scene of 60 draws and 324,932
triangles. It uses lean shading: Lambert plus Blinn-Phong, a hemisphere ambient, two textures (a
canvas label and a masked grille), all static.

**The setup.**
- It ran on dolores: a Raspberry Pi 5 4 GB, Debian 13, Chromium 156, ANGLE over V3D 7.1.
- Chromium ran in the Pi's desktop session, one tab, viewport 1366x768.
- `sweep.mjs` drove it over CDP from the Mac.
- The Mac served the files through an SSH tunnel.
- Each configuration ran 10 s. The results are in `dolores-sweep.json`.

| Canvas | MSAA | fps | GPU ms mean (p95) | CPU ms median |
| --- | --- | --- | --- | --- |
| 1366×768 | off | 60 | 6.89 (8.95) | 0.2 |
| 1024×576 | off | 60 | 5.28 (6.55) | 0.2 |
| 683×384 | off | 60 | 3.73 (4.83) | 0.2 |
| 341×192 | off | 60 | 2.60 (3.85) | 0.2 |
| 136×76 | off | 60 | 2.11 (3.28) | 0.2 |
| 1024×576 | 4× | 59.5 | 9.47 (12.95) | 0.3 |

**The fit.** Over the five sizes with MSAA off it is **2.32 ms fixed plus 0.0046 ms per 1,000 drawn
pixels**, R² 0.985. On p50 instead, it is 2.86 + 0.0044, R² 0.94.
- The drawn pixels are the canvas's.
- Part of each frame is clear background, so this is not a cost per covered pixel.

**MSAA.** At 1024×576, 4× MSAA added **4.19 ms** of GPU time (9.47 against 5.28). It is
proportionally the largest single cost in this table, and a sweep with MSAA on at every size is
still to do.

**What it shows.** About 325k triangles in 60 draws, with lean vertex and fragment shading, cost a
few milliseconds of GPU time on this Pi.

**What it does not show.** The shading of a real scene: skinning, morph targets, alpha-tested or
blended hair, several textures per material, image light, tone mapping. It also predates two shader
changes:
- three.js's Schlick Fresnel;
- `highp` for the highlight.

Re-measure with each.
