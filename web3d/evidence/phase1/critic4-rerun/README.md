# The fourth critic's probes, rerun after its round (2026-11-03)

- `run4.txt`, `run4.json`: `../critic4/run4.mjs` against the current `pkg/` (Chrome 156, ANGLE over
  Metal, Mac mini M4 Pro):

  ```
  PLAYWRIGHT_CORE=.../playwright-core/index.mjs node ../critic4/run4.mjs <port> run4.json
  ```

- `reproducible.txt`: `build.sh` twice, then once from an empty target directory, with the sha256 of
  every `pkg/` file each time (identical), and every path-like string left in the wasm (none from
  this machine).

## Results

X1 to X7 pass: kinds, renderers, the Proxy, the lying subclass, the failed create, the compile
cache, the handover and re-entrancy.

X8 fails on one of its eleven values, and that value is the handle itself:
- the probe passes `o + 2^-30` and expects it refused;
- handles are now whole numbers below 2^53 with the renderer's serial in their high bits, so `o` is
  107,374,182,401 here (`run4.json`);
- at that size a double's step is 2^-16, so `o + 2^-30` rounds to exactly `o`.

So that call is a valid `setVisible(o, false)` on the probe's own object, which hides it, and the
probe's "still drawn" pixel is then black. The other ten values are refused with "is not a valid
object handle": -0, a Number object, a BigInt, true, an Array, `{ valueOf }`, a numeric string,
2^30, 2^31 + o and Infinity. A fraction that a double can hold, such as `o + 0.5`, is refused
(`demo/probe.js`, probe H; the fifth critic's X8).

The allocation half of `run4.mjs` shows again that Chrome accepts an unknown sampling parameter
silently, and that without the "objects already collected" flags the profiler reads 0. The rendered
check now has a positive control for this. `../blind-control/` is the check run with `--blind`,
which leaves the flags out: the control read 0.7 sampled bytes a frame against at least 40, and the
check failed, as it must.
