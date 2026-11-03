# The eighth critic's probes, rerun after its round (2026-11-03)

```
PLAYWRIGHT_CORE=.../playwright-core/index.mjs node ../critic8/run8.mjs <port> run8.json
PLAYWRIGHT_CORE=.../playwright-core/index.mjs node ../critic8/run8.mjs <port> run8-batch2.json batch2.html
PLAYWRIGHT_CORE=.../playwright-core/index.mjs node ../critic8/run8.mjs <port> run8-batch3.json batch3.html
```

## What changed under these probes

- **One loss controller a canvas** (`js/web3d.js`, `controllerFor`) replaced the shim's listeners and
  the per-create restore listeners. It puts two permanent listeners on a canvas the first time web3d
  takes a context from it: the lost one cancels every lost event and calls the live renderer's
  `onContextLost`; the restored one calls each `onContextRestored` once (the renderer live at the
  loss, renderers disposed while lost, creates that failed because of it), each in its own `try`,
  reporting what it throws. After a restore no renderer is live until a new one is made. The shim
  listens for nothing and learns of losses from the controller's count.
- **`bitmaprenderer` canvases are refused** before any upload, with a message to pass the
  `ImageBitmap` itself.
- **A detached DataView** is refused with web3d's own message.

## Results

These pass: E1, E2, E3, E5, E6, E7, E7b, E8, E9, E9b, E10, E11, E12 (its note now holds the other way:
the canvas is restored), E13, E14 and E15.

Two fail by their premises:

- **`manyOnOneCanvas`** (E4 and E4b) reads `globalThis[Symbol.for('loadngo-web3d')].restores`, the
  previous round's internal map of waiting hooks, which the controller replaced, so it throws before
  it measures. Its intent is probe LE in `demo/probe.js`: one function given to a live renderer and to
  a failed create runs once (E4b), and every waiting hook runs once.
- **E16** expects a `bitmaprenderer` canvas smaller than its bitmap to be uploaded whole. Such
  canvases are now refused (`"... pass the ImageBitmap itself"`), so the probe finds no texture and
  throws reading its pixels. Probe TU in `demo/probe.js` checks the refusal, for a canvas smaller and
  one larger than its bitmap.
