//! The arithmetic of the CPU forward pass, in `f32`.
//!
//! [`matmul`] splits a product's output rows across the machine's cores with scoped
//! threads that end with the call; the rest is plain loops. This is the reference path
//! the GPU path is checked against, so it favours clarity over speed.

use std::thread;

fn threads() -> usize {
    thread::available_parallelism().map_or(1, usize::from)
}

/// The dot product of two equal-length slices, eight lanes at a time.
#[must_use]
pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    let mut acc = [0.0_f32; 8];
    let (a8, a_rest) = a.as_chunks::<8>();
    let (b8, b_rest) = b[..a.len()].as_chunks::<8>();
    for (x, y) in a8.iter().zip(b8) {
        for k in 0..8 {
            acc[k] += x[k] * y[k];
        }
    }
    let mut sum = ((acc[0] + acc[4]) + (acc[1] + acc[5])) + ((acc[2] + acc[6]) + (acc[3] + acc[7]));
    for (x, y) in a_rest.iter().zip(b_rest) {
        sum += x * y;
    }
    sum
}

/// `x W^T`: `x` is `t x cols`, `w` is `rows x cols`; the result is `t x rows`.
#[must_use]
pub fn matmul(x: &[f32], t: usize, w: &[f32], rows: usize, cols: usize) -> Vec<f32> {
    debug_assert_eq!(x.len(), t * cols);
    debug_assert_eq!(w.len(), rows * cols);
    let mut out = vec![0.0_f32; t * rows];
    // Each thread owns a band of output rows, computed for every position, so a weight
    // row is read once per band while the positions stream past it.
    let bands = threads().min(rows).max(1);
    let per = rows.div_ceil(bands);
    let mut columns: Vec<Vec<f32>> = (0..bands).map(|_| Vec::new()).collect();
    thread::scope(|scope| {
        for (band, column) in columns.iter_mut().enumerate() {
            let start = band * per;
            let end = (start + per).min(rows);
            if start >= end {
                continue;
            }
            scope.spawn(move || {
                column.reserve_exact(t * (end - start));
                for p in 0..t {
                    let xp = &x[p * cols..(p + 1) * cols];
                    for r in start..end {
                        column.push(dot(xp, &w[r * cols..(r + 1) * cols]));
                    }
                }
            });
        }
    });
    for (band, column) in columns.iter().enumerate() {
        let start = band * per;
        let width = (start + per).min(rows).saturating_sub(start);
        for p in 0..t {
            if width > 0 {
                out[p * rows + start..p * rows + start + width]
                    .copy_from_slice(&column[p * width..(p + 1) * width]);
            }
        }
    }
    out
}

/// Runs `job(i, chunk)` on `n` disjoint chunks of `data` of `len` each, across threads.
pub fn par_chunks<F>(data: &mut [f32], len: usize, job: F)
where
    F: Fn(usize, &mut [f32]) + Sync,
{
    let n = data.len() / len;
    let groups = threads().min(n).max(1);
    let per = n.div_ceil(groups);
    let job = &job;
    thread::scope(|scope| {
        for (g, group) in data.chunks_mut(per * len).enumerate() {
            scope.spawn(move || {
                for (k, chunk) in group.chunks_mut(len).enumerate() {
                    job(g * per + k, chunk);
                }
            });
        }
    });
}

#[must_use]
pub fn silu(x: f32) -> f32 {
    x / (1.0 + (-x).exp())
}

#[must_use]
pub fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

/// `log(1 + e^x)`, as torch computes it (linear above 20).
#[must_use]
pub fn softplus(x: f32) -> f32 {
    if x > 20.0 {
        x
    } else {
        x.exp().ln_1p()
    }
}

/// `x / sqrt(mean(x^2) + eps)`, in place.
pub fn rms_normalize(x: &mut [f32], eps: f32) {
    #[allow(clippy::cast_precision_loss)]
    let mean = x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32;
    let scale = 1.0 / (mean + eps).sqrt();
    for v in x {
        *v *= scale;
    }
}

/// The zero-centred RMS norm of Qwen3.5: `normalized * (1 + w)`, in place.
pub fn rms_norm_centred(x: &mut [f32], w: &[f32], eps: f32) {
    rms_normalize(x, eps);
    for (v, w) in x.iter_mut().zip(w) {
        *v *= 1.0 + w;
    }
}

/// A softmax over `logits`, in place.
pub fn softmax(logits: &mut [f32]) {
    let max = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let mut sum = 0.0;
    for v in logits.iter_mut() {
        *v = (*v - max).exp();
        sum += *v;
    }
    for v in logits {
        *v /= sum;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matmul_matches_the_definition_for_odd_shapes() {
        let (t, rows, cols) = (3, 13, 21);
        #[allow(clippy::cast_precision_loss)]
        let x: Vec<f32> = (0..t * cols).map(|i| (i as f32 * 0.37).sin()).collect();
        #[allow(clippy::cast_precision_loss)]
        let w: Vec<f32> = (0..rows * cols).map(|i| (i as f32 * 0.11).cos()).collect();
        let y = matmul(&x, t, &w, rows, cols);
        for p in 0..t {
            for r in 0..rows {
                let want: f32 = (0..cols).map(|c| x[p * cols + c] * w[r * cols + c]).sum();
                assert!((y[p * rows + r] - want).abs() < 1e-4, "{p} {r}");
            }
        }
        let mut data = vec![0.0_f32; 12];
        #[allow(clippy::cast_precision_loss)]
        par_chunks(&mut data, 3, |i, c| c.fill(i as f32));
        assert_eq!(data, [0., 0., 0., 1., 1., 1., 2., 2., 2., 3., 3., 3.]);
    }
}
