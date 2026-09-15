//! OCP Microscaling (MX) v1.0 MXFP4.
//!
//! MXFP4 stores each element as a 4-bit E2M1 float (1 sign bit, 2 exponent bits with bias
//! 1, 1 mantissa bit) and gives every block of [`BLOCK_SIZE`] consecutive elements one
//! shared 8-bit E8M0 scale, an exponent with bias 127. An element's value is
//! `2^(scale - 127) * E2M1`. A scale byte of `0xFF` is NaN, and the specification makes
//! every element of that block NaN.
//!
//! The specification defines elements, not how they are packed into bytes. Published
//! MXFP4 checkpoints pack two elements per byte, **the low nibble holding the even
//! element**, row by row, with one scale byte per block per row. [`Mxfp4Matrix`] uses that
//! layout. Reversing the nibble order yields a matrix with every statistic intact and
//! every value in the wrong place, so it is pinned by a test rather than assumed.

/// Elements sharing one scale.
pub const BLOCK_SIZE: usize = 32;

/// The eight non-negative E2M1 magnitudes, indexed by the low three bits of a code.
const E2M1_MAGNITUDES: [f32; 8] = [0.0, 0.5, 1.0, 1.5, 2.0, 3.0, 4.0, 6.0];

/// The value of a 4-bit E2M1 code; bit 3 is the sign, so code 8 is `-0.0`.
#[inline]
pub fn e2m1(code: u8) -> f32 {
    let magnitude = E2M1_MAGNITUDES[usize::from(code & 0x7)];
    if code & 0x8 != 0 {
        -magnitude
    } else {
        magnitude
    }
}

/// The multiplier an E8M0 scale byte denotes: `2^(byte - 127)`, or NaN for `0xFF`.
///
/// Built from the bit pattern, so it is exact: every such power of two is representable
/// in `f32`, with `2^-127` as the subnormal whose only set bit is bit 22.
#[inline]
pub fn e8m0(byte: u8) -> f32 {
    match byte {
        0xff => f32::NAN,
        0 => f32::from_bits(1 << 22),
        _ => f32::from_bits(u32::from(byte) << 23),
    }
}

/// A row-major MXFP4 matrix borrowed from packed element and scale bytes.
#[derive(Clone, Copy, Debug)]
pub struct Mxfp4Matrix<'a> {
    elements: &'a [u8],
    scales: &'a [u8],
    rows: usize,
    cols: usize,
}

/// An [`Mxfp4Matrix`] whose byte buffers do not match its declared shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Mxfp4ShapeError {
    #[error("a {rows}x{cols} MXFP4 matrix needs {expected} element bytes, got {actual}")]
    Elements {
        rows: usize,
        cols: usize,
        expected: usize,
        actual: usize,
    },
    #[error("a {rows}x{cols} MXFP4 matrix needs {expected} scale bytes, got {actual}")]
    Scales {
        rows: usize,
        cols: usize,
        expected: usize,
        actual: usize,
    },
}

impl<'a> Mxfp4Matrix<'a> {
    /// Element bytes per row: two elements per byte, rounded up.
    pub const fn element_bytes_per_row(cols: usize) -> usize {
        cols.div_ceil(2)
    }

    /// Scale bytes per row: one per block, rounded up.
    pub const fn scales_per_row(cols: usize) -> usize {
        cols.div_ceil(BLOCK_SIZE)
    }

    pub fn new(
        elements: &'a [u8],
        scales: &'a [u8],
        rows: usize,
        cols: usize,
    ) -> Result<Self, Mxfp4ShapeError> {
        let expected = rows * Self::element_bytes_per_row(cols);
        if elements.len() != expected {
            return Err(Mxfp4ShapeError::Elements {
                rows,
                cols,
                expected,
                actual: elements.len(),
            });
        }
        let expected = rows * Self::scales_per_row(cols);
        if scales.len() != expected {
            return Err(Mxfp4ShapeError::Scales {
                rows,
                cols,
                expected,
                actual: scales.len(),
            });
        }
        Ok(Self {
            elements,
            scales,
            rows,
            cols,
        })
    }

    pub const fn rows(&self) -> usize {
        self.rows
    }

    pub const fn cols(&self) -> usize {
        self.cols
    }

    #[inline]
    fn code(row_elements: &[u8], c: usize) -> u8 {
        let byte = row_elements[c / 2];
        if c & 1 == 0 {
            byte & 0x0f
        } else {
            byte >> 4
        }
    }

    fn row_parts(&self, r: usize) -> (&[u8], &[u8]) {
        let eb = Self::element_bytes_per_row(self.cols);
        let sb = Self::scales_per_row(self.cols);
        (
            &self.elements[r * eb..(r + 1) * eb],
            &self.scales[r * sb..(r + 1) * sb],
        )
    }

    /// Row `r` dequantised into `out`, which must be `cols` long.
    ///
    /// # Panics
    ///
    /// Panics when `r` is out of range or `out` is not `cols` long.
    pub fn dequantize_row(&self, r: usize, out: &mut [f32]) {
        assert!(r < self.rows, "row {r} of {}", self.rows);
        assert_eq!(out.len(), self.cols, "row width");
        let (elements, scales) = self.row_parts(r);
        for (c, o) in out.iter_mut().enumerate() {
            *o = e8m0(scales[c / BLOCK_SIZE]) * e2m1(Self::code(elements, c));
        }
    }

    /// `y = W x` without widening `W`.
    ///
    /// Within a block every element shares one scale, so each block's E2M1 values are
    /// dotted with `x` first and the scale applied once. Accumulation is in `f64`, where
    /// every E2M1-by-`f32` product and every power-of-two scaling is exact, so the result
    /// agrees with dequantise-then-multiply to within the rounding of the additions.
    ///
    /// # Panics
    ///
    /// Panics when `x` is not `cols` long or `y` is not `rows` long.
    pub fn mul_vec(&self, y: &mut [f32], x: &[f32]) {
        assert_eq!(x.len(), self.cols, "input width");
        assert_eq!(y.len(), self.rows, "output height");
        for (r, out) in y.iter_mut().enumerate() {
            let (elements, scales) = self.row_parts(r);
            let mut acc = 0.0_f64;
            for (b, &scale) in scales.iter().enumerate() {
                let start = b * BLOCK_SIZE;
                let end = (start + BLOCK_SIZE).min(self.cols);
                let mut block = 0.0_f64;
                for (offset, &xc) in x[start..end].iter().enumerate() {
                    let code = Self::code(elements, start + offset);
                    block += f64::from(e2m1(code)) * f64::from(xc);
                }
                acc += block * f64::from(e8m0(scale));
            }
            *out = acc as f32;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn e2m1_codes_follow_the_specification_table() {
        let positive = [0.0, 0.5, 1.0, 1.5, 2.0, 3.0, 4.0, 6.0];
        for code in 0..8_u8 {
            assert_eq!(e2m1(code), positive[usize::from(code)]);
            assert_eq!(e2m1(code | 8), -positive[usize::from(code)]);
            assert!(e2m1(code | 8).is_sign_negative());
        }
    }

    #[test]
    fn e8m0_scales_are_exact_powers_of_two_and_0xff_is_nan() {
        for byte in 0..=254_u8 {
            assert_eq!(
                f64::from(e8m0(byte)),
                2.0_f64.powi(i32::from(byte) - 127),
                "{byte}"
            );
        }
        assert!(e8m0(0xff).is_nan());
    }

    /// Packs codes two per byte, low nibble first.
    fn pack(codes: &[u8]) -> Vec<u8> {
        codes
            .chunks(2)
            .map(|pair| pair[0] | pair.get(1).map_or(0, |hi| hi << 4))
            .collect()
    }

    #[test]
    fn the_low_nibble_is_the_even_element() {
        // Row [1.0, -3.0] under scale 2^0 packs as one byte: low 0x2, high 0xd.
        let m = Mxfp4Matrix::new(&[0xd2], &[127], 1, 2).unwrap();
        let mut row = [0.0; 2];
        m.dequantize_row(0, &mut row);
        assert_eq!(row, [1.0, -3.0]);
    }

    #[test]
    fn products_agree_with_dequantise_then_multiply_across_blocks_and_odd_widths() {
        // 3 rows x 69 columns: two full blocks, a 5-element tail, and an odd final byte.
        let (rows, cols) = (3, 69);
        let codes: Vec<u8> = (0..rows * cols).map(|i| ((i * 7 + 3) % 16) as u8).collect();
        let elements: Vec<u8> = codes.chunks(cols).flat_map(pack).collect();
        let scales: Vec<u8> = (0..rows * Mxfp4Matrix::scales_per_row(cols))
            .map(|i| 120 + (i % 9) as u8)
            .collect();
        let m = Mxfp4Matrix::new(&elements, &scales, rows, cols).unwrap();
        let x: Vec<f32> = (0..cols).map(|i| ((i as f32) * 0.618).sin()).collect();

        let mut y = vec![0.0; rows];
        m.mul_vec(&mut y, &x);
        let mut row = vec![0.0; cols];
        for (r, &got) in y.iter().enumerate() {
            m.dequantize_row(r, &mut row);
            for (c, &value) in row.iter().enumerate() {
                assert_eq!(
                    value,
                    e8m0(scales[r * 3 + c / BLOCK_SIZE]) * e2m1(codes[r * cols + c])
                );
            }
            let want: f64 = row
                .iter()
                .zip(&x)
                .map(|(&w, &v)| f64::from(w) * f64::from(v))
                .sum();
            let rel = (f64::from(got) - want).abs() / want.abs().max(f64::MIN_POSITIVE);
            assert!(rel < 1e-6, "row {r}: {got} vs {want}");
        }
    }

    #[test]
    fn a_nan_scale_makes_its_block_nan_as_the_specification_requires() {
        let elements = pack(&[1; 32]);
        let m = Mxfp4Matrix::new(&elements, &[0xff], 1, 32).unwrap();
        let mut row = [0.0; 32];
        m.dequantize_row(0, &mut row);
        assert!(row.iter().all(|v| v.is_nan()));
        let mut y = [0.0];
        m.mul_vec(&mut y, &[1.0; 32]);
        assert!(y[0].is_nan());
    }

    #[test]
    fn buffers_must_match_the_declared_shape() {
        // 33 columns: 17 element bytes (the last holds one element) and 2 scale bytes.
        assert!(Mxfp4Matrix::new(&[0; 17], &[0; 2], 1, 33).is_ok());
        assert!(matches!(
            Mxfp4Matrix::new(&[0; 16], &[0; 2], 1, 33),
            Err(Mxfp4ShapeError::Elements {
                expected: 17,
                actual: 16,
                ..
            })
        ));
        assert!(matches!(
            Mxfp4Matrix::new(&[0; 17], &[0; 1], 1, 33),
            Err(Mxfp4ShapeError::Scales {
                expected: 2,
                actual: 1,
                ..
            })
        ));
    }
}
