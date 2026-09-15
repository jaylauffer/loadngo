//! Matrix-vector products read straight out of half-precision bytes.
//!
//! A checkpoint's large matrices are usually stored as bfloat16 or binary16. Multiplying
//! from those bytes halves the memory and bandwidth of holding them as `f32`, and loses
//! nothing: each element widens exactly (see [`crate::dtype`]).

use crate::dtype::{bf16_to_f32, f16_to_f32};

/// The encoding of a [`HalfMatrix`]'s elements.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HalfFormat {
    Bf16,
    F16,
}

/// A row-major matrix of little-endian 16-bit float elements, borrowed as raw bytes so it
/// can point straight into a file buffer with no alignment requirement.
#[derive(Clone, Copy, Debug)]
pub struct HalfMatrix<'a> {
    bytes: &'a [u8],
    rows: usize,
    cols: usize,
    format: HalfFormat,
}

/// A [`HalfMatrix`] whose bytes do not match its declared shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("a {rows}x{cols} half-precision matrix needs {expected} bytes, got {actual}")]
pub struct ShapeError {
    pub rows: usize,
    pub cols: usize,
    pub expected: usize,
    pub actual: usize,
}

impl<'a> HalfMatrix<'a> {
    pub fn new(
        bytes: &'a [u8],
        rows: usize,
        cols: usize,
        format: HalfFormat,
    ) -> Result<Self, ShapeError> {
        let expected = rows * cols * 2;
        if bytes.len() != expected {
            return Err(ShapeError {
                rows,
                cols,
                expected,
                actual: bytes.len(),
            });
        }
        Ok(Self {
            bytes,
            rows,
            cols,
            format,
        })
    }

    pub const fn rows(&self) -> usize {
        self.rows
    }

    pub const fn cols(&self) -> usize {
        self.cols
    }

    #[inline]
    fn element(&self, at: usize) -> f32 {
        let bits = u16::from_le_bytes([self.bytes[2 * at], self.bytes[2 * at + 1]]);
        match self.format {
            HalfFormat::Bf16 => bf16_to_f32(bits),
            HalfFormat::F16 => f16_to_f32(bits),
        }
    }

    /// `y = W x`, each row summed in `f64` in column order.
    ///
    /// # Panics
    ///
    /// Panics when `x` is not `cols` long or `y` is not `rows` long.
    pub fn mul_vec(&self, y: &mut [f32], x: &[f32]) {
        assert_eq!(x.len(), self.cols, "input width");
        assert_eq!(y.len(), self.rows, "output height");
        for (r, out) in y.iter_mut().enumerate() {
            let base = r * self.cols;
            let mut acc = 0.0_f64;
            for (c, &xc) in x.iter().enumerate() {
                acc += f64::from(self.element(base + c)) * f64::from(xc);
            }
            *out = acc as f32;
        }
    }

    /// Row `r` widened into `out`, which must be `cols` long.
    ///
    /// # Panics
    ///
    /// Panics when `r` is out of range or `out` is not `cols` long.
    pub fn row_into(&self, r: usize, out: &mut [f32]) {
        assert!(r < self.rows, "row {r} of {}", self.rows);
        assert_eq!(out.len(), self.cols, "row width");
        for (c, o) in out.iter_mut().enumerate() {
            *o = self.element(r * self.cols + c);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(codes: &[u16]) -> Vec<u8> {
        codes.iter().flat_map(|c| c.to_le_bytes()).collect()
    }

    #[test]
    fn products_match_the_widened_matrix_for_both_formats() {
        // 2x3, bf16: [[1, -2, 0.5], [0, 3, -1]]
        let bf16 = bytes(&[0x3f80, 0xc000, 0x3f00, 0x0000, 0x4040, 0xbf80]);
        // the same values as binary16
        let f16 = bytes(&[0x3c00, 0xc000, 0x3800, 0x0000, 0x4200, 0xbc00]);
        let x = [0.25_f32, -1.0, 4.0];
        let want = [0.25 + 2.0 + 2.0, -3.0 - 4.0];
        for (data, format) in [(&bf16, HalfFormat::Bf16), (&f16, HalfFormat::F16)] {
            let w = HalfMatrix::new(data, 2, 3, format).unwrap();
            let mut y = [0.0; 2];
            w.mul_vec(&mut y, &x);
            assert_eq!(y, want, "{format:?}");
            let mut row = [0.0; 3];
            w.row_into(1, &mut row);
            assert_eq!(row, [0.0, 3.0, -1.0]);
        }
    }

    #[test]
    fn a_matrix_must_match_its_declared_shape() {
        assert_eq!(
            HalfMatrix::new(&[0; 10], 2, 3, HalfFormat::Bf16).unwrap_err(),
            ShapeError {
                rows: 2,
                cols: 3,
                expected: 12,
                actual: 10
            }
        );
    }
}
