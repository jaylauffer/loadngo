// The phase-1 demo on a Pi's own screen: drives the one tab of a Chromium already running in the Pi's
// desktop session over CDP, sets a 1366x768 viewport, and measures the demo (60 draws, about 325k
// triangles, lean shading) at five render scales without MSAA, then at scale 0.75 with MSAA, 10 s
// each, by reloading the same tab. Fits GPU ms against drawn pixels.
//   PLAYWRIGHT_CORE=.../playwright-core/index.mjs node sweep.mjs <cdp url> <demo url as the Pi sees it> <out json>
import { writeFileSync } from 'node:fs';
// playwright-core from PLAYWRIGHT_CORE (a path to its index.mjs), else from node_modules.
const { chromium } = await import(process.env.PLAYWRIGHT_CORE ?? 'playwright-core');

const [cdp, demo, out] = process.argv.slice(2);
const browser = await chromium.connectOverCDP(cdp);
const pages = browser.contexts().flatMap((c) => c.pages());
if (pages.length !== 1) throw new Error(`expected exactly one tab, found ${pages.length}`);
const page = pages[0];
await page.setViewportSize({ width: 1366, height: 768 });
const runs = [];
for (const [scale, aa] of [[1, 0], [0.75, 0], [0.5, 0], [0.25, 0], [0.1, 0], [0.75, 1]]) {
  await page.goto(`${demo}?scale=${scale}&aa=${aa}`);
  await page.waitForFunction(() => window.__web3d && window.__web3d.measure, null, { timeout: 120000 });
  await page.waitForTimeout(3000);
  const m = await page.evaluate(() => window.__web3d.measure(10000));
  runs.push({ scale, aa, drawnPixels: m.canvas[0] * m.canvas[1], ...m });
  console.log(JSON.stringify({ scale, aa, canvas: m.canvas, fps: m.fps, gpu: m.gpuMs, cpu: m.cpuMs?.median ?? m.cpuMs, draws: m.draws, triangles: m.triangles }));
}
const fit = runs.filter((r) => r.aa === 0);
const xs = fit.map((r) => r.drawnPixels), ys = fit.map((r) => r.gpuMs.mean ?? r.gpuMs.median);
const n = xs.length, mx = xs.reduce((a, b) => a + b) / n, my = ys.reduce((a, b) => a + b) / n;
const b = xs.reduce((s, x, i) => s + (x - mx) * (ys[i] - my), 0) / xs.reduce((s, x) => s + (x - mx) ** 2, 0);
const a = my - b * mx;
const r2 = 1 - xs.reduce((s, x, i) => s + (ys[i] - (a + b * x)) ** 2, 0) / ys.reduce((s, y) => s + (y - my) ** 2, 0);
const result = { machine: 'dolores, Raspberry Pi 5 4 GB, Debian 13, Chromium 156, desktop session', demo, runs, fit: { fixedMs: +a.toFixed(2), msPer1000Pixels: +(b * 1000).toFixed(4), r2: +r2.toFixed(3) } };
writeFileSync(out, JSON.stringify(result, null, 2) + '\n');
console.log(JSON.stringify(result.fit));
await browser.close(); // disconnects only
