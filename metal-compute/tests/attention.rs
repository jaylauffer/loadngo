//! `attention_split_key` against a float64 reference: short and long caches, several
//! heads, partial position blocks, sharp and flat softmaxes, and shape validation.
#![cfg(target_os = "macos")]

use std::sync::{Arc, Mutex};

use loadngo_metal_compute::{AttentionShape, Batch, Buffer, Completed, Dispatch, Gpu, Slice};
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

/// Small deterministic values in [-1, 1).
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

/// The definition, in float64: scores, softmax over positions `0..=cached + i`, values.
fn reference(q: &[f32], kv: &[f32], shared: &[f32], s: AttentionShape) -> Vec<f32> {
    let (dq, row) = (s.qa + s.qb, s.qa + s.dv);
    let mut out = vec![0.0; s.t * s.heads * s.dv];
    for i in 0..s.t {
        for h in 0..s.heads {
            let qt = &q[(i * s.heads + h) * dq..][..dq];
            let scores: Vec<f64> = (0..=s.cached + i)
                .map(|p| {
                    let k = &kv[(p * s.heads + h) * row..];
                    let r = &shared[p * s.qb..];
                    let a: f64 = (0..s.qa).map(|d| f64::from(qt[d]) * f64::from(k[d])).sum();
                    let b: f64 = (0..s.qb)
                        .map(|d| f64::from(qt[s.qa + d]) * f64::from(r[d]))
                        .sum();
                    (a + b) * f64::from(s.scale)
                })
                .collect();
            let m = scores.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            let z: f64 = scores.iter().map(|x| (x - m).exp()).sum();
            for d in 0..s.dv {
                let v: f64 = scores
                    .iter()
                    .enumerate()
                    .map(|(p, x)| {
                        (x - m).exp() / z * f64::from(kv[(p * s.heads + h) * row + s.qa + d])
                    })
                    .sum();
                out[(i * s.heads + h) * s.dv + d] = v as f32;
            }
        }
    }
    out
}

/// Runs one dispatch and returns the largest absolute difference from the reference.
fn max_error(gpu: &Gpu, proactor: &Proactor<PlatformPort>, s: AttentionShape, sharp: f32) -> f32 {
    let positions = s.cached + s.t;
    let q: Vec<f32> = values(1, s.t * s.heads * (s.qa + s.qb))
        .iter()
        .map(|x| x * sharp)
        .collect();
    let kv = values(2, positions * s.heads * (s.qa + s.dv));
    let shared = values(3, positions * s.qb);
    let buffers = vec![
        upload(gpu, &q),
        upload(gpu, &kv),
        upload(gpu, &shared),
        gpu.buffer(s.t * s.heads * s.dv * 4).unwrap(),
    ];
    let lens = [
        q.len() * 4,
        kv.len() * 4,
        shared.len() * 4,
        s.t * s.heads * s.dv * 4,
    ];
    let mut batch = gpu.batch(buffers, Dispatch::Serial).unwrap();
    batch
        .attention_split_key(
            Slice::new(0, 0, lens[0]),
            Slice::new(1, 0, lens[1]),
            Slice::new(2, 0, lens[2]),
            Slice::new(3, 0, lens[3]),
            s,
        )
        .unwrap();
    let done = run(proactor, batch);
    done.gpu_time.unwrap();
    let want = reference(&q, &kv, &shared, s);
    done.buffers[3]
        .as_f32()
        .iter()
        .zip(&want)
        .map(|(g, w)| (g - w).abs())
        // NaN counts as the worst error (f32::max alone would skip it).
        .fold(0.0, |worst, e| {
            if e.is_nan() {
                f32::INFINITY
            } else {
                worst.max(e)
            }
        })
}

fn shape(t: usize, cached: usize, heads: usize, qa: usize, qb: usize, dv: usize) -> AttentionShape {
    AttentionShape {
        t,
        cached,
        heads,
        qa,
        qb,
        dv,
        scale: 1.0 / ((qa + qb) as f32).sqrt(),
    }
}

#[test]
fn matches_the_float64_definition() {
    let gpu = Gpu::new().unwrap();
    let proactor = new_platform_proactor().unwrap();
    for (s, sharp) in [
        // One new position on an empty and a filled cache (decode).
        (shape(1, 0, 4, 128, 64, 128), 1.0),
        (shape(1, 700, 4, 128, 64, 128), 1.0),
        // Partial blocks of 8 new positions, odd widths, a sharp softmax.
        (shape(13, 5, 3, 40, 24, 72), 8.0),
        (shape(37, 0, 2, 128, 64, 128), 8.0),
        // A long cache: the running maximum and sum over thousands of rows.
        // Decoding against a long cache (the split-walk kernel, below 8 new positions).
        (shape(3, 3000, 2, 128, 64, 128), 4.0),
        (shape(9, 3000, 2, 128, 64, 128), 4.0),
        // No shared key part.
        (shape(5, 11, 2, 96, 0, 32), 1.0),
    ] {
        let error = max_error(&gpu, &proactor, s, sharp);
        // Outputs are averages of values in [-1, 1); float32 scores and sums.
        assert!(error < 2e-5, "{s:?} sharp {sharp}: max error {error:e}");
    }
}

#[test]
fn rejects_unsupported_shapes_and_overlapping_output() {
    let gpu = Gpu::new().unwrap();
    let buffers = vec![gpu.buffer(1 << 20).unwrap()];
    let mut batch = gpu.batch(buffers, Dispatch::Serial).unwrap();
    let whole = |len| Slice::new(0, 0, len);
    let s = shape(1, 0, 1, 128, 64, 256);
    assert!(batch
        .attention_split_key(whole(0), whole(0), whole(0), whole(0), s)
        .is_err());
    let s = shape(2, 0, 1, 4, 0, 4);
    // q and out both at offset 0: out overlaps an input.
    assert!(batch
        .attention_split_key(
            Slice::new(0, 0, 32),
            Slice::new(0, 64, 64),
            Slice::new(0, 128, 0),
            Slice::new(0, 0, 32),
            s
        )
        .is_err());
    assert!(batch
        .attention_split_key(
            Slice::new(0, 0, 32),
            Slice::new(0, 64, 64),
            Slice::new(0, 128, 0),
            Slice::new(0, 256, 32),
            s
        )
        .is_ok());
}
