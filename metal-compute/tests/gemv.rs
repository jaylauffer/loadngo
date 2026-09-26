//! GPU results against the CPU reference in `loadngo-weights`, on shapes that exercise
//! every kernel path, plus dispatch validation and completion through the proactor.
#![cfg(target_os = "macos")]

use std::sync::{Arc, Mutex};

use loadngo_metal_compute::{Batch, Buffer, Completed, Dispatch, Error, Gpu, Rows, Slice};
use loadngo_proactor::{new_platform_proactor, PlatformPort, Proactor};
use loadngo_weights::dense::{HalfFormat, HalfMatrix};
use loadngo_weights::mxfp4::{quantize_block, Mxfp4Matrix, BLOCK_SIZE};

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

fn bf16_bytes(v: &[f32]) -> Vec<u8> {
    v.iter()
        .flat_map(|x| ((x.to_bits() >> 16) as u16).to_le_bytes())
        .collect()
}

fn mxfp4_bytes(v: &[f32], rows: usize, cols: usize) -> (Vec<u8>, Vec<u8>) {
    let row_bytes = Mxfp4Matrix::element_bytes_per_row(cols);
    let mut elements = vec![0u8; rows * row_bytes];
    let mut scales = Vec::with_capacity(rows * Mxfp4Matrix::scales_per_row(cols));
    for r in 0..rows {
        let row = &v[r * cols..(r + 1) * cols];
        for (b, block) in row.chunks(BLOCK_SIZE).enumerate() {
            let at = r * row_bytes + b * BLOCK_SIZE / 2;
            let len = block.len().div_ceil(2);
            scales.push(quantize_block(block, &mut elements[at..at + len]));
        }
    }
    (elements, scales)
}

fn upload(gpu: &Gpu, bytes: &[u8]) -> Buffer {
    let mut buffer = gpu.buffer(bytes.len()).unwrap();
    buffer.as_bytes_mut().copy_from_slice(bytes);
    buffer
}

fn upload_f32(gpu: &Gpu, v: &[f32]) -> Buffer {
    upload(
        gpu,
        &v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<_>>(),
    )
}

/// Worst-case float32 summation bound for one row, against an exact (f64) reference.
fn assert_close(got: &[f32], want: &[f32], magnitude: &[f64], cols: usize) {
    for (r, ((&g, &w), &m)) in got.iter().zip(want).zip(magnitude).enumerate() {
        let bound = cols as f64 * f64::from(f32::EPSILON) * m + 1e-30;
        let error = (f64::from(g) - f64::from(w)).abs();
        assert!(
            error <= bound,
            "row {r}: GPU {g}, reference {w}, bound {bound:e}"
        );
    }
}

fn magnitudes(
    rows: usize,
    cols: usize,
    x: &[f32],
    row_into: impl Fn(usize, &mut [f32]),
) -> Vec<f64> {
    let mut row = vec![0.0; cols];
    (0..rows)
        .map(|r| {
            row_into(r, &mut row);
            row.iter()
                .zip(x)
                .map(|(&w, &xc)| (f64::from(w) * f64::from(xc)).abs())
                .sum()
        })
        .collect()
}

fn check_bf16(gpu: &Gpu, proactor: &Proactor<PlatformPort>, rows: usize, cols: usize) {
    let w = bf16_bytes(&values(rows as u64 * 31 + cols as u64, rows * cols));
    let x = values(7, cols);
    let batch_buffers = vec![
        upload(gpu, &w),
        upload_f32(gpu, &x),
        gpu.buffer(rows * 4).unwrap(),
    ];
    let mut batch = gpu.batch(batch_buffers, Dispatch::Serial).unwrap();
    batch
        .gemv_bf16(
            Slice::new(0, 0, w.len()),
            Slice::new(1, 0, cols * 4),
            Slice::new(2, 0, rows * 4),
            rows,
            cols,
        )
        .unwrap();
    let done = run(proactor, batch);
    done.gpu_time.unwrap();
    let m = HalfMatrix::new(&w, rows, cols, HalfFormat::Bf16).unwrap();
    let mut want = vec![0.0; rows];
    m.mul_vec(&mut want, &x);
    let magnitude = magnitudes(rows, cols, &x, |r, out| m.row_into(r, out));
    assert_close(done.buffers[2].as_f32(), &want, &magnitude, cols);
}

fn check_mxfp4(gpu: &Gpu, proactor: &Proactor<PlatformPort>, rows: usize, cols: usize) {
    let (elements, scales) = mxfp4_bytes(
        &values(rows as u64 * 17 + cols as u64, rows * cols),
        rows,
        cols,
    );
    let x = values(11, cols);
    let batch_buffers = vec![
        upload(gpu, &elements),
        upload(gpu, &scales),
        upload_f32(gpu, &x),
        gpu.buffer(rows * 4).unwrap(),
    ];
    let mut batch = gpu.batch(batch_buffers, Dispatch::Serial).unwrap();
    batch
        .gemv_mxfp4(
            Slice::new(0, 0, elements.len()),
            Slice::new(1, 0, scales.len()),
            Slice::new(2, 0, cols * 4),
            Slice::new(3, 0, rows * 4),
            rows,
            cols,
        )
        .unwrap();
    let done = run(proactor, batch);
    done.gpu_time.unwrap();
    let m = Mxfp4Matrix::new(&elements, &scales, rows, cols).unwrap();
    let mut want = vec![0.0; rows];
    m.mul_vec(&mut want, &x);
    let magnitude = magnitudes(rows, cols, &x, |r, out| m.dequantize_row(r, out));
    assert_close(done.buffers[3].as_f32(), &want, &magnitude, cols);
}

/// Vector paths (cols a multiple of 8 / 32), scalar paths (other widths), row counts that
/// leave partial simdgroups and threadgroups, for every rows-per-simdgroup variant.
#[test]
fn products_match_the_cpu_reference_on_every_path() {
    let mut gpu = Gpu::new().unwrap();
    let proactor = new_platform_proactor().unwrap();
    for rows_variant in Rows::ALL {
        gpu.set_rows(rows_variant);
        for (rows, cols) in [
            (1, 8),
            (37, 2304),
            (130, 1024),
            (5, 13),
            (33, 45),
            (64, 4096),
        ] {
            check_bf16(&gpu, &proactor, rows, cols);
        }
        for (rows, cols) in [(1, 32), (37, 2304), (130, 1024), (5, 13), (33, 45), (9, 70)] {
            check_mxfp4(&gpu, &proactor, rows, cols);
        }
    }
}

/// An E8M0 scale of 0xFF is NaN (OCP MX v1.0): it poisons its row and no other.
#[test]
fn a_nan_scale_poisons_only_its_row() {
    let gpu = Gpu::new().unwrap();
    let proactor = new_platform_proactor().unwrap();
    let (rows, cols) = (4, 64);
    let (elements, mut scales) = mxfp4_bytes(&values(3, rows * cols), rows, cols);
    scales[2 * 2 + 1] = 0xff; // row 2, second block
    let x = values(5, cols);
    let buffers = vec![
        upload(&gpu, &elements),
        upload(&gpu, &scales),
        upload_f32(&gpu, &x),
        gpu.buffer(rows * 4).unwrap(),
    ];
    let mut batch = gpu.batch(buffers, Dispatch::Serial).unwrap();
    batch
        .gemv_mxfp4(
            Slice::new(0, 0, elements.len()),
            Slice::new(1, 0, scales.len()),
            Slice::new(2, 0, cols * 4),
            Slice::new(3, 0, rows * 4),
            rows,
            cols,
        )
        .unwrap();
    let done = run(&proactor, batch);
    let y = done.buffers[3].as_f32();
    for (r, v) in y.iter().enumerate() {
        assert_eq!(v.is_nan(), r == 2, "row {r}: {v}");
    }
}

/// In a concurrent batch, a barrier orders a product after the one whose output it reads.
#[test]
fn a_barrier_orders_dependent_products() {
    let gpu = Gpu::new().unwrap();
    let proactor = new_platform_proactor().unwrap();
    let n = 512;
    let w1 = bf16_bytes(&values(21, n * n));
    let w2 = bf16_bytes(&values(22, n * n));
    let x = values(23, n);
    let buffers = vec![
        upload(&gpu, &w1),
        upload(&gpu, &w2),
        upload_f32(&gpu, &x),
        gpu.buffer(n * 4).unwrap(),
        gpu.buffer(n * 4).unwrap(),
    ];
    let mut batch = gpu.batch(buffers, Dispatch::Concurrent).unwrap();
    let whole = |b| Slice::new(b, 0, n * 4);
    batch
        .gemv_bf16(Slice::new(0, 0, w1.len()), whole(2), whole(3), n, n)
        .unwrap();
    batch.barrier();
    batch
        .gemv_bf16(Slice::new(1, 0, w2.len()), whole(3), whole(4), n, n)
        .unwrap();
    let done = run(&proactor, batch);
    done.gpu_time.unwrap();

    let middle = done.buffers[3].as_f32();
    let m2 = HalfMatrix::new(&w2, n, n, HalfFormat::Bf16).unwrap();
    let mut want = vec![0.0; n];
    m2.mul_vec(&mut want, middle);
    let magnitude = magnitudes(n, n, middle, |r, out| m2.row_into(r, out));
    assert_close(done.buffers[4].as_f32(), &want, &magnitude, n);
}

#[test]
fn invalid_dispatches_are_refused_before_encoding() {
    let gpu = Gpu::new().unwrap();
    let proactor = new_platform_proactor().unwrap();
    let (rows, cols) = (8, 64);
    let buffers = vec![
        gpu.buffer(rows * cols * 2 + 16).unwrap(),
        gpu.buffer(cols * 4 + 16).unwrap(),
        gpu.buffer(rows * 4).unwrap(),
    ];
    let mut batch = gpu.batch(buffers, Dispatch::Serial).unwrap();
    let w = Slice::new(0, 0, rows * cols * 2);
    let x = Slice::new(1, 0, cols * 4);
    let y = Slice::new(2, 0, rows * 4);
    let refused = |r: Result<(), Error>| matches!(r, Err(Error::Dispatch(_)));
    assert!(
        refused(batch.gemv_bf16(w, x, y, rows, cols + 8)),
        "wrong length"
    );
    assert!(
        refused(batch.gemv_bf16(Slice::new(0, 8, w.len), x, y, rows, cols)),
        "misaligned"
    );
    assert!(
        refused(batch.gemv_bf16(Slice::new(0, 32, w.len), x, y, rows, cols)),
        "out of range"
    );
    assert!(
        refused(batch.gemv_bf16(Slice::new(3, 0, w.len), x, y, rows, cols)),
        "no buffer"
    );
    assert!(
        refused(batch.gemv_bf16(w, x, Slice::new(0, 0, rows * 4), rows, cols)),
        "overlap"
    );
    assert!(refused(batch.gemv_bf16(w, x, y, 0, cols)), "empty shape");
    assert_eq!(batch.dispatches(), 0);
    batch.gemv_bf16(w, x, y, rows, cols).unwrap();
    let done = run(&proactor, batch);
    done.gpu_time.unwrap();
    assert_eq!(done.buffers.len(), 3, "the batch hands every buffer back");
}

#[test]
fn a_batch_dropped_without_commit_is_harmless() {
    let gpu = Gpu::new().unwrap();
    let batch = gpu
        .batch(vec![gpu.buffer(64).unwrap()], Dispatch::Serial)
        .unwrap();
    drop(batch);
    assert!(matches!(gpu.buffer(0), Err(Error::Alloc(0))));
}
