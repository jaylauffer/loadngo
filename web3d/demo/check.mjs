#!/usr/bin/env node
// The rendered check for the web3d demo: serves web3d/ on 127.0.0.1 (a port of your choosing,
// refused if taken), opens the demo in a headless Google Chrome of its own on this machine's GPU,
// aborts if WebGL is software-rendered, measures frame times, CPU and GPU milliseconds, takes
// screenshots, and stops its browser and server.
//
// It fails (exit 1) unless every demo run draws the scene it should (60 draws, 324,932 triangles),
// its pixels are right (the exact clear colour above the horizon, a lit object at the centre), and
// it reports no GL error and no page error; and unless every probe in demo/probe.html passes
// (pixel-exact rendering, lighting against three.js r180, refusals, use after dispose, context loss
// and recovery) with no WebGL INVALID_OPERATION warning.
//
// A last run samples JS heap allocation while frames are drawn: once in the first seconds (V8
// still interpreting the JS and using its generic wasm call wrappers, which box fractional numbers)
// and once at steady state, after --steady seconds of frames, with the profiler off in between. It
// fails if the page's own scripts allocate at steady state (CPU timing and readout off). A third
// window is the positive control: the demo makes one short-lived Array of 8 numbers a frame, and
// the check fails as blind unless the profiler reports it (Chrome accepts an unknown sampling
// parameter silently, so a profiler that cannot see collected objects would otherwise read 0 and
// pass). A fourth window, with CPU timing on, is reported, not gated.
//
//   PLAYWRIGHT_CORE=/path/to/node_modules/playwright-core/index.mjs \
//     node web3d/demo/check.mjs --port 5397 --out web3d/evidence/phase1 [--seconds 10] [--steady 60] [--blind]
//
// --blind samples without the "objects already collected" flags, as a Chrome that ignored them
// would: a test of the positive control, after which the check must fail.
//
// Build first (web3d/build.sh). Needs Google Chrome installed (playwright's channel 'chrome').
// BSD-3-Clause; see the repository's LICENSE.

import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';

const args = Object.fromEntries(
  process.argv.slice(2).reduce((pairs, a, i, all) => (a.startsWith('--') ? [...pairs, [a.slice(2), all[i + 1]]] : pairs), []),
);
const port = Number(args.port);
if (!Number.isInteger(port) || port <= 0 || !args.out) {
  console.error('usage: node check.mjs --port <free port> --out <dir> [--seconds 10] [--steady 60]');
  process.exit(2);
}
const seconds = Number(args.seconds ?? 10);
const steady = Number(args.steady ?? 60);
const blind = process.argv.includes('--blind');
const outDir = path.resolve(args.out);
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const { chromium } = await import(process.env.PLAYWRIGHT_CORE ?? 'playwright-core');

// The camera holds still (still=1) in the timed runs, so every run draws the same frame: all 60
// objects in view, and pixels that can be checked. The allocation run below moves it.
const RUNS = [
  { name: 'low-1024x576', query: 'still=1&scale=0.75', note: 'a 1366x768 window drawn at pixel ratio 0.75' },
  { name: 'full-1366x768', query: 'still=1&scale=1', note: 'the same window at pixel ratio 1' },
  { name: 'low-1024x576-no-msaa', query: 'still=1&scale=0.75&aa=0', note: 'pixel ratio 0.75 without multisampling' },
];
const VIEWPORT = { width: 1366, height: 768 };
const SCENE = { draws: 60, triangles: 324932 };
const srgbByte = (c) => Math.round((c <= 0.0031308 ? c * 12.92 : 1.055 * c ** (1 / 2.4) - 0.055) * 255);
// CPU timing off (cputime=0): its two clock readings a frame are boxed numbers by design (20.8
// sampled bytes a frame, see Renderer.setCpuTiming). The readout off (hud=0) and frames counted without
// statistics (frames()), so nothing the page runs per frame should allocate: the check fails if the
// page's own scripts (demo/ and pkg/) average more than ALLOCATION_LIMIT sampled bytes a frame at
// steady state. Chrome's own code ("(native or internal)") is reported, not gated.
const ALLOCATION_RUN = { name: 'allocation', query: 'scale=0.75&cputime=0&hud=0', note: 'JS heap allocation while drawing at pixel ratio 0.75, camera and three objects moving, CPU timing and readout off' };
const ALLOCATION_LIMIT = 1;
// A window must hold at least this share of the frames 60 Hz would give, or it proves nothing.
const MIN_FRAME_SHARE = 0.9;
// The positive control's least reading, in sampled bytes a frame from demo.js: it allocates about
// 90 a frame, all of it collected by the next minor GC. Below this the profiler is blind.
const CONTROL_MIN = 40;
const SOFTWARE = /SwiftShader|llvmpipe|softpipe|software|Basic Render/i;

const TYPES = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.mjs': 'text/javascript; charset=utf-8',
  '.wasm': 'application/wasm',
  '.json': 'application/json',
  '.ts': 'text/plain; charset=utf-8',
};

const server = http.createServer(async (req, res) => {
  let p = decodeURIComponent(new URL(req.url, 'http://localhost').pathname);
  if (p.endsWith('/')) p += 'index.html';
  const file = path.join(root, path.normalize(p));
  if (!file.startsWith(root + path.sep)) {
    res.writeHead(403).end();
    return;
  }
  try {
    const body = await readFile(file);
    // Cross-origin isolation gives performance.now() 5 us resolution instead of 100 us.
    res.writeHead(200, {
      'Content-Type': TYPES[path.extname(file)] ?? 'application/octet-stream',
      'Cache-Control': 'no-store',
      'Cross-Origin-Opener-Policy': 'same-origin',
      'Cross-Origin-Embedder-Policy': 'require-corp',
    });
    res.end(body);
  } catch {
    res.writeHead(404).end();
  }
});
// Strict port: fail rather than pick another one.
await new Promise((resolve, reject) => {
  server.once('error', reject);
  server.listen(port, '127.0.0.1', resolve);
});
console.log(`serving ${root} on http://127.0.0.1:${port}/`);

/** Sums sampled allocation bytes by script file and by function, from a sampling heap profile. */
function allocationsByScript(profile) {
  const byFile = {};
  const byFunction = {};
  const walk = (node) => {
    const { url, functionName, lineNumber } = node.callFrame;
    if (node.selfSize > 0) {
      const file = url ? new URL(url).pathname : '(native or internal)';
      byFile[file] = (byFile[file] ?? 0) + node.selfSize;
      if (url) {
        const key = `${file}:${lineNumber + 1} ${functionName || '(anonymous)'}`;
        byFunction[key] = (byFunction[key] ?? 0) + node.selfSize;
      }
    }
    node.children.forEach(walk);
  };
  walk(profile.head);
  return { byFile, byFunction };
}

const report = {
  date: new Date().toISOString(),
  machine: { platform: `${os.platform()} ${os.release()}`, cpu: os.cpus()[0]?.model, memoryGiB: Math.round(os.totalmem() / 2 ** 30) },
  viewport: VIEWPORT,
  measuredSeconds: seconds,
  runs: [],
};

await mkdir(outDir, { recursive: true });
let browser;
let failed = false;
try {
  browser = await chromium.launch({ channel: 'chrome', headless: true, args: ['--enable-gpu', '--ignore-gpu-blocklist'] });
  report.browser = `Chrome ${browser.version()} (headless, launched by this script)`;
  console.log(report.browser);
  for (const run of RUNS) {
    const context = await browser.newContext({ viewport: VIEWPORT, deviceScaleFactor: 1 });
    const page = await context.newPage();
    const messages = [];
    page.on('console', (m) => {
      if (m.type() === 'error' || m.type() === 'warning') messages.push(`${m.type()}: ${m.text()}`);
    });
    page.on('pageerror', (e) => messages.push(`pageerror: ${e.message}`));
    const url = `http://127.0.0.1:${port}/demo/?${run.query}`;
    await page.goto(url);
    const info = await page.evaluate(() => window.__web3d.ready);
    const isolated = await page.evaluate(() => self.crossOriginIsolated);
    if (SOFTWARE.test(info.renderer)) {
      throw new Error(`WebGL is software-rendered here (${info.renderer}); this check needs the real GPU`);
    }
    // Warm-up: shaders compiled, caches filled, GPU clocks up.
    await page.evaluate(() => window.__web3d.measure(3000));
    const result = await page.evaluate((ms) => window.__web3d.measure(ms), seconds * 1000);
    const glError = await page.evaluate(() => window.__web3d.glError());
    const hud = await page.evaluate(() => document.getElementById('hud').textContent);
    // Pixels: above the horizon is the clear colour, exactly; the centre is a lit object.
    const [w, h] = result.canvas;
    const clear = (await page.evaluate(() => window.__web3d.clearColor)).map(srgbByte);
    const [top, centre] = await page.evaluate((points) => window.__web3d.pixels(points), [
      [Math.floor(w / 2), 4],
      [Math.floor(w / 2), Math.floor(h / 2)],
    ]);
    const pixels = {
      top,
      centre,
      clearExpected: clear,
      topIsClear: top.every((v, i) => Math.abs(v - clear[i]) <= 1),
      centreIsLit: Math.max(...centre) > 40 && centre.some((v, i) => Math.abs(v - clear[i]) > 10),
    };
    const sceneOk = result.draws === SCENE.draws && result.triangles === SCENE.triangles;
    const shot = path.join(outDir, `demo-${run.name}.png`);
    await page.screenshot({ path: shot });

    const record = { name: run.name, note: run.note, url, crossOriginIsolated: isolated, ...result, glError, pixels, sceneOk, hud, messages, screenshot: path.basename(shot) };
    report.runs.push(record);
    const g = result.gpuMs;
    console.log(
      `${run.name}: ${result.canvas.join('x')} ${result.fps} fps, frame p50 ${result.intervalMs.p50} p95 ${result.intervalMs.p95} ms, ` +
        `GPU ${g ? `mean ${g.mean} p50 ${g.p50} p95 ${g.p95} ms (${g.n} readings)` : 'n/a'}, CPU mean ${result.cpuMs.mean} p95 ${result.cpuMs.p95} ms, ` +
        `${result.draws} draws ${result.triangles} tris, glError ${glError}, messages ${messages.length}, ` +
        `pixels top ${top} (clear ${clear}) centre ${centre}`,
    );
    const problems = [];
    if (!sceneOk) problems.push(`scene is ${result.draws} draws and ${result.triangles} triangles, not ${SCENE.draws} and ${SCENE.triangles}`);
    if (!pixels.topIsClear) problems.push(`the top centre pixel ${top} is not the clear colour ${clear}`);
    if (!pixels.centreIsLit) problems.push(`the centre pixel ${centre} is not a lit object`);
    if (glError !== 0) problems.push(`getError ${glError}`);
    if (messages.some((m) => m.startsWith('error') || m.startsWith('pageerror'))) problems.push('page errors');
    record.problems = problems;
    if (problems.length) {
      failed = true;
      console.log(`  FAILED: ${problems.join('; ')}`);
    }
    await context.close();
  }

  // Probes: pixel-exact rendering, lighting against three.js r180, refusals, dispose, context loss.
  {
    const context = await browser.newContext({ viewport: { width: 800, height: 300 }, deviceScaleFactor: 1 });
    const page = await context.newPage();
    const messages = [];
    page.on('console', (m) => messages.push(`${m.type()}: ${m.text()}`));
    page.on('pageerror', (e) => messages.push(`pageerror: ${e.message}`));
    await page.goto(`http://127.0.0.1:${port}/demo/probe.html`);
    const probe = await page.evaluate(() => window.__probe.done);
    if (SOFTWARE.test(probe.info?.renderer ?? '')) throw new Error(`WebGL is software-rendered here (${probe.info.renderer})`);
    const invalid = messages.filter((m) => /INVALID_OPERATION|does not belong/.test(m));
    const failedProbes = [...(probe.failed ?? []), ...(probe.error ? [probe.error] : [])];
    report.probes = { ...probe, invalidOperationWarnings: invalid, messages };
    console.log(`probes: ${probe.passed} passed, ${failedProbes.length} failed; INVALID_OPERATION warnings: ${invalid.length}`);
    for (const t of probe.tests ?? []) console.log(`  ${t.pass ? 'pass' : 'FAIL'}  ${t.name}`);
    if (failedProbes.length || invalid.length || messages.some((m) => m.startsWith('pageerror'))) {
      failed = true;
      console.log(`  FAILED: ${[...failedProbes, ...invalid].join('; ')}`);
    }
    await context.close();
  }

  // Allocation: the sampling heap profiler over two windows of frames, cold and steady.
  {
    const context = await browser.newContext({ viewport: VIEWPORT, deviceScaleFactor: 1 });
    const page = await context.newPage();
    const url = `http://127.0.0.1:${port}/demo/?${ALLOCATION_RUN.query}`;
    await page.goto(url);
    await page.evaluate(() => window.__web3d.ready);
    const cdp = await context.newCDPSession(page);
    await cdp.send('HeapProfiler.enable');
    const flags = blind ? 'none (--blind)' : 'objects already collected by minor and major GC included';
    // Chrome accepts unknown parameters here without complaint, so the flags' effect is proved by
    // the positive control below, not by the call succeeding.
    const start = () =>
      cdp.send(
        'HeapProfiler.startSampling',
        blind ? { samplingInterval: 32 } : { samplingInterval: 32, includeObjectsCollectedByMajorGC: true, includeObjectsCollectedByMinorGC: true },
      );
    const sample = async (label) => {
      await cdp.send('HeapProfiler.collectGarbage');
      await start();
      const frames = await page.evaluate((ms) => window.__web3d.frames(ms), seconds * 1000);
      const { profile } = await cdp.send('HeapProfiler.stopSampling');
      const heap = allocationsByScript(profile);
      const top = Object.entries(heap.byFunction).sort((a, b) => b[1] - a[1]).slice(0, 12);
      const perFrame = Object.fromEntries(Object.entries(heap.byFile).map(([f, b]) => [f, Math.round((b / frames) * 10) / 10]));
      const pageBytes = Object.entries(heap.byFile).filter(([f]) => f.startsWith('/demo/') || f.startsWith('/pkg/')).reduce((a, [, b]) => a + b, 0);
      const pagePerFrame = Math.round((pageBytes / frames) * 100) / 100;
      console.log(`allocation, ${label}: ${frames} frames; page scripts ${pagePerFrame} sampled bytes a frame; by file ${JSON.stringify(perFrame)}`);
      return { label, frames, pageScriptBytesPerFrame: pagePerFrame, sampledBytesByFile: heap.byFile, sampledBytesPerFrameByFile: perFrame, topFunctions: Object.fromEntries(top) };
    };
    const cold = await sample('cold (first frames)');
    await page.evaluate((ms) => window.__web3d.frames(ms), Math.max(0, steady - seconds) * 1000);
    const warm = await sample(`steady (after ${steady + seconds} s of frames)`);
    const minFrames = Math.floor(seconds * 60 * MIN_FRAME_SHARE);
    const enoughFrames = cold.frames >= minFrames && warm.frames >= minFrames;
    if (!enoughFrames) {
      failed = true;
      console.log(`  FAILED: a sampling window drew ${Math.min(cold.frames, warm.frames)} frames, fewer than ${minFrames}`);
    }
    await page.evaluate(() => window.__web3d.control(true));
    const control = await sample('positive control (one short-lived Array of 8 numbers a frame)');
    await page.evaluate(() => window.__web3d.control(false));
    await page.evaluate(() => window.__web3d.renderer.setCpuTiming(true));
    const timing = await sample('CPU timing on (reported, not gated)');
    await page.evaluate(() => window.__web3d.renderer.setCpuTiming(false));
    const controlBytes = control.sampledBytesPerFrameByFile['/demo/demo.js'] ?? 0;
    const sighted = control.frames >= minFrames && controlBytes >= CONTROL_MIN;
    if (!sighted) {
      failed = true;
      console.log(`  FAILED: the positive control read ${controlBytes} sampled bytes a frame from demo.js (at least ${CONTROL_MIN} expected): the profiler is blind to short-lived objects, so the gate proves nothing`);
    }
    const allocationOk = enoughFrames && sighted && warm.pageScriptBytesPerFrame <= ALLOCATION_LIMIT;
    report.allocation = {
      url,
      samplingIntervalBytes: 32,
      flags,
      limitBytesPerFrame: ALLOCATION_LIMIT,
      minFrames,
      controlMinBytesPerFrame: CONTROL_MIN,
      controlBytesPerFrame: controlBytes,
      sighted,
      steadyOk: allocationOk,
      note: 'Sampled: each sample stands for about 32 bytes on average. CPU timing and the readout are off; the gate is on the page scripts (demo/ and pkg/) at steady state, and holds only if the positive control is seen.',
      windows: [cold, warm, control, timing],
    };
    if (enoughFrames && sighted && !allocationOk) {
      failed = true;
      console.log(`  FAILED: the page's scripts allocate ${warm.pageScriptBytesPerFrame} sampled bytes a frame at steady state (limit ${ALLOCATION_LIMIT})`);
    }
    await context.close();
  }
} finally {
  await browser?.close();
  server.closeAllConnections?.();
  await new Promise((resolve) => server.close(resolve));
  console.log('browser and server stopped');
}
await writeFile(path.join(outDir, 'rendered-check.json'), `${JSON.stringify(report, null, 2)}\n`);
console.log(`wrote ${path.join(outDir, 'rendered-check.json')}`);
if (failed) {
  console.error('FAILED: see the lines marked FAILED above');
  process.exit(1);
}
