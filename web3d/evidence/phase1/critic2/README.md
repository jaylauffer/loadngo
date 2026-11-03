# The second critic's probes (phase 1, 2026-11-03)

- `critic.js`, with `index.html`: GPU probes against web3d, and against a real three.js r180
  `WebGLRenderer` for lighting. `run-critic.mjs` and `index.html` were added by the builder, since
  the critic's own runner was not kept.
- `abi-edge.mjs`: the raw wasm ABI in Node, with stub GL imports.
- `report-probe.json`, `report-alloc.json`: the critic's results.

The rerun results are in `../critic2-rerun/`. Two scripts that parsed the asset of the application
the renderer is being built for moved to that application's private repository.

```
PLAYWRIGHT_CORE=.../playwright-core/index.mjs THREE_BUILD=.../three/build node run-critic.mjs <port> <out dir>
node abi-edge.mjs
```
