//! Grouped-query attention on the Neural Engine (`loadngo_coreml::attention`) against a
//! float64 reference, each pass submitted asynchronously and its result received as a
//! job on a loadngo proactor. Inputs are RMS-normalised per head, as Gemma 4's q, k and
//! v are, so they stay well inside fp16.
#![cfg(target_os = "macos")]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use loadngo_coreml::attention::{AttentionEngine, AttentionOutput, AttentionPass, KvCache};
use loadngo_inference::compute::{ComputePolicy, DeviceKind};
use loadngo_proactor::{new_platform_proactor, PlatformPort, Proactor};

/// Deterministic values in [-1, 1), each `dim`-wide head scaled to RMS `rms`.
fn heads(seed: u64, n: usize, dim: usize, rms: f32) -> Vec<f32> {
    let mut s = seed | 1;
    let mut v: Vec<f32> = (0..n)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 40) as f32 / (1u64 << 23) as f32 - 1.0
        })
        .collect();
    for head in v.chunks_exact_mut(dim) {
        let r = (head.iter().map(|x| x * x).sum::<f32>() / dim as f32).sqrt();
        for x in head {
            *x *= rms / r;
        }
    }
    v
}

struct Case {
    t: usize,
    start: usize,
    heads: usize,
    kv_heads: usize,
    dim: usize,
    window: usize,
    slots: usize,
}

impl Case {
    fn end(&self) -> usize {
        self.start + self.t
    }
}

/// Keys or values for every absolute position, then the ring the decoder would hold.
fn ring(all: &[f32], c: &Case) -> Vec<f32> {
    let row = c.kv_heads * c.dim;
    let rows = c.slots.min(c.end());
    let mut out = vec![0.0; rows * row];
    for p in c.end().saturating_sub(c.slots)..c.end() {
        let slot = p % c.slots;
        out[slot * row..(slot + 1) * row].copy_from_slice(&all[p * row..(p + 1) * row]);
    }
    out
}

fn reference(q: &[f32], k: &[f32], v: &[f32], c: &Case) -> Vec<f64> {
    let row = c.kv_heads * c.dim;
    let group = c.heads / c.kv_heads;
    let mut out = vec![0.0; c.t * c.heads * c.dim];
    for i in 0..c.t {
        let last = c.start + i;
        let first = (last + 1).saturating_sub(c.window);
        for h in 0..c.heads {
            let g = h / group;
            let qt = &q[(i * c.heads + h) * c.dim..][..c.dim];
            let scores: Vec<f64> = (first..=last)
                .map(|p| {
                    let kr = &k[p * row + g * c.dim..][..c.dim];
                    qt.iter()
                        .zip(kr)
                        .map(|(a, b)| f64::from(*a) * f64::from(*b))
                        .sum()
                })
                .collect();
            let m = scores.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            let z: f64 = scores.iter().map(|x| (x - m).exp()).sum();
            for d in 0..c.dim {
                out[(i * c.heads + h) * c.dim + d] = scores
                    .iter()
                    .zip(first..=last)
                    .map(|(x, p)| (x - m).exp() / z * f64::from(v[p * row + g * c.dim + d]))
                    .sum();
            }
        }
    }
    out
}

/// Submits a pass and runs the proactor until its completion job has run.
fn run(
    engine: &mut AttentionEngine,
    proactor: &Proactor<PlatformPort>,
    pass: &AttentionPass<'_>,
    cache: &mut Option<KvCache>,
) -> AttentionOutput {
    let slot: Arc<Mutex<Option<Result<AttentionOutput, String>>>> = Arc::default();
    let filled = Arc::clone(&slot);
    engine
        .submit(pass, cache, &proactor.handle(), move |result| {
            *filled.lock().unwrap() = Some(result);
        })
        .unwrap();
    loop {
        proactor.run_once().unwrap();
        if let Some(result) = slot.lock().unwrap().take() {
            return result.unwrap();
        }
    }
}

/// (max abs error, RMS error) against the reference, and the latency.
fn measure(
    engine: &mut AttentionEngine,
    proactor: &Proactor<PlatformPort>,
    c: &Case,
) -> (f64, f64, Duration) {
    let row = c.kv_heads * c.dim;
    let q = heads(1, c.t * c.heads * c.dim, c.dim, 1.0);
    let k = heads(2, c.end() * row, c.dim, 0.5);
    let v = heads(3, c.end() * row, c.dim, 1.0);
    let (kr, vr) = (ring(&k, c), ring(&v, c));
    let pass = AttentionPass {
        q: &q,
        k: &kr,
        v: &vr,
        t: c.t,
        start: c.start,
        heads: c.heads,
        kv_heads: c.kv_heads,
        dim: c.dim,
        window: c.window,
        slots: c.slots,
    };
    let done = run(engine, proactor, &pass, &mut None);
    let want = reference(&q, &k, &v, c);
    let (mut worst, mut sq) = (0.0_f64, 0.0_f64);
    for (g, w) in done.out.iter().zip(&want) {
        let e = (f64::from(*g) - w).abs();
        worst = worst.max(e);
        sq += e * e;
    }
    (worst, (sq / want.len() as f64).sqrt(), done.latency)
}

fn cases() -> Vec<(&'static str, Case)> {
    let full = usize::MAX;
    vec![
        // Gemma 4 31B sliding layers: 32 heads over 16, 256 wide, window 1024, ring 1536.
        (
            "sliding, 512-position pass, wrapped",
            Case {
                t: 512,
                start: 2600,
                heads: 32,
                kv_heads: 16,
                dim: 256,
                window: 1024,
                slots: 1536,
            },
        ),
        (
            "sliding, first 512 positions",
            Case {
                t: 512,
                start: 0,
                heads: 32,
                kv_heads: 16,
                dim: 256,
                window: 1024,
                slots: 1536,
            },
        ),
        (
            "sliding, one token at 3000",
            Case {
                t: 1,
                start: 3000,
                heads: 32,
                kv_heads: 16,
                dim: 256,
                window: 1024,
                slots: 1536,
            },
        ),
        // Its full layers: 32 heads over 4, 512 wide, every position.
        (
            "full, 77 positions after 700",
            Case {
                t: 77,
                start: 700,
                heads: 32,
                kv_heads: 4,
                dim: 512,
                window: full,
                slots: 32768,
            },
        ),
        (
            "full, one token at 1500",
            Case {
                t: 1,
                start: 1500,
                heads: 32,
                kv_heads: 4,
                dim: 512,
                window: full,
                slots: 32768,
            },
        ),
    ]
}

#[test]
fn matches_the_float64_definition_through_the_proactor() {
    let mut engine = AttentionEngine::new(ComputePolicy::CpuAndNpu).unwrap();
    let proactor = new_platform_proactor().unwrap();
    for (name, c) in cases() {
        let (worst, rms, _) = measure(&mut engine, &proactor, &c);
        eprintln!("{name}: max error {worst:.2e}, rms {rms:.2e}");
        // fp16 inputs, products and softmax: measured rms 0.5-2.2e-3 and max 0.3-2.6e-2
        // on outputs of RMS ~1 (2026-10-03); a wrong mask or head mapping is off by
        // 0.1-1.
        assert!(worst < 5e-2 && rms < 3e-3, "{name}: max {worst}, rms {rms}");
    }
}

/// Placement and latency on Gemma 4 31B's shapes, inputs prepared once and predictions
/// back to back (the error is the correctness test's):
/// `cargo test --release -p loadngo-coreml --test attention -- --ignored --nocapture`
#[test]
#[ignore = "timing; run by hand on the Mac mini"]
fn neural_engine_attention_timing() {
    let mut engine = AttentionEngine::new(ComputePolicy::CpuAndNpu).unwrap();
    let proactor = new_platform_proactor().unwrap();
    for (name, c) in cases() {
        let row = c.kv_heads * c.dim;
        let q = heads(1, c.t * c.heads * c.dim, c.dim, 1.0);
        let k = ring(&heads(2, c.end() * row, c.dim, 0.5), &c);
        let v = ring(&heads(3, c.end() * row, c.dim, 1.0), &c);
        let pass = AttentionPass {
            q: &q,
            k: &k,
            v: &v,
            t: c.t,
            start: c.start,
            heads: c.heads,
            kv_heads: c.kv_heads,
            dim: c.dim,
            window: c.window,
            slots: c.slots,
        };
        let shape = AttentionEngine::shape_of(&pass).unwrap();
        let mut cache = None;
        let started = Instant::now();
        engine.prepare(shape).unwrap();
        let load = started.elapsed();
        run(&mut engine, &proactor, &pass, &mut cache);
        let fill_before = engine.stats().fill_s;
        let mut times: Vec<Duration> = (0..20)
            .map(|_| run(&mut engine, &proactor, &pass, &mut cache).latency)
            .collect();
        let fill = (engine.stats().fill_s - fill_before) / 20.0;
        times.sort();
        let off_npu: Vec<String> = engine
            .placements(shape)
            .unwrap()
            .iter()
            .filter(|(_, d)| *d != DeviceKind::Npu)
            .map(|(n, d)| format!("{n}:{d:?}"))
            .collect();
        let ms = |d: Duration| d.as_secs_f64() * 1e3;
        eprintln!(
            "{name}: compile+load {:.0} ms; 20 predictions: best {:.2}, median {:.2}, p90 {:.2} ms; fill {:.2} ms each; off the NPU: {off_npu:?}",
            ms(load), ms(times[0]), ms(times[10]), ms(times[18]), fill * 1e3,
        );
    }
}

/// Whether the Neural Engine and the GPU run at the same time: one sliding-window prompt
/// pass on each, alone and then submitted together, both completions received on one
/// loadngo proactor.
/// `cargo test --release -p loadngo-coreml --test attention -- --ignored --nocapture concurrent`
#[test]
#[ignore = "timing; run by hand on the Mac mini"]
fn neural_engine_and_gpu_run_concurrently() {
    use loadngo_metal_compute::{Dispatch, Gpu, GroupedShape, Slice};

    let c = Case {
        t: 512,
        start: 2600,
        heads: 32,
        kv_heads: 16,
        dim: 256,
        window: 1024,
        slots: 1536,
    };
    let row = c.kv_heads * c.dim;
    let q = heads(1, c.t * c.heads * c.dim, c.dim, 1.0);
    let k = ring(&heads(2, c.end() * row, c.dim, 0.5), &c);
    let v = ring(&heads(3, c.end() * row, c.dim, 1.0), &c);
    let pass = AttentionPass {
        q: &q,
        k: &k,
        v: &v,
        t: c.t,
        start: c.start,
        heads: c.heads,
        kv_heads: c.kv_heads,
        dim: c.dim,
        window: c.window,
        slots: c.slots,
    };
    let mut engine = AttentionEngine::new(ComputePolicy::CpuAndNpu).unwrap();
    let proactor = new_platform_proactor().unwrap();
    let gpu = Gpu::new().unwrap();
    let shape = GroupedShape {
        t: c.t,
        start: c.start,
        heads: c.heads,
        kv_heads: c.kv_heads,
        dim: c.dim,
        window: c.window,
        slots: c.slots,
        scale: 1.0,
    };
    // The GPU side: `passes` tiled attention dispatches in one command buffer.
    let gpu_batch = |passes: usize| {
        let upload = |v: &[f32]| {
            let mut b = gpu.buffer(v.len() * 4).unwrap();
            b.as_f32_mut().copy_from_slice(v);
            b
        };
        let out_len = q.len() * 4;
        let buffers = vec![
            upload(&q),
            upload(&k),
            upload(&v),
            gpu.buffer(out_len).unwrap(),
        ];
        let mut batch = gpu.batch(buffers, Dispatch::Serial).unwrap();
        for _ in 0..passes {
            batch
                .attention_grouped_tiled(
                    Slice::new(0, 0, q.len() * 4),
                    Slice::new(1, 0, k.len() * 4),
                    Slice::new(2, 0, v.len() * 4),
                    Slice::new(3, 0, out_len),
                    shape,
                )
                .unwrap();
        }
        batch
    };
    let mut cache = None;
    engine
        .prepare(AttentionEngine::shape_of(&pass).unwrap())
        .unwrap();
    run(&mut engine, &proactor, &pass, &mut cache);
    let ms = |d: Duration| d.as_secs_f64() * 1e3;
    // Four GPU passes against four Neural Engine passes, so each side has work for a
    // comparable time; the Neural Engine runs its four one after another (two input
    // sets, refilled as each completes).
    let gpu_passes = 2;
    let npu_passes = 4;
    let gpu_alone = {
        let started = Instant::now();
        let done: Arc<Mutex<bool>> = Arc::default();
        let flag = Arc::clone(&done);
        gpu_batch(gpu_passes).commit(&proactor.handle(), move |c| {
            c.gpu_time.unwrap();
            *flag.lock().unwrap() = true;
        });
        while !*done.lock().unwrap() {
            proactor.run_once().unwrap();
        }
        started.elapsed()
    };
    let npu_alone = {
        let started = Instant::now();
        for _ in 0..npu_passes {
            run(&mut engine, &proactor, &pass, &mut cache);
        }
        started.elapsed()
    };
    // Together: the GPU batch is committed, then the Neural Engine passes run while it
    // works; one proactor receives every completion.
    let started = Instant::now();
    let gpu_done: Arc<Mutex<Option<Duration>>> = Arc::default();
    let flag = Arc::clone(&gpu_done);
    let gpu_started = Instant::now();
    gpu_batch(gpu_passes).commit(&proactor.handle(), move |c| {
        c.gpu_time.unwrap();
        *flag.lock().unwrap() = Some(gpu_started.elapsed());
    });
    for _ in 0..npu_passes {
        run(&mut engine, &proactor, &pass, &mut cache);
    }
    let npu_together = started.elapsed();
    while gpu_done.lock().unwrap().is_none() {
        proactor.run_once().unwrap();
    }
    let both = started.elapsed();
    let gpu_together = gpu_done.lock().unwrap().unwrap();
    eprintln!(
        "alone: GPU {gpu_passes} passes {:.1} ms, Neural Engine {npu_passes} passes {:.1} ms (sum {:.1}); together: both done in {:.1} ms (GPU {:.1} ms, Neural Engine {:.1} ms)",
        ms(gpu_alone), ms(npu_alone), ms(gpu_alone + npu_alone), ms(both), ms(gpu_together), ms(npu_together),
    );
}

/// One layer's cache carried through a decoder's sequence of passes: prompt passes that
/// fill and then wrap the ring, single tokens, and a jump back (a restored session) that
/// must rewrite the cache. Each pass is checked against the float64 definition, and only
/// new rows are written while the cache keeps up.
#[test]
fn a_kept_cache_matches_the_definition_across_passes() {
    let mut engine = AttentionEngine::new(ComputePolicy::CpuAndNpu).unwrap();
    let proactor = new_platform_proactor().unwrap();
    // Gemma's sliding layers at a quarter of the heads, so the test stays quick.
    let (heads_n, kvh, dim, window, slots) = (8, 4, 256, 1024, 1536);
    let total = 3200;
    let row = kvh * dim;
    let k_all = heads(2, total * row, dim, 0.5);
    let v_all = heads(3, total * row, dim, 1.0);
    let mut cache = None;
    for (start, t) in [
        (0, 512),
        (512, 512),
        (1024, 512),
        (1536, 512),
        (2048, 1),
        (2049, 1),
        (2050, 300),
        (1000, 37),
    ] {
        let c = Case {
            t,
            start,
            heads: heads_n,
            kv_heads: kvh,
            dim,
            window,
            slots,
        };
        let q = heads(10 + start as u64, t * heads_n * dim, dim, 1.0);
        let (k, v) = (ring(&k_all, &c), ring(&v_all, &c));
        let pass = AttentionPass {
            q: &q,
            k: &k,
            v: &v,
            t,
            start,
            heads: heads_n,
            kv_heads: kvh,
            dim,
            window,
            slots,
        };
        let before = engine.stats().kv_rows;
        let done = run(&mut engine, &proactor, &pass, &mut cache);
        let wrote = engine.stats().kv_rows - before;
        let want = reference(&q, &k_all, &v_all, &c);
        let (mut worst, mut sq) = (0.0_f64, 0.0_f64);
        for (g, w) in done.out.iter().zip(&want) {
            let e = (f64::from(*g) - w).abs();
            worst = worst.max(e);
            sq += e * e;
        }
        let rms = (sq / want.len() as f64).sqrt();
        assert!(
            worst < 5e-2 && rms < 3e-3,
            "pass at {start} (+{t}): max {worst}, rms {rms}"
        );
        if start == 1000 {
            // Behind the cache, as after /undo: every position the ring holds is written.
            assert_eq!(wrote, c.end().min(slots) as u64, "rewrite at {start}");
        } else {
            assert_eq!(wrote, t as u64, "only new rows at {start}");
        }
        assert_eq!(cache.as_ref().unwrap().synced(), c.end());
    }
    assert_eq!(engine.stats().kv_rewrites, 1);
}

/// The Neural Engine's error as a full layer's key count grows (512 queries; 32 heads
/// over 4, 512 wide): `cargo test --release -p loadngo-coreml --test attention -- --ignored --nocapture long`
#[test]
#[ignore = "diagnostic; run by hand"]
fn long_full_layer_error() {
    let mut engine = AttentionEngine::new(ComputePolicy::CpuAndNpu).unwrap();
    let proactor = new_platform_proactor().unwrap();
    for start in [512, 1536, 3584, 5120] {
        let c = Case {
            t: 512,
            start,
            heads: 32,
            kv_heads: 4,
            dim: 512,
            window: usize::MAX,
            slots: 6014,
        };
        let (worst, rms, _) = measure(&mut engine, &proactor, &c);
        eprintln!(
            "full layer, {} keys: max error {worst:.2e}, rms {rms:.2e}",
            c.end()
        );
    }
}
