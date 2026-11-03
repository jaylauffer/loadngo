# The fifth critic's probes, rerun after its round (2026-11-03)

```
PLAYWRIGHT_CORE=.../playwright-core/index.mjs node ../critic5/run5.mjs <port> run5-main.json x8 main heap cap
PLAYWRIGHT_CORE=.../playwright-core/index.mjs node ../critic5/run5.mjs <port> run5-exhaust.json exhaust
```

## What changed under these probes

Handles were 30 bits, with a 5-bit salt reused once a page had made 32 renderers, so a disposed
renderer's handle could name a later renderer's object (H5). They are now whole numbers below 2^53:
`serial << 33 | kind << 30 | generation << 16 | (index + 1)`. The serial comes from a page-wide counter
that never repeats, and handles cross the wasm boundary as exact doubles (`src/slots.rs`,
`src/web.rs`, `js/web3d.js`). So no handle is issued twice in a page, there is no limit of 32 live
renderers, and a table holds 65,535 live values and issues about a billion handles.

## Results

These pass: C1, C2, H5, R1, R2, T1, T2, L5, Y5 and B1.

Three probes encode the old 30-bit layout, so they fail by their own premises:

- **X8** builds a "representable fraction" from a mesh handle it expects to be below 2^23. Every
  handle is now at least 2^33, where a double's step is 2^-19, so `spare + 2^-30` is `spare` itself;
  destroying it destroys `spare`. A fraction a double can hold on a handle, `o + 0.5`, is refused
  (`setVisibleObjectPlusHalf` in `run5-main.json`).
- **K1** expects a 33rd live renderer to be refused with "32 renderers are live". That limit is gone:
  the 33rd is created. The probe then creates again on the 33rd's canvas, which still has a live
  renderer, and is refused for that, as one renderer per canvas requires. Its failed-create check
  holds: no hook fired.
- **E1** expects the old table's 1 + 32,766 x 128 handles. With the new layout the object table
  issued **1,073,709,057** (1 + 65,534 x 16,384) in 125 s, then "web3d: too many objects". The first,
  a middle and the last handle were refused, the kept object still drew (green), other kinds still
  created, and `getError` was 0 (`run5-exhaust.json`). Its guard against a low slot's handle being
  issued twice never fired.

The same rounding makes the fourth critic's X8 fail one value, as before (`../critic4-rerun/README.md`).
