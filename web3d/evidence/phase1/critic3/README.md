# The third critic's reruns and probes (phase 1, 2026-11-03)

| Path | What |
| --- | --- |
| `gates-rerun.txt` | fmt, clippy and tests, also from a fresh target directory |
| `build-rerun.txt` | `build.sh`, with `pkg` byte-identical from two target directories |
| `check-rerun.log`, `check-rerun/` | `demo/check.mjs` |
| `earlier-critics-rerun/` | The first and second critics' scripts |
| `pkg-before/` | The `pkg` the critic reviewed |
| `probe-site/probe3.js`, `run3.mjs`, `probe3-report.json`, `probe3-run.txt` | Its own probes: primitive restart against a raw-WebGL control, forged handles (H), a second renderer on one canvas (S), a NaN clear colour, resize to 0x0 and back |

**Moved out.** Parts of this round read the assets of the application the renderer is being built
for, which is private. They moved to that application's repository: the asset parses, the light
check and the verdict. Scratch-directory paths in the logs are shown as `<scratch>`.

**Redactions.** An application name in the `pkg-before/` comments, and in a run note of
`check-rerun/rendered-check.json`, is redacted.

**Its findings, and where they stand:**
- Forged handles reached live objects through ToInt32, and a second renderer on one canvas broke the
  first. Both are fixed, and probes H and S are now in `demo/probe.js`.
- The allocation gate could pass vacuously. It is now strict.
- A NaN clear colour was accepted. It is now refused.
- Wording in `src/shaders.rs`.

**Running the probes:**

```
PLAYWRIGHT_CORE=.../playwright-core/index.mjs node run3.mjs <port> <out dir>
```
