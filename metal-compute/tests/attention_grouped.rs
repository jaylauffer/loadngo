//! `attention_grouped` against a float64 reference: grouped heads, full and sliding
//! windows, rings smaller than the positions, decode and multi-position steps, sharp and
//! flat softmaxes, and shape validation.
#![cfg(target_os = "macos")]

use std::sync::{Arc, Mutex};

use loadngo_metal_compute::{Batch, Buffer, Completed, Dispatch, Gpu, GroupedShape, Slice};
use loadngo_proactor::{new_platform_proactor, PlatformPort, Proactor};

fn run(proactor: &Proactor<PlatformPort>, batch: Batch<'_>) -> Completed {
    let slot: Arc<Mutex<Option<Completed>>> = Arc::default();
    let filled = Arc::clone(&slot);
    batch.commit(&proactor.handle(), move |done| {
        *filled.lock().unwrap() = Some(done);
    });
    loop {
        proactor.run_once().unwrap();
        if let Some(done) = slot.lock().unwrap().take() {
            return done;
        }
    }
}

fn values(seed: u64, n: usize) -> Vec<f32> {
    let mut s = seed | 1;
    (0..n)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 40) as f32 / (1u64 << 23) as f32 - 1.0
        })
        .collect()
}

fn upload(gpu: &Gpu, v: &[f32]) -> Buffer {
    let mut buffer = gpu.buffer(v.len().max(1) * 4).unwrap();
    buffer.as_f32_mut()[..v.len()].copy_from_slice(v);
    buffer
}

/// Keys and values for every absolute position, then laid into a ring of `slots` rows
/// holding the newest ones, as a decoder's cache would after reaching `start + t`. The
/// tiled kernel reads rows up to the next multiple of 32 (multiplied by zero): those are
/// zero; rows the plain kernel must never read are NaN.
fn ring(all: &[f32], s: GroupedShape, tiled: bool) -> Vec<f32> {
    let row = s.kv_heads * s.dim;
    let end = s.start + s.t;
    let rows = if !tiled {
        s.slots.min(end)
    } else if s.slots < end {
        s.slots
    } else {
        end.next_multiple_of(32)
    };
    let mut out = vec![if tiled { 0.0 } else { f32::NAN }; rows * row];
    for p in end.saturating_sub(s.slots)..end {
        let slot = p % s.slots;
        out[slot * row..(slot + 1) * row].copy_from_slice(&all[p * row..(p + 1) * row]);
    }
    out
}

fn reference(q: &[f32], k: &[f32], v: &[f32], s: GroupedShape) -> Vec<f32> {
    let row = s.kv_heads * s.dim;
    let group = s.heads / s.kv_heads;
    let mut out = vec![0.0; s.t * s.heads * s.dim];
    for i in 0..s.t {
        let last = s.start + i;
        let first = (last + 1).saturating_sub(s.window);
        for h in 0..s.heads {
            let g = h / group;
            let qt = &q[(i * s.heads + h) * s.dim..][..s.dim];
            let scores: Vec<f64> = (first..=last)
                .map(|p| {
                    let kr = &k[p * row + g * s.dim..][..s.dim];
                    qt.iter()
                        .zip(kr)
                        .map(|(a, b)| f64::from(*a) * f64::from(*b))
                        .sum::<f64>()
                        * f64::from(s.scale)
                })
                .collect();
            let m = scores.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            let z: f64 = scores.iter().map(|x| (x - m).exp()).sum();
            for d in 0..s.dim {
                let acc: f64 = scores
                    .iter()
                    .zip(first..=last)
                    .map(|(x, p)| (x - m).exp() / z * f64::from(v[p * row + g * s.dim + d]))
                    .sum();
                out[(i * s.heads + h) * s.dim + d] = acc as f32;
            }
        }
    }
    out
}

fn max_error(
    gpu: &Gpu,
    proactor: &Proactor<PlatformPort>,
    s: GroupedShape,
    sharp: f32,
    tiled: bool,
) -> f32 {
    let end = s.start + s.t;
    let rows = if tiled { s.t.next_multiple_of(32) } else { s.t };
    let mut q: Vec<f32> = values(1, s.t * s.heads * s.dim)
        .iter()
        .map(|x| x * sharp)
        .collect();
    let want_q = q.clone();
    q.resize(rows * s.heads * s.dim, 0.0);
    let k = values(2, end * s.kv_heads * s.dim);
    let v = values(3, end * s.kv_heads * s.dim);
    let (kr, vr) = (ring(&k, s, tiled), ring(&v, s, tiled));
    let out_len = rows * s.heads * s.dim * 4;
    let buffers = vec![
        upload(gpu, &q),
        upload(gpu, &kr),
        upload(gpu, &vr),
        gpu.buffer(out_len).unwrap(),
    ];
    let mut batch = gpu.batch(buffers, Dispatch::Serial).unwrap();
    let (qs, ks, vs, os) = (
        Slice::new(0, 0, q.len() * 4),
        Slice::new(1, 0, kr.len() * 4),
        Slice::new(2, 0, vr.len() * 4),
        Slice::new(3, 0, out_len),
    );
    if tiled {
        batch.attention_grouped_tiled(qs, ks, vs, os, s)
    } else {
        batch.attention_grouped(qs, ks, vs, os, s)
    }
    .unwrap();
    let done = run(proactor, batch);
    done.gpu_time.unwrap();
    let want = reference(&want_q, &k, &v, s);
    done.buffers[3].as_f32()[..want.len()]
        .iter()
        .zip(&want)
        .map(|(g, w)| (g - w).abs())
        .fold(0.0, |worst, e| {
            if e.is_nan() {
                f32::INFINITY
            } else {
                worst.max(e)
            }
        })
}

#[allow(clippy::too_many_arguments)]
fn shape(
    t: usize,
    start: usize,
    heads: usize,
    kv_heads: usize,
    dim: usize,
    window: usize,
    slots: usize,
) -> GroupedShape {
    GroupedShape {
        t,
        start,
        heads,
        kv_heads,
        dim,
        window,
        slots,
        scale: 1.0,
    }
}

#[test]
fn matches_the_float64_definition() {
    let gpu = Gpu::new().unwrap();
    let proactor = new_platform_proactor().unwrap();
    let full = usize::MAX;
    for (s, sharp) in [
        // Gemma 4 31B's full layers: 32 heads over 4 KV heads, 512 wide, values from keys.
        (shape(1, 0, 32, 4, 512, full, 1), 1.0),
        (shape(1, 700, 32, 4, 512, full, 701), 0.05),
        (shape(37, 300, 32, 4, 512, full, 337), 0.05),
        // Its sliding layers: 32 over 16, 256 wide, window 1024 in a ring of 1024 + 512.
        (shape(1, 3000, 32, 16, 256, 1024, 1536), 0.08),
        (shape(200, 1400, 32, 16, 256, 1024, 1536), 0.08),
        (shape(512, 0, 32, 16, 256, 1024, 1536), 0.08),
        // A small window and ring, wrapping many times; one KV head; sharp scores.
        (shape(5, 97, 4, 1, 64, 7, 12), 3.0),
        (shape(3, 2, 8, 2, 32, 16, 40), 1.0),
    ] {
        let error = max_error(&gpu, &proactor, s, sharp, false);
        assert!(error < 2e-5, "{s:?} sharp {sharp}: max error {error}");
    }
}

#[test]
fn tiled_matches_the_float64_definition() {
    let gpu = Gpu::new().unwrap();
    let proactor = new_platform_proactor().unwrap();
    let full = usize::MAX;
    for (s, sharp) in [
        // Full layers: a whole first pass, a partial pass after a cache, 33 positions.
        (shape(512, 0, 32, 4, 512, full, 512), 0.05),
        (shape(77, 700, 32, 4, 512, full, 4096), 0.05),
        (shape(33, 5, 32, 4, 512, full, 40), 0.05),
        // Sliding layers in their ring of 1024 + 512, wrapped, and a first pass.
        (shape(512, 2600, 32, 16, 256, 1024, 1536), 0.08),
        (shape(200, 1400, 32, 16, 256, 1024, 1536), 0.08),
        (shape(512, 0, 32, 16, 256, 1024, 1536), 0.08),
        // A small window inside one tile, a ring of 64, sharp scores.
        (shape(40, 97, 4, 1, 64, 7, 64), 3.0),
    ] {
        let error = max_error(&gpu, &proactor, s, sharp, true);
        // Matrix-unit products sum in another order than the float64 reference.
        assert!(error < 1e-4, "{s:?} sharp {sharp}: max error {error}");
    }
}

#[test]
fn rejects_shapes_it_cannot_run() {
    let gpu = Gpu::new().unwrap();
    for s in [
        shape(1, 0, 6, 4, 64, 8, 8),    // heads not a multiple of KV heads
        shape(1, 0, 4, 4, 48, 8, 8),    // dim not a multiple of 32
        shape(1, 0, 4, 4, 544, 8, 8),   // dim over 512
        shape(4, 100, 4, 4, 64, 8, 10), // ring smaller than window + t
        shape(0, 0, 4, 4, 64, 8, 8),    // nothing to do
    ] {
        let buffers = (0..4).map(|_| gpu.buffer(1 << 20).unwrap()).collect();
        let mut batch = gpu.batch(buffers, Dispatch::Serial).unwrap();
        let slice = |b| Slice::new(b, 0, 1 << 20);
        assert!(
            batch
                .attention_grouped(slice(0), slice(1), slice(2), slice(3), s)
                .is_err(),
            "{s:?}"
        );
    }
}
