//! The glue kernels (convolution with history, row norms, decay, sigmoid) against CPU
//! references, including a convolution split over calls shorter than its kernel.
#![cfg(target_os = "macos")]

use std::sync::{Arc, Mutex};

use loadngo_metal_compute::{Batch, Buffer, Completed, Dispatch, Gpu, Slice};
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

fn whole(index: usize, floats: usize) -> Slice {
    Slice::new(index, 0, floats * 4)
}

fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

fn assert_close(got: &[f32], want: &[f32], tolerance: f32, what: &str) {
    assert_eq!(got.len(), want.len(), "{what}");
    for (i, (&g, &w)) in got.iter().zip(want).enumerate() {
        let bound = tolerance * (1.0 + w.abs());
        assert!(
            (g - w).abs() <= bound,
            "{what}[{i}]: GPU {g}, reference {w}"
        );
    }
}

/// The CPU convolution: channel by channel, history carried in `state`.
fn conv_reference(x: &mut [f32], taps: &[f32], state: &mut [f32], channels: usize, k: usize) {
    let hist = k - 1;
    let rows = x.len() / channels;
    for c in 0..channels {
        let mut buf = state[c * hist..(c + 1) * hist].to_vec();
        let t = &taps[c * k..(c + 1) * k];
        for r in 0..rows {
            let cur = x[r * channels + c];
            let mut acc = t[hist] * cur;
            for (&tap, &past) in t[..hist].iter().zip(&buf) {
                acc += tap * past;
            }
            buf.remove(0);
            buf.push(cur);
            x[r * channels + c] = acc * sigmoid(acc);
        }
        state[c * hist..(c + 1) * hist].copy_from_slice(&buf);
    }
}

#[test]
fn convolution_with_history_matches_across_calls() {
    let gpu = Gpu::new().unwrap();
    let proactor = new_platform_proactor().unwrap();
    let (channels, k) = (300, 4);
    let taps = values(1, channels * k);
    let mut want_state = values(2, channels * (k - 1));
    let mut state = want_state.clone();
    // Calls of 5, 1, 2 and 9 rows: two shorter than the kernel's history.
    for (call, rows) in [5, 1, 2, 9].into_iter().enumerate() {
        let x = values(10 + call as u64, rows * channels);
        let mut want = x.clone();
        conv_reference(&mut want, &taps, &mut want_state, channels, k);
        let buffers = vec![
            upload(&gpu, &x),
            upload(&gpu, &taps),
            upload(&gpu, &state),
            gpu.buffer(rows * channels * 4).unwrap(),
        ];
        let mut batch = gpu.batch(buffers, Dispatch::Concurrent).unwrap();
        batch
            .causal_conv_silu(
                (
                    whole(0, rows * channels),
                    whole(1, taps.len()),
                    whole(2, state.len()),
                ),
                whole(3, rows * channels),
                rows,
                channels,
                k,
            )
            .unwrap();
        batch.barrier();
        batch
            .causal_conv_history(
                whole(0, rows * channels),
                whole(2, state.len()),
                rows,
                channels,
                k,
            )
            .unwrap();
        let done = run(&proactor, batch);
        done.gpu_time.unwrap();
        assert_close(
            &done.buffers[3].as_f32()[..rows * channels],
            &want,
            1e-6,
            &format!("call {call} output"),
        );
        state = done.buffers[2].as_f32()[..state.len()].to_vec();
        // The history is copied, not computed: exact.
        assert_eq!(state, want_state, "call {call} history");
    }
}

#[test]
fn row_norms_decay_sigmoid_and_silu_match_their_definitions() {
    let gpu = Gpu::new().unwrap();
    let proactor = new_platform_proactor().unwrap();
    let (rows, d) = (37, 128);
    let v = values(3, rows * d);
    let gate = values(4, rows * d);
    let w = values(5, d);
    let (width, heads) = (256, 2);
    // Decay inputs spanning both softplus branches and the very negative range.
    let z: Vec<f32> = values(6, 3 * width).iter().map(|x| x * 30.0).collect();
    let a_log = values(7, heads);
    let bias = values(8, width);
    let s = values(9, 100);
    let (g, u) = (values(11, 300), values(12, 300));
    let buffers = vec![
        upload(&gpu, &v),
        upload(&gpu, &v),
        upload(&gpu, &gate),
        upload(&gpu, &w),
        upload(&gpu, &z),
        upload(&gpu, &a_log),
        upload(&gpu, &bias),
        gpu.buffer(z.len() * 4).unwrap(),
        upload(&gpu, &s),
        upload(&gpu, &g),
        upload(&gpu, &u),
    ];
    let mut batch = gpu.batch(buffers, Dispatch::Concurrent).unwrap();
    batch
        .l2norm_rows(whole(0, v.len()), rows, d, 1e-6, 0.25)
        .unwrap();
    batch
        .rmsnorm_gated_rows(
            whole(1, v.len()),
            (whole(2, gate.len()), whole(3, d)),
            rows,
            d,
            1e-5,
        )
        .unwrap();
    batch
        .softplus_decay(
            (whole(4, z.len()), whole(5, heads), whole(6, width)),
            whole(7, z.len()),
            z.len(),
            width,
            width / heads,
        )
        .unwrap();
    batch.sigmoid_in_place(whole(8, s.len()), s.len()).unwrap();
    batch
        .silu_mul(whole(9, g.len()), whole(10, u.len()), g.len())
        .unwrap();
    let done = run(&proactor, batch);
    done.gpu_time.unwrap();

    let mut l2 = v.clone();
    let mut rms = v.clone();
    for r in 0..rows {
        let row = &v[r * d..(r + 1) * d];
        let ss: f64 = row.iter().map(|&x| f64::from(x) * f64::from(x)).sum();
        let inv_l2 = (1.0 / (ss + 1e-6).sqrt()) as f32;
        let inv_rms = (1.0 / (ss / d as f64 + 1e-5).sqrt()) as f32;
        for i in 0..d {
            l2[r * d + i] = row[i] * inv_l2 * 0.25;
            rms[r * d + i] = w[i] * row[i] * inv_rms * sigmoid(gate[r * d + i]);
        }
    }
    let alpha: Vec<f32> = z
        .iter()
        .enumerate()
        .map(|(i, &zi)| {
            let x = zi + bias[i % width];
            let sp = if x > 20.0 { x } else { x.exp().ln_1p() };
            (-a_log[(i % width) / (width / heads)].exp() * sp).exp()
        })
        .collect();
    let sig: Vec<f32> = s.iter().map(|&x| sigmoid(x)).collect();
    assert_close(&done.buffers[0].as_f32()[..v.len()], &l2, 2e-6, "l2norm");
    assert_close(
        &done.buffers[1].as_f32()[..v.len()],
        &rms,
        2e-6,
        "rmsnorm_gated",
    );
    assert_close(&done.buffers[7].as_f32()[..z.len()], &alpha, 2e-6, "decay");
    assert_close(&done.buffers[8].as_f32()[..s.len()], &sig, 1e-6, "sigmoid");
    let gated: Vec<f32> = g
        .iter()
        .zip(&u)
        .map(|(&x, &y)| x * sigmoid(x) * y)
        .collect();
    assert_close(
        &done.buffers[9].as_f32()[..g.len()],
        &gated,
        1e-6,
        "silu_mul",
    );
}

#[test]
fn strided_rmsnorm_and_row_copy_touch_only_their_columns() {
    let gpu = Gpu::new().unwrap();
    let proactor = new_platform_proactor().unwrap();
    // Rows of 20 floats: normalize the first 12, copy columns 12..20 elsewhere.
    let (rows, d, stride) = (9, 12, 20);
    let v = values(20, rows * stride);
    let w = values(21, d);
    let buffers = vec![
        upload(&gpu, &v),
        upload(&gpu, &w),
        gpu.buffer(rows * 16 * 4).unwrap(),
    ];
    let mut batch = gpu.batch(buffers, Dispatch::Concurrent).unwrap();
    // Copy first (reads columns the norm does not write), then normalize.
    batch
        .copy_rows(
            (Slice::new(0, d * 4, ((rows - 1) * stride + 8) * 4), stride),
            (Slice::new(2, 0, ((rows - 1) * 16 + 8) * 4), 16),
            rows,
            8,
        )
        .unwrap();
    batch
        .rmsnorm_rows(
            (Slice::new(0, 0, ((rows - 1) * stride + d) * 4), whole(1, d)),
            rows,
            (d, stride),
            1e-5,
        )
        .unwrap();
    let done = run(&proactor, batch);
    done.gpu_time.unwrap();
    let mut want = v.clone();
    for r in 0..rows {
        let row = &v[r * stride..r * stride + d];
        let ss: f64 = row.iter().map(|&x| f64::from(x) * f64::from(x)).sum();
        let inv = (1.0 / (ss / d as f64 + 1e-5).sqrt()) as f32;
        for i in 0..d {
            want[r * stride + i] = w[i] * row[i] * inv;
        }
    }
    assert_close(
        &done.buffers[0].as_f32()[..v.len()],
        &want,
        2e-6,
        "rmsnorm_rows",
    );
    let copied = done.buffers[2].as_f32();
    for r in 0..rows {
        assert_eq!(
            &copied[r * 16..r * 16 + 8],
            &v[r * stride + d..r * stride + d + 8],
            "row {r}"
        );
    }
}
