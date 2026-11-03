# The sixth critic's probes, rerun after its round (2026-11-03)

```
PLAYWRIGHT_CORE=.../playwright-core/index.mjs node ../critic6/run6.mjs <port> run6.json
PLAYWRIGHT_CORE=.../playwright-core/index.mjs node ../critic6/run6.mjs <port> run6-extra.json extra.html
```

## What changed under these probes

- **A lost context.** `createRenderer` now refuses a lost context as lost before it reads or records
  anything from it. A create during a loss, or on a context the browser dropped, says "the WebGL
  context is lost", and the documented recovery then works with the default options (G9c; probe K2
  in `demo/probe.js`). A context lost during the create is reported as lost, and stays restorable:
  the shim lets a lost event that has not been dispatched yet be cancelled (G10; probe K3).
- **Depth.** A context without a depth buffer is refused (G9a; probe G).
- **Texture sources.** Only sources whose upload size is exactly knowable are taken: `ImageBitmap`,
  canvas, `OffscreenCanvas`, `ImageData`, and `VideoFrame`, sized by its visible rectangle. An `<img>`
  or `<video>` is refused with a message to decode it with `createImageBitmap`. The shim also checks
  `getError()` once after each upload, and refuses and frees the texture on any error (G8b, G8c;
  probe G).
- **Realms.** The serial counter now lives on the top window when the realm can reach it, so a window
  and its same-origin frames share it; each counter starts at a random serial and goes round
  (G6; probe R). A worker or a cross-origin frame keeps its own counter, so two of those can give the
  same serial by chance.
- **The handle layout,** to keep fractions fractions: `serial << 30 | kind << 27 | generation << 16 |
  (index + 1)`, below 2^50. With a random start, serials near 2^20 put the round-6 layout's handles
  near 2^53, where a double holds no fractions and `o + 0.5` rounded to `o + 1`, another handle (G2
  failed once on that). Below 2^50 a double still holds eighths, so `o + 0.5` stays a fraction and is
  refused (probe H7, at the largest serial).

## Results

These pass: G2, G3, G4, G5, G6, G7, G8b, G8c, G9a, G9b, G9c, G10, G11, G13 and G16.

Four encode what changed, and fail by their own premises:

- **G1** decodes handles with the round-6 shifts (`serial << 33 | kind << 30`); under the present
  layout its decode reads wrong fields. Handles are below 2^50 and decode as `src/slots.rs` says.
- **G12** expects 16,384 generations a slot; there are now 2,048. Its own counts show it: 16,400
  handles over slots 2 to 10, 2,048 each, none issued twice, the retired slot's handles refused.
- **G8a** expects a plain `<img>` to be uploaded. An `<img>` is now refused, with the message to
  decode it first, since its `naturalWidth` can differ from the pixels WebGL uploads (G8b is the case
  where it does). Its other sources (ImageData, a closed ImageBitmap, a broken or cross-origin image)
  are refused or uploaded cleanly, with `getError` 0.
- **G14** sets the counter to its last serials and expects the next create refused. The counter now
  goes round, counting renderers made instead, so the next create takes serial 1. Its handles at the
  largest serial were exact and drew, and the other renderer's handle was refused.
