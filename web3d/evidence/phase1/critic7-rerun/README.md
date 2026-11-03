# The seventh critic's probes, rerun after its round (2026-11-03)

```
PLAYWRIGHT_CORE=.../playwright-core/index.mjs node ../critic7/run7.mjs <port> run7.json
```

The runner takes two ports, the one given and the next.

## What changed under these probes

- **LC1.** `dispose()` on a lost renderer, the recovery the LOST message describes, now leaves a
  one-shot listener that calls `onContextRestored` when the context comes back. A create that fails
  because the context is lost does the same with its own options' hook. One listener a canvas, each
  hook once (`js/web3d.js`, `awaitRestore`).
- **LC2.** `resize` throws "the context is lost" once the renderer is lost; a new renderer may own the
  canvas by then.
- **LC3b.** A create that fails because of a loss no longer calls `onContextLost`.
- **LC4.** A loss while the programs compile is logged as a loss, not as a shader failure.
- **LC7.** `frame()` returns false from the moment the context is lost, before its event.
- **T1, T2.** Before each texture upload the shim clears the errors left pending by earlier calls,
  reporting each as pending in a console warning, so the check after the upload is the upload's own.
- **T3, T4.** A VideoFrame is sized by its visible rectangle turned by its rotation. A canvas with a
  `bitmaprenderer` context, whose size is not its bitmap's, is still refused only by the check after
  the upload; the documentation now says so, and the refusal's reason names it.
- **W1.** A `DataView`'s bytes are passed to `WebAssembly.compile` as a `Uint8Array`.
- **D1.** A context that does not suit names every setting that does not, in one message.

## Results

All 23 probes pass: LC1-LC8 (LC3b included), D1, D2, T1-T5, S1-S3, H1, H2, W1 and W2 (`run7.txt`).
