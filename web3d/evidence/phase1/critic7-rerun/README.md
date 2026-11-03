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

After the eighth critic's round, 22 of the 23 probes pass (`run7.txt`). LC5 fails on one detail,
by design now: it expects a second loss on the canvas not to be cancelled ("the one-shot listener
does not outlive it"). Since the eighth round, one loss controller a canvas cancels every lost event
for the canvas's life, so that a canvas lost while no renderer holds it can still be restored
(`docs/WEB3D.md`, the loss controller). The rest of LC5 holds: the create during the loss says
"lost", the default was prevented, the restore worked, a new renderer drew, `getError` was 0.
