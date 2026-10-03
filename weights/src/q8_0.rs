//! ggml's `Q8_0`: blocks of 32 elements, each an IEEE binary16 scale `d` followed by 32
//! signed bytes `q`; element `i` is `d * q[i]` (ggml `docs/gguf.md` and its type table).
//!
//! Every value is exact in `f32`: an `f16` has an 11-bit significand and `q` at most 8
//! bits, so their product needs at most 19.

use crate::dtype::f16_to_f32;

pub const BLOCK_SIZE: usize = 32;
pub const BLOCK_BYTES: usize = 34;

/// Splits `Q8_0` blocks into their signed bytes, in order, and their binary16 scales
/// (little-endian, one per block): the layout GPU kernels load aligned. Moves bytes only.
///
/// # Panics
/// When `blocks` is not whole blocks.
pub fn split_blocks(blocks: &[u8]) -> (Vec<u8>, Vec<u8>) {
    assert!(
        blocks.len().is_multiple_of(BLOCK_BYTES),
        "whole Q8_0 blocks"
    );
    let n = blocks.len() / BLOCK_BYTES;
    let mut codes = Vec::with_capacity(n * BLOCK_SIZE);
    let mut scales = Vec::with_capacity(n * 2);
    for block in blocks.as_chunks::<BLOCK_BYTES>().0 {
        scales.extend_from_slice(&block[..2]);
        codes.extend_from_slice(&block[2..]);
    }
    (codes, scales)
}

/// A row-major `Q8_0` matrix borrowed from its blocks.
#[derive(Clone, Copy, Debug)]
pub struct Q8Matrix<'a> {
    bytes: &'a [u8],
    rows: usize,
    cols: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("a {rows}x{cols} Q8_0 matrix needs whole blocks of 32 per row and {expected} bytes, got {actual}")]
pub struct Q8ShapeError {
    rows: usize,
    cols: usize,
    expected: usize,
    actual: usize,
}

impl<'a> Q8Matrix<'a> {
    pub fn new(bytes: &'a [u8], rows: usize, cols: usize) -> Result<Self, Q8ShapeError> {
        let expected = rows * (cols / BLOCK_SIZE) * BLOCK_BYTES;
        if !cols.is_multiple_of(BLOCK_SIZE) || bytes.len() != expected {
            return Err(Q8ShapeError {
                rows,
                cols,
                expected,
                actual: bytes.len(),
            });
        }
        Ok(Self { bytes, rows, cols })
    }

    pub const fn rows(&self) -> usize {
        self.rows
    }

    pub const fn cols(&self) -> usize {
        self.cols
    }

    fn row(&self, r: usize) -> &'a [u8] {
        let width = self.cols / BLOCK_SIZE * BLOCK_BYTES;
        &self.bytes[r * width..(r + 1) * width]
    }

    /// Row `r` widened into `out` (`cols` long).
    ///
    /// # Panics
    /// When `r` is out of range or `out` is not `cols` long.
    pub fn dequantize_row(&self, r: usize, out: &mut [f32]) {
        assert!(r < self.rows, "row {r} of {}", self.rows);
        assert_eq!(out.len(), self.cols, "row width");
        for (block, out) in self
            .row(r)
            .as_chunks::<BLOCK_BYTES>()
            .0
            .iter()
            .zip(out.as_chunks_mut::<BLOCK_SIZE>().0)
        {
            let d = f16_to_f32(u16::from_le_bytes([block[0], block[1]]));
            for (o, &q) in out.iter_mut().zip(&block[2..]) {
                *o = d * f32::from(q as i8);
            }
        }
    }

    /// `y[r] = row r · x` for rows `first..first + y.len()`: each block's integer
    /// products are summed first and scaled once.
    ///
    /// # Panics
    /// When `x` is not `cols` long or the rows run past the matrix.
    pub fn mul_rows(&self, first: usize, y: &mut [f32], x: &[f32]) {
        assert_eq!(x.len(), self.cols, "input width");
        assert!(first + y.len() <= self.rows, "rows past the matrix");
        for (r, out) in y.iter_mut().enumerate() {
            let mut acc = 0.0_f32;
            for (block, x) in self
                .row(first + r)
                .as_chunks::<BLOCK_BYTES>()
                .0
                .iter()
                .zip(x.as_chunks::<BLOCK_SIZE>().0)
            {
                let d = f16_to_f32(u16::from_le_bytes([block[0], block[1]]));
                let mut sum = 0.0_f32;
                for (&q, &x) in block[2..].iter().zip(x) {
                    sum += f32::from(q as i8) * x;
                }
                acc += d * sum;
            }
            *out = acc;
        }
    }

    /// `y = W x`.
    pub fn mul_vec(&self, y: &mut [f32], x: &[f32]) {
        assert_eq!(y.len(), self.rows, "output height");
        self.mul_rows(0, y, x);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A block with scale `d` (as f16 bits) and codes `q`.
    fn block(d: u16, q: impl Fn(usize) -> i8) -> Vec<u8> {
        let mut out = d.to_le_bytes().to_vec();
        out.extend((0..32).map(|i| q(i) as u8));
        out
    }

    #[test]
    fn elements_are_the_scale_times_the_signed_byte_and_products_agree() {
        // 0x3800 is 0.5, 0xC000 is -2.0.
        let mut bytes = block(0x3800, |i| i as i8 - 16);
        bytes.extend(block(0xC000, |i| if i % 2 == 0 { 127 } else { -128 }));
        bytes.extend(block(0x0000, |_| 5));
        bytes.extend(block(0x3C00, |i| i as i8));
        let m = Q8Matrix::new(&bytes, 2, 64).unwrap();
        let mut row = vec![0.0; 64];
        m.dequantize_row(0, &mut row);
        assert_eq!(row[0], -8.0);
        assert_eq!(row[31], 7.5);
        assert_eq!(row[32], -254.0);
        assert_eq!(row[33], 256.0);
        m.dequantize_row(1, &mut row);
        assert_eq!(row[..32], [0.0; 32]);
        assert_eq!(row[63], 31.0);
        let x: Vec<f32> = (0..64).map(|i| (i as f32 * 0.37).sin()).collect();
        let mut y = vec![0.0; 2];
        m.mul_vec(&mut y, &x);
        for (r, &got) in y.iter().enumerate() {
            m.dequantize_row(r, &mut row);
            let want: f64 = row
                .iter()
                .zip(&x)
                .map(|(&a, &b)| f64::from(a) * f64::from(b))
                .sum();
            assert!(
                (f64::from(got) - want).abs() < 1e-4 * want.abs().max(1.0),
                "{r}"
            );
        }
        assert!(Q8Matrix::new(&bytes, 2, 63).is_err());
        assert!(Q8Matrix::new(&bytes[1..], 2, 64).is_err());
    }
}
