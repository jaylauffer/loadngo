//! Timing of the multi-position kernels against per-position products. Run by hand:
//! `cargo test --release -p loadngo-metal-compute --test gemm_timing -- --ignored --nocapture`
#![cfg(target_os = "macos")]

use std::sync::{Arc, Mutex};

use loadngo_metal_compute::{Batch, Completed, Dispatch, Gpu, Slice};
use loadngo_proactor::{new_platform_proactor, PlatformPort, Proactor};

fn run(proactor: &Proactor<PlatformPort>, batch: Batch<'_>) -> Completed {
    let slot: Arc<Mutex<Option<Completed>>> = Arc::default();
    let filled = Arc::clone(&slot);
    batch.commit(&proactor.handle(), move |done| {
        *filled.lock().unwrap() = Some(done)
    });
    loop {
        proactor.run_once().unwrap();
        if let Some(done) = slot.lock().unwrap().take() {
            return done;
        }
    }
}

#[test]
#[ignore = "timing, run by hand"]
fn time_multi_position_kernels() {
    let gpu = Gpu::new().unwrap();
    let proactor = new_platform_proactor().unwrap();
    for (mx, rows, cols, n) in [
        (false, 4096, 2304, 1),
        (false, 4096, 2304, 8),
        (false, 4096, 2304, 70),
        (true, 1024, 2304, 1),
        (true, 1024, 2304, 2),
        (true, 1024, 2304, 8),
        (true, 1024, 2304, 70),
    ] {
        let wbytes = if mx { rows * cols / 2 } else { rows * cols * 2 };
        let mut w = gpu.buffer(wbytes).unwrap();
        for (i, b) in w.as_bytes_mut().iter_mut().enumerate() {
            *b = (i * 7 % 251) as u8 & 0x3f;
        }
        let scales = gpu.buffer(rows * cols / 32).unwrap();
        let mut x = gpu.buffer(n * cols * 4).unwrap();
        x.as_f32_mut().fill(0.5);
        let y = gpu.buffer(n * rows * 4).unwrap();
        let mut buffers = vec![w, scales, x, y];
        let mut best = [f64::MAX; 2];
        for _ in 0..6 {
            for (k, per_position) in [false, true].into_iter().enumerate() {
                let mut batch = gpu
                    .batch(std::mem::take(&mut buffers), Dispatch::Concurrent)
                    .unwrap();
                for _ in 0..20 {
                    if per_position || n == 1 {
                        for p in 0..n {
                            let xs = Slice::new(2, p * cols * 4, cols * 4);
                            let ys = Slice::new(3, p * rows * 4, rows * 4);
                            if mx {
                                batch
                                    .gemv_mxfp4(
                                        Slice::new(0, 0, wbytes),
                                        Slice::new(1, 0, rows * cols / 32),
                                        xs,
                                        ys,
                                        rows,
                                        cols,
                                    )
                                    .unwrap();
                            } else {
                                batch
                                    .gemv_bf16(Slice::new(0, 0, wbytes), xs, ys, rows, cols)
                                    .unwrap();
                            }
                        }
                    } else {
                        let xs = Slice::new(2, 0, n * cols * 4);
                        let ys = Slice::new(3, 0, n * rows * 4);
                        if mx {
                            batch
                                .gemm_mxfp4(
                                    Slice::new(0, 0, wbytes),
                                    Slice::new(1, 0, rows * cols / 32),
                                    xs,
                                    ys,
                                    rows,
                                    cols,
                                    n,
                                    (cols, rows),
                                )
                                .unwrap();
                        } else {
                            batch
                                .gemm_bf16(
                                    Slice::new(0, 0, wbytes),
                                    xs,
                                    ys,
                                    rows,
                                    cols,
                                    n,
                                    (cols, rows),
                                )
                                .unwrap();
                        }
                    }
                    batch.barrier();
                }
                let done = run(&proactor, batch);
                best[k] = best[k].min(done.gpu_time.unwrap().as_secs_f64() / 20.0);
                buffers = done.buffers;
            }
        }
        println!(
            "{} {rows}x{cols} n={n:<3} multi {:>8.1} us   per-position {:>8.1} us",
            if mx { "mxfp4" } else { "bf16 " },
            best[0] * 1e6,
            best[1] * 1e6
        );
    }
}

#[test]
#[ignore = "timing, run by hand"]
fn time_submission_against_dispatch_count() {
    let gpu = Gpu::new().unwrap();
    let proactor = new_platform_proactor().unwrap();
    let (rows, cols) = (1024, 2304);
    let packed = vec![0x21u8; rows * cols / 2];
    let scales = vec![127u8; rows * cols / 32];
    let experts: Vec<_> = (0..64)
        .map(|_| {
            (
                gpu.resident(&packed).unwrap(),
                gpu.resident(&scales).unwrap(),
            )
        })
        .collect();
    let mut x = gpu.buffer(cols * 4).unwrap();
    x.as_f32_mut().fill(0.25);
    let mut buffers = vec![x, gpu.buffer(4096 * rows * 4).unwrap()];
    for count in [1, 10, 100, 300, 1000] {
        let mut best = (f64::MAX, f64::MAX);
        for _ in 0..5 {
            let start = std::time::Instant::now();
            let mut batch = gpu
                .batch(std::mem::take(&mut buffers), Dispatch::Concurrent)
                .unwrap();
            for i in 0..count {
                let (e, s) = &experts[i % experts.len()];
                let (e, s) = (batch.attach(e), batch.attach(s));
                batch
                    .gemv_mxfp4(
                        e,
                        s,
                        Slice::new(0, 0, cols * 4),
                        Slice::new(1, i * rows * 4, rows * 4),
                        rows,
                        cols,
                    )
                    .unwrap();
            }
            let done = run(&proactor, batch);
            let wall = start.elapsed().as_secs_f64();
            best = (
                best.0.min(wall),
                best.1.min(done.gpu_time.unwrap().as_secs_f64()),
            );
            buffers = done.buffers;
        }
        println!(
            "{count:>5} dispatches: wall {:>8.2} ms, GPU {:>8.2} ms",
            best.0 * 1e3,
            best.1 * 1e3
        );
    }
}

#[test]
#[ignore = "timing, run by hand"]
fn time_first_use_of_fresh_arenas() {
    let gpu = Gpu::new().unwrap();
    let proactor = new_platform_proactor().unwrap();
    let (rows, cols) = (1024, 2304);
    let packed = vec![0x21u8; rows * cols / 2];
    let scales = vec![127u8; rows * cols / 32];
    // ~8 GB of residents, as ~6,400 experts' worth of matrices across ~8 arenas.
    let experts: Vec<_> = (0..6400)
        .map(|_| {
            (
                gpu.resident(&packed).unwrap(),
                gpu.resident(&scales).unwrap(),
            )
        })
        .collect();
    let mut x = gpu.buffer(cols * 4).unwrap();
    x.as_f32_mut().fill(0.25);
    let mut buffers = vec![x, gpu.buffer(400 * rows * 4).unwrap()];
    for round in 0..4 {
        let start = std::time::Instant::now();
        let mut batch = gpu
            .batch(std::mem::take(&mut buffers), Dispatch::Concurrent)
            .unwrap();
        for i in 0..400 {
            let (e, s) = &experts[(round * 400 + i * 16) % experts.len()];
            let (e, s) = (batch.attach(e), batch.attach(s));
            batch
                .gemv_mxfp4(
                    e,
                    s,
                    Slice::new(0, 0, cols * 4),
                    Slice::new(1, i * rows * 4, rows * 4),
                    rows,
                    cols,
                )
                .unwrap();
        }
        let done = run(&proactor, batch);
        println!(
            "round {round}: wall {:>8.2} ms, GPU {:>8.2} ms",
            start.elapsed().as_secs_f64() * 1e3,
            done.gpu_time.unwrap().as_secs_f64() * 1e3
        );
        buffers = done.buffers;
    }
}

#[test]
#[ignore = "timing, run by hand"]
fn time_submission_after_cpu_gaps() {
    let gpu = Gpu::new().unwrap();
    let proactor = new_platform_proactor().unwrap();
    let (rows, cols) = (1024, 2304);
    let packed = gpu.resident(&vec![0x21u8; rows * cols / 2]).unwrap();
    let scales = gpu.resident(&vec![127u8; rows * cols / 32]).unwrap();
    let mut x = gpu.buffer(cols * 4).unwrap();
    x.as_f32_mut().fill(0.25);
    let mut buffers = vec![x, gpu.buffer(100 * rows * 4).unwrap()];
    for gap_ms in [0u64, 1, 5, 10, 20, 50] {
        let mut extra = Vec::new();
        for _ in 0..8 {
            // CPU busy (not sleeping), as the model's attention code is between steps.
            let until = std::time::Instant::now() + std::time::Duration::from_millis(gap_ms);
            let mut spin = 0u64;
            while std::time::Instant::now() < until {
                spin = spin.wrapping_add(1);
            }
            std::hint::black_box(spin);
            let start = std::time::Instant::now();
            let mut batch = gpu
                .batch(std::mem::take(&mut buffers), Dispatch::Concurrent)
                .unwrap();
            for i in 0..100 {
                let (e, s) = (batch.attach(&packed), batch.attach(&scales));
                batch
                    .gemv_mxfp4(
                        e,
                        s,
                        Slice::new(0, 0, cols * 4),
                        Slice::new(1, i * rows * 4, rows * 4),
                        rows,
                        cols,
                    )
                    .unwrap();
            }
            let done = run(&proactor, batch);
            extra.push((start.elapsed() - done.gpu_time.unwrap()).as_secs_f64() * 1e3);
            buffers = done.buffers;
        }
        extra.sort_by(f64::total_cmp);
        println!(
            "gap {gap_ms:>3} ms: wall minus GPU, median {:>6.2} ms, max {:>6.2} ms",
            extra[4], extra[7]
        );
    }
}
