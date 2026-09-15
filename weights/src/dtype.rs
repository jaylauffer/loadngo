//! Element types a safetensors header can name, and exact conversions to `f32`.

use std::fmt;

/// An element type as named in a safetensors header.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Dtype {
    Bool,
    U8,
    I8,
    F8E4M3,
    F8E5M2,
    I16,
    U16,
    F16,
    Bf16,
    I32,
    U32,
    F32,
    F64,
    I64,
    U64,
}

impl Dtype {
    /// Every dtype, for exhaustive checks.
    pub const ALL: [Self; 15] = [
        Self::Bool,
        Self::U8,
        Self::I8,
        Self::F8E4M3,
        Self::F8E5M2,
        Self::I16,
        Self::U16,
        Self::F16,
        Self::Bf16,
        Self::I32,
        Self::U32,
        Self::F32,
        Self::F64,
        Self::I64,
        Self::U64,
    ];

    /// Parses a header dtype name. Unknown names are `None`: a tensor whose size cannot
    /// be computed cannot be validated, so callers must refuse the file.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|dtype| dtype.name() == name)
    }

    /// The name the header uses.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Bool => "BOOL",
            Self::U8 => "U8",
            Self::I8 => "I8",
            Self::F8E4M3 => "F8_E4M3",
            Self::F8E5M2 => "F8_E5M2",
            Self::I16 => "I16",
            Self::U16 => "U16",
            Self::F16 => "F16",
            Self::Bf16 => "BF16",
            Self::I32 => "I32",
            Self::U32 => "U32",
            Self::F32 => "F32",
            Self::F64 => "F64",
            Self::I64 => "I64",
            Self::U64 => "U64",
        }
    }

    /// Bytes per element.
    pub const fn size_bytes(self) -> usize {
        match self {
            Self::Bool | Self::U8 | Self::I8 | Self::F8E4M3 | Self::F8E5M2 => 1,
            Self::I16 | Self::U16 | Self::F16 | Self::Bf16 => 2,
            Self::I32 | Self::U32 | Self::F32 => 4,
            Self::F64 | Self::I64 | Self::U64 => 8,
        }
    }
}

impl fmt::Display for Dtype {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// bfloat16 to `f32`.
///
/// bfloat16 keeps binary32's sign bit and 8-bit exponent with the same bias and drops the
/// low 16 mantissa bits, so its 16 bits are exactly the high half of a binary32 and
/// widening is a shift: no rounding, and infinities and NaNs carry over.
#[inline]
pub fn bf16_to_f32(bits: u16) -> f32 {
    f32::from_bits(u32::from(bits) << 16)
}

/// IEEE 754 binary16 to `f32`, exact for every one of the 65,536 codes.
///
/// binary16 has a 5-bit exponent with bias 15 and a 10-bit mantissa. Normal values move
/// to binary32's bias of 127; subnormals are renormalised, because binary32 represents
/// them as normals; infinities and NaNs keep their payload in the top mantissa bits.
pub fn f16_to_f32(bits: u16) -> f32 {
    let sign = u32::from(bits >> 15) << 31;
    let exponent = u32::from((bits >> 10) & 0x1f);
    let mantissa = u32::from(bits & 0x3ff);
    let out = match (exponent, mantissa) {
        (0, 0) => sign,
        (0, _) => {
            // value = mantissa * 2^-24. Shift the leading one up to bit 10; each shift
            // lowers the exponent by one from binary16's subnormal exponent of -14.
            let mut m = mantissa;
            let mut unbiased: i32 = -14;
            while m & 0x400 == 0 {
                m <<= 1;
                unbiased -= 1;
            }
            let biased = u32::try_from(unbiased + 127).expect("renormalised exponent is positive");
            sign | (biased << 23) | ((m & 0x3ff) << 13)
        }
        (0x1f, _) => sign | 0x7f80_0000 | (mantissa << 13),
        _ => sign | ((exponent + 127 - 15) << 23) | (mantissa << 13),
    };
    f32::from_bits(out)
}

/// Why a byte buffer cannot be widened to `f32`.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum WidenError {
    #[error("{0} is not a floating-point type that widens to f32 exactly")]
    Unsupported(Dtype),
    #[error("{len} bytes is not a whole number of {dtype} elements")]
    Ragged { dtype: Dtype, len: usize },
}

/// Widens little-endian F32, F16 or BF16 bytes into `f32` values, exactly.
pub fn widen_to_f32(dtype: Dtype, bytes: &[u8]) -> Result<Vec<f32>, WidenError> {
    if !matches!(dtype, Dtype::F32 | Dtype::F16 | Dtype::Bf16) {
        return Err(WidenError::Unsupported(dtype));
    }
    let elements = bytes.chunks_exact(dtype.size_bytes());
    if !elements.remainder().is_empty() {
        return Err(WidenError::Ragged {
            dtype,
            len: bytes.len(),
        });
    }
    Ok(elements
        .map(|b| match *b {
            [b0, b1, b2, b3] => f32::from_le_bytes([b0, b1, b2, b3]),
            [lo, hi] if dtype == Dtype::F16 => f16_to_f32(u16::from_le_bytes([lo, hi])),
            [lo, hi] => bf16_to_f32(u16::from_le_bytes([lo, hi])),
            _ => unreachable!("chunks are exactly one element wide"),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_dtype_name_round_trips_and_unknown_names_are_refused() {
        for dtype in Dtype::ALL {
            assert_eq!(Dtype::from_name(dtype.name()), Some(dtype));
        }
        assert_eq!(Dtype::from_name("F4"), None);
        assert_eq!(Dtype::from_name("f32"), None, "names are case-sensitive");
    }

    #[test]
    fn bf16_widening_keeps_sign_exponent_and_mantissa() {
        assert_eq!(bf16_to_f32(0x3f80), 1.0);
        assert_eq!(bf16_to_f32(0xc000), -2.0);
        assert_eq!(bf16_to_f32(0x8000).to_bits(), (-0.0_f32).to_bits());
        assert_eq!(bf16_to_f32(0x7f80), f32::INFINITY);
        assert!(bf16_to_f32(0x7fc0).is_nan());
        // The smallest nonzero bfloat16 is a binary32 subnormal with only bit 16 set.
        assert_eq!(bf16_to_f32(0x0001).to_bits(), 0x0001_0000);
    }

    /// The value a binary16 code denotes, computed independently in f64.
    fn binary16_reference(bits: u16) -> f64 {
        let sign = if bits & 0x8000 != 0 { -1.0 } else { 1.0 };
        let exponent = i32::from((bits >> 10) & 0x1f);
        let mantissa = f64::from(bits & 0x3ff);
        match exponent {
            0 => sign * mantissa / 1024.0 * 2.0_f64.powi(-14),
            0x1f if mantissa == 0.0 => sign * f64::INFINITY,
            0x1f => f64::NAN,
            _ => sign * (1.0 + mantissa / 1024.0) * 2.0_f64.powi(exponent - 15),
        }
    }

    #[test]
    fn f16_widening_is_exact_for_all_65536_codes() {
        for bits in 0..=u16::MAX {
            let got = f16_to_f32(bits);
            let want = binary16_reference(bits);
            if want.is_nan() {
                assert!(got.is_nan(), "code {bits:#06x} must stay NaN");
            } else {
                assert_eq!(f64::from(got), want, "code {bits:#06x}");
                assert_eq!(
                    got.is_sign_negative(),
                    bits & 0x8000 != 0,
                    "sign of {bits:#06x}"
                );
            }
        }
    }

    #[test]
    fn widening_reads_little_endian_bytes_and_refuses_what_it_cannot_widen() {
        assert_eq!(
            widen_to_f32(Dtype::F16, &[0x00, 0x3c, 0x00, 0xc0]).unwrap(),
            vec![1.0, -2.0]
        );
        assert_eq!(widen_to_f32(Dtype::Bf16, &[0x80, 0x3f]).unwrap(), vec![1.0]);
        assert_eq!(
            widen_to_f32(Dtype::F32, &1.5_f32.to_le_bytes()).unwrap(),
            vec![1.5]
        );
        assert_eq!(
            widen_to_f32(Dtype::F16, &[0; 3]),
            Err(WidenError::Ragged {
                dtype: Dtype::F16,
                len: 3
            })
        );
        assert_eq!(
            widen_to_f32(Dtype::I32, &[0; 4]),
            Err(WidenError::Unsupported(Dtype::I32))
        );
    }
}
