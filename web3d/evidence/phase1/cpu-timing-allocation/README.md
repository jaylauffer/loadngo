# The CPU timer's allocation, 2026-11-03

The first critic's round found 22 to 23 bytes a frame of JS allocation on an application's
per-frame path at steady state. That figure is two boxed numbers. With the timer in `web3d.js`, the
profiler charged them to `frame()`, or to the caller when V8 inlined `frame()`.

They are the CPU timer's two `performance.now()` readings. V8's middle tier (Maglev) compiles every
per-frame method here (status 25 in `on.txt` and `off.txt`). In that tier each reading comes back
from the browser as a boxed number. That stays true when wasm reads the clock through a bound
import.

So CPU timing is off by default (`createRenderer(canvas, { cpuTiming: true })`,
`renderer.setCpuTiming(on)`). A perf readout turns it on.

`run.mjs` serves `site/`: a per-frame path (`setCamera`, `setTransform`, `frame`, `readStats`, with
plain-Array matrices) and nothing else per frame. The run samples the JS heap for 10 s after 40 s of
frames, one headless Chrome 156 per run, on the M4 Pro.

```
PLAYWRIGHT_CORE=.../playwright-core/index.mjs node run.mjs <port> 40 '?bare=1[&cpu=1]'
```

| CPU timing | Sampled JS bytes per frame | File |
| --- | --- | --- |
| off (the default) | 0 (only the measuring call's own 1.4) | `off.txt` |
| on | **20.8**, charged to the caller, where `frame()` is inlined | `on.txt` |

The renderer's documents quoted the 20.8 measured here until the eleventh round. They now quote
22.0, measured again for that round's code in `../critic10-rerun/cpu-timing/` (this page no longer
runs: it asks V8 about `_putMatrix`, which is `_put` now).
