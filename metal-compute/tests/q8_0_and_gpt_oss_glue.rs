//! The `Q8_0` products (one position, several, and tiled) and the glue gpt-oss needs
//! (row adds, rotary by halves, the clamped SwiGLU) against float64 references.
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

fn upload_bytes(gpu: &Gpu, v: &[u8]) -> Buffer {
    let mut buffer = gpu.buffer(v.len().max(16)).unwrap();
    buffer.as_bytes_mut()[..v.len()].copy_from_slice(v);
    buffer
}

/// A repacked `Q8_0` matrix: codes, binary16 scale bytes, and every element's value.
/// Scales are exact binary16 values of either sign, small to large.
fn q8_matrix(rows: usize, cols: usize, seed: u64) -> (Vec<u8>, Vec<u8>, Vec<f64>) {
    // (binary16 bits, value)
    const SCALES: [(u16, f64); 6] = [
        (0x3800, 0.5),
        (0x2000, 0.007_812_5),
        (0xB600, -0.375),
        (0x3C00, 1.0),
        (0x1400, 0.000_976_562_5),
        (0x4100, 2.5),
    ];
    let codes: Vec<u8> = values(seed, rows * cols)
        .iter()
        .map(|v| ((v * 127.9) as i8) as u8)
        .collect();
    let mut scale_bytes = Vec::new();
    let mut scale_values = Vec::new();
    for (i, v) in values(seed + 1, rows * cols / 32).iter().enumerate() {
        let (bits, value) = SCALES[(((v + 1.0) * 3.0) as usize + i) % 6];
        scale_bytes.extend_from_slice(&bits.to_le_bytes());
        scale_values.push(value);
    }
    let elements = codes
        .iter()
        .enumerate()
        .map(|(i, &c)| f64::from(c as i8) * scale_values[i / 32])
        .collect();
    (codes, scale_bytes, elements)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Gemv,
    Gemm,
    Tiled,
}

/// Largest error of `y = W x` over `n` positions, relative to the largest `|y|`.
fn q8_error(
    gpu: &Gpu,
    proactor: &Proactor<PlatformPort>,
    rows: usize,
    cols: usize,
    n: usize,
    kind: Kind,
) -> f64 {
    let (codes, scales, w) = q8_matrix(rows, cols, 7 + rows as u64);
    let x = values(11, n * cols);
    let buffers = vec![
        upload_bytes(gpu, &codes),
        upload_bytes(gpu, &scales),
        upload(gpu, &x),
        gpu.buffer(n * rows * 4).unwrap(),
    ];
    let mut batch = gpu.batch(buffers, Dispatch::Serial).unwrap();
    let matrix = (
        Slice::new(0, 0, codes.len()),
        Slice::new(1, 0, scales.len()),
    );
    let (xs, ys) = (
        Slice::new(2, 0, x.len() * 4),
        Slice::new(3, 0, n * rows * 4),
    );
    match kind {
        Kind::Gemv => batch.gemv_q8_0(matrix, xs, ys, rows, cols),
        Kind::Gemm => batch.gemm_q8_0(matrix, xs, ys, rows, cols, n, (cols, rows)),
        Kind::Tiled => batch.gemm_q8_0_tiled(matrix, xs, ys, rows, cols, n, (cols, rows)),
    }
    .unwrap();
    let done = run(proactor, batch);
    done.gpu_time.unwrap();
    let got = done.buffers[3].as_f32();
    let mut worst = 0.0_f64;
    let mut largest = 0.0_f64;
    for p in 0..n {
        for r in 0..rows {
            let want: f64 = (0..cols)
                .map(|c| w[r * cols + c] * f64::from(x[p * cols + c]))
                .sum();
            largest = largest.max(want.abs());
            worst = worst.max((f64::from(got[p * rows + r]) - want).abs());
        }
    }
    worst / largest
}

#[test]
fn q8_0_products_match_the_float64_definition() {
    let gpu = Gpu::new().unwrap();
    let proactor = new_platform_proactor().unwrap();
    // gpt-oss's shapes: q (4096 x 2880), k and v (512 x 2880), o (2880 x 4096); and odd
    // row counts for the plain kernels.
    for (rows, cols, n, kind) in [
        (4096, 2880, 1, Kind::Gemv),
        (512, 2880, 1, Kind::Gemv),
        (37, 96, 1, Kind::Gemv),
        (512, 2880, 5, Kind::Gemm),
        (37, 96, 13, Kind::Gemm),
        (2880, 4096, 32, Kind::Tiled),
        (512, 2880, 96, Kind::Tiled),
    ] {
        let error = q8_error(&gpu, &proactor, rows, cols, n, kind);
        assert!(
            error < 1e-5,
            "{rows} x {cols}, {n} positions, {kind:?}: relative error {error:e}"
        );
    }
}

#[test]
fn q8_0_rejects_partial_blocks() {
    let gpu = Gpu::new().unwrap();
    let buffers = (0..4).map(|_| gpu.buffer(1 << 16).unwrap()).collect();
    let mut batch = gpu.batch(buffers, Dispatch::Serial).unwrap();
    let s = |b, len| Slice::new(b, 0, len);
    assert!(batch
        .gemv_q8_0(
            (s(0, 4 * 40), s(1, 4 * 40 / 16)),
            s(2, 160),
            s(3, 16),
            4,
            40
        )
        .is_err());
    assert!(batch
        .gemm_q8_0_tiled(
            (s(0, 64 * 32), s(1, 128)),
            s(2, 31 * 128),
            s(3, 31 * 256),
            64,
            32,
            31,
            (32, 64)
        )
        .is_err());
}

#[test]
fn glue_adds_rotates_and_gates_as_defined() {
    let gpu = Gpu::new().unwrap();
    let proactor = new_platform_proactor().unwrap();
    let (rows, heads, dim, width) = (3, 4, 64, 96);
    let x = values(1, rows * heads * dim);
    let bias = values(2, width);
    let h = values(3, rows * width);
    let table: Vec<f32> = (0..rows * dim / 2)
        .flat_map(|i| {
            let angle = i as f64 * 0.37;
            [angle.cos() as f32 * 1.3, angle.sin() as f32 * 1.3]
        })
        .collect();
    let gate: Vec<f32> = values(4, rows * width).iter().map(|v| v * 12.0).collect();
    let up: Vec<f32> = values(5, rows * width).iter().map(|v| v * 12.0).collect();
    let (gb, ub) = (values(6, width), values(7, width));
    let buffers = vec![
        upload(&gpu, &x),
        upload(&gpu, &table),
        upload(&gpu, &h),
        upload(&gpu, &bias),
        upload(&gpu, &gate),
        upload(&gpu, &up),
        upload(&gpu, &gb),
        upload(&gpu, &ub),
        gpu.buffer(rows * width * 4).unwrap(),
    ];
    let mut batch = gpu.batch(buffers, Dispatch::Serial).unwrap();
    let f = |b: usize, n: usize| Slice::new(b, 0, n * 4);
    batch
        .rotate_halves(f(0, x.len()), f(1, table.len()), rows, (heads, dim))
        .unwrap();
    batch
        .add_rows(f(2, h.len()), (f(3, width), 0), rows, width)
        .unwrap();
    batch
        .clamped_swiglu(
            (f(4, gate.len()), f(5, up.len())),
            (f(6, width), f(7, width)),
            f(8, rows * width),
            rows,
            width,
            (7.0, 1.702),
        )
        .unwrap();
    let done = run(&proactor, batch);
    done.gpu_time.unwrap();

    let half = dim / 2;
    let rotated = done.buffers[0].as_f32();
    for r in 0..rows {
        for hd in 0..heads {
            for i in 0..half {
                let (c, s) = (
                    f64::from(table[(r * half + i) * 2]),
                    f64::from(table[(r * half + i) * 2 + 1]),
                );
                let base = (r * heads + hd) * dim;
                let (a, b) = (f64::from(x[base + i]), f64::from(x[base + i + half]));
                assert!((f64::from(rotated[base + i]) - (a * c - b * s)).abs() < 1e-6);
                assert!((f64::from(rotated[base + i + half]) - (b * c + a * s)).abs() < 1e-6);
            }
        }
    }
    let added = done.buffers[2].as_f32();
    for (i, &got) in added.iter().take(rows * width).enumerate() {
        assert_eq!(got, h[i] + bias[i % width]);
    }
    let gated = done.buffers[8].as_f32();
    let mut clamped = 0;
    for i in 0..rows * width {
        let g = (f64::from(gate[i]) + f64::from(gb[i % width])).min(7.0);
        let u = (f64::from(up[i]) + f64::from(ub[i % width])).clamp(-7.0, 7.0);
        let want = (u + 1.0) * g / (1.0 + (-1.702 * g).exp());
        assert!(
            (f64::from(gated[i]) - want).abs() < 1e-5 * want.abs().max(1.0),
            "{i}"
        );
        clamped += usize::from(g == 7.0) + usize::from(u.abs() == 7.0);
    }
    assert!(clamped > 20, "the clamps were exercised {clamped} times");
}
