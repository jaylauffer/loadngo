//! loadngo's addition to this copy of `fips204` (2026-11-24): the three inner loops of ML-DSA's polynomial
//! arithmetic on a 64-bit ARM processor's vector instructions (NEON), four coefficients at a time.
//!
//! * [`ntt_poly`]: the forward transform of one polynomial (FIPS 204 Algorithm 41);
//! * [`inv_ntt_poly`]: the inverse transform, with its final scaling (Algorithm 42);
//! * [`mul_acc`]: `acc[n] += a[n] * b[n] * 2^-32 mod q`, the step of the matrix-by-vector product.
//!
//! Each gives, coefficient for coefficient and bit for bit, what the plain functions it stands in for give
//! (`ntt::ntt_poly_scalar`, `ntt::inv_ntt_poly_scalar`, `helpers::mul_acc_scalar`): the same butterflies in the
//! same order with the same reductions, so nothing else in the crate can tell which ran. The tests at the foot
//! hold that on random and on extreme inputs. Nothing here branches on, or indexes memory by, a coefficient.
//!
//! The Montgomery product. The plain code computes `mont_reduce(a * b)`: with `t = low32(a * b) * QINV`,
//! `(a * b - t * Q) >> 32`. With vector instructions there is no 64-bit product of four lanes at once, so it is
//! formed from high halves: `hi = floor(2ab / 2^32)` (the doubling multiply, high half), `t = low32(a * b * QINV)`
//! (two low multiplies, or one when `b` is a constant whose `b * QINV` is tabled), `tq = floor(2tQ / 2^32)`, and
//! the result is `(hi - tq) / 2`. It is the same number: `ab - tQ` is a multiple of `2^32`, so `2ab` and `2tQ`
//! have the same low 32 bits, `hi - tq` is exactly `2(ab - tQ) / 2^32`, and halving is exact. The doubling
//! multiply saturates only for `-2^31 * -2^31`, which no coefficient here reaches.
//!
//! `unsafe` is allowed in this file alone. It is needed for two things: reading and writing four `i32` through a
//! pointer (the slices are bounds-checked first), and entering a function compiled for NEON from one that is
//! not (NEON is part of every 64-bit ARM target this module is compiled for: see the `cfg` on `mod neon`).
#![allow(unsafe_code)]
// The one-letter names are the standard's and upstream's: m, j, q, t, and a and b for two operands.
#![allow(clippy::many_single_char_names)]

use crate::helpers::gen_zeta_table_mont;
use crate::Q;
use core::arch::aarch64::{
    int32x4_t, vaddq_s32, vandq_s32, vcombine_s32, vdup_n_s32, vdupq_n_s32, vextq_s32, vhsubq_s32,
    vld1q_s32, vmlsq_s32, vmulq_s32, vnegq_s32, vqdmulhq_s32, vreinterpretq_s32_s64,
    vreinterpretq_s64_s32, vrev64q_s32, vshrq_n_s32, vst1q_s32, vsubq_s32, vtrn1q_s64, vtrn2q_s64,
    vuzp1q_s32, vuzp2q_s32, vzip1q_s32, vzip2q_s32,
};

/// `q^-1 mod 2^32`, as in `helpers::mont_reduce`.
const QINV: i32 = 58_728_449;

/// The transform's constants in Montgomery form (`helpers::ZETA_TABLE_MONT`), and each times `QINV`
/// (its low 32 bits), which the vector Montgomery product multiplies by.
static ZETA: [i32; 256] = gen_zeta_table_mont();
static ZETA_TWIST: [i32; 256] = twist(gen_zeta_table_mont());

const fn twist(mut table: [i32; 256]) -> [i32; 256] {
    let mut i = 0;
    while i < 256 {
        table[i] = table[i].wrapping_mul(QINV);
        i += 1;
    }
    table
}

/// `256^-1 mod q` in Montgomery form, the inverse transform's final factor (`ntt::inv_ntt`'s `F_MONT`).
#[allow(clippy::cast_possible_truncation)]
const F_MONT: i32 = 8_347_681_i128.wrapping_mul(1 << 32).rem_euclid(Q as i128) as i32;
const F_TWIST: i32 = F_MONT.wrapping_mul(QINV);

/// Four coefficients starting at `i`.
#[inline]
#[target_feature(enable = "neon")]
fn load(p: &[i32; 256], i: usize) -> int32x4_t {
    let lanes: &[i32] = &p[i..i + 4];
    // SAFETY: `lanes` is four initialised `i32`, checked by the slice above.
    unsafe { vld1q_s32(lanes.as_ptr()) }
}

/// Writes four coefficients starting at `i`.
#[inline]
#[target_feature(enable = "neon")]
fn store(p: &mut [i32; 256], i: usize, v: int32x4_t) {
    let lanes: &mut [i32] = &mut p[i..i + 4];
    // SAFETY: `lanes` is room for four `i32`, checked by the slice above.
    unsafe { vst1q_s32(lanes.as_mut_ptr(), v) }
}

/// Four table entries starting at `i`.
#[inline]
#[target_feature(enable = "neon")]
fn load4(table: &[i32; 256], i: usize) -> int32x4_t {
    load(table, i)
}

/// `mont_reduce(c * x)` in each lane, for a multiplier `c` whose `c * QINV` is `c_twist`.
#[inline]
#[target_feature(enable = "neon")]
fn mont_mul_twisted(x: int32x4_t, c: int32x4_t, c_twist: int32x4_t, q: int32x4_t) -> int32x4_t {
    let hi = vqdmulhq_s32(x, c);
    let t = vmulq_s32(x, c_twist);
    let tq = vqdmulhq_s32(t, q);
    vhsubq_s32(hi, tq)
}

/// The table's four entries from `i`, last first, each negated (wrapping, as the plain code's `-zeta` times
/// `QINV` wraps).
#[inline]
#[target_feature(enable = "neon")]
fn reversed_negated(table: &[i32; 256], i: usize) -> int32x4_t {
    let pairs_swapped = vrev64q_s32(load4(table, i)); // [i + 1, i, i + 3, i + 2]
    vnegq_s32(vextq_s32::<2>(pairs_swapped, pairs_swapped)) // [i + 3, i + 2, i + 1, i]
}

/// `[a0, a1, b0, b1]` and `[a2, a3, b2, b3]`: the low halves and the high halves of two vectors.
#[inline]
#[target_feature(enable = "neon")]
fn halves(a: int32x4_t, b: int32x4_t) -> (int32x4_t, int32x4_t) {
    let (a, b) = (vreinterpretq_s64_s32(a), vreinterpretq_s64_s32(b));
    (
        vreinterpretq_s32_s64(vtrn1q_s64(a, b)),
        vreinterpretq_s32_s64(vtrn2q_s64(a, b)),
    )
}

/// The forward transform of one polynomial, in place. Equal to `ntt::ntt_poly_scalar`.
pub(crate) fn ntt_poly(w: &mut [i32; 256]) {
    // SAFETY: this module is compiled only where NEON is enabled for the whole target.
    unsafe { ntt_poly_neon(w) }
}

#[target_feature(enable = "neon")]
fn ntt_poly_neon(w: &mut [i32; 256]) {
    let q = vdupq_n_s32(Q);
    let mut m = 0;

    // Layers with butterflies 128, 64, 32, 16, 8 and 4 apart: one constant for a whole run, four pairs a step.
    let mut len = 128;
    while len >= 4 {
        let mut start = 0;
        while start < 256 {
            m += 1;
            let (zeta, twist) = (vdupq_n_s32(ZETA[m]), vdupq_n_s32(ZETA_TWIST[m]));
            let mut j = start;
            while j < start + len {
                let (low, high) = (load(w, j), load(w, j + len));
                let t = mont_mul_twisted(high, zeta, twist, q);
                store(w, j + len, vsubq_s32(low, t));
                store(w, j, vaddq_s32(low, t));
                j += 4;
            }
            start += 2 * len;
        }
        len >>= 1;
    }

    // Two apart: each four coefficients have their own constant; two such fours a step.
    let mut start = 0;
    while start < 256 {
        let zeta = vcombine_s32(vdup_n_s32(ZETA[m + 1]), vdup_n_s32(ZETA[m + 2]));
        let twist = vcombine_s32(vdup_n_s32(ZETA_TWIST[m + 1]), vdup_n_s32(ZETA_TWIST[m + 2]));
        m += 2;
        let (low, high) = halves(load(w, start), load(w, start + 4));
        let t = mont_mul_twisted(high, zeta, twist, q);
        let (first, second) = halves(vaddq_s32(low, t), vsubq_s32(low, t));
        store(w, start, first);
        store(w, start + 4, second);
        start += 8;
    }

    // One apart: each pair has its own constant; four pairs a step.
    let mut start = 0;
    while start < 256 {
        let (zeta, twist) = (load4(&ZETA, m + 1), load4(&ZETA_TWIST, m + 1));
        m += 4;
        let (a, b) = (load(w, start), load(w, start + 4));
        let (even, odd) = (vuzp1q_s32(a, b), vuzp2q_s32(a, b));
        let t = mont_mul_twisted(odd, zeta, twist, q);
        let (sum, difference) = (vaddq_s32(even, t), vsubq_s32(even, t));
        store(w, start, vzip1q_s32(sum, difference));
        store(w, start + 4, vzip2q_s32(sum, difference));
        start += 8;
    }
}

/// The inverse transform of one polynomial, in place, with the final multiplication by `256^-1` and the
/// reduction into `0..q`. Equal to `ntt::inv_ntt_poly_scalar`.
pub(crate) fn inv_ntt_poly(w: &mut [i32; 256]) {
    // SAFETY: this module is compiled only where NEON is enabled for the whole target.
    unsafe { inv_ntt_poly_neon(w) }
}

#[target_feature(enable = "neon")]
fn inv_ntt_poly_neon(w: &mut [i32; 256]) {
    let q = vdupq_n_s32(Q);
    let mut m = 256;

    // One apart: the constants run backwards through the table, one for each pair.
    let mut start = 0;
    while start < 256 {
        m -= 4;
        // Pairs 0..4 take the constants at m + 3, m + 2, m + 1, m, negated.
        let (zeta, twist) = (reversed_negated(&ZETA, m), reversed_negated(&ZETA_TWIST, m));
        let (a, b) = (load(w, start), load(w, start + 4));
        let (even, odd) = (vuzp1q_s32(a, b), vuzp2q_s32(a, b));
        let sum = vaddq_s32(even, odd);
        let product = mont_mul_twisted(vsubq_s32(even, odd), zeta, twist, q);
        store(w, start, vzip1q_s32(sum, product));
        store(w, start + 4, vzip2q_s32(sum, product));
        start += 8;
    }

    // Two apart: one constant for each four coefficients; two such fours a step.
    let mut start = 0;
    while start < 256 {
        m -= 2;
        let zeta = vnegq_s32(vcombine_s32(vdup_n_s32(ZETA[m + 1]), vdup_n_s32(ZETA[m])));
        let twist = vnegq_s32(vcombine_s32(
            vdup_n_s32(ZETA_TWIST[m + 1]),
            vdup_n_s32(ZETA_TWIST[m]),
        ));
        let (low, high) = halves(load(w, start), load(w, start + 4));
        let sum = vaddq_s32(low, high);
        let product = mont_mul_twisted(vsubq_s32(low, high), zeta, twist, q);
        let (first, second) = halves(sum, product);
        store(w, start, first);
        store(w, start + 4, second);
        start += 8;
    }

    // Four and more apart: one constant for a whole run.
    let mut len = 4;
    while len < 256 {
        let mut start = 0;
        while start < 256 {
            m -= 1;
            let (zeta, twist) = (
                vdupq_n_s32(ZETA[m].wrapping_neg()),
                vdupq_n_s32(ZETA_TWIST[m].wrapping_neg()),
            );
            let mut j = start;
            while j < start + len {
                let (low, high) = (load(w, j), load(w, j + len));
                store(w, j, vaddq_s32(low, high));
                store(
                    w,
                    j + len,
                    mont_mul_twisted(vsubq_s32(low, high), zeta, twist, q),
                );
                j += 4;
            }
            start += 2 * len;
        }
        len <<= 1;
    }

    // Times 256^-1, then into 0..q: `full_reduce32(mont_reduce(F_MONT * x))`.
    let (f, f_twist, half) = (
        vdupq_n_s32(F_MONT),
        vdupq_n_s32(F_TWIST),
        vdupq_n_s32(1 << 22),
    );
    let mut j = 0;
    while j < 256 {
        let x = mont_mul_twisted(load(w, j), f, f_twist, q);
        // partial_reduce32: x - ((x + 2^22) >> 23) * q
        let partial = vmlsq_s32(x, vshrq_n_s32::<23>(vaddq_s32(x, half)), q);
        // full_reduce32: add q where negative
        store(
            w,
            j,
            vaddq_s32(partial, vandq_s32(vshrq_n_s32::<31>(partial), q)),
        );
        j += 4;
    }
}

/// `acc[n] += mont_reduce(a[n] * b[n])` for all 256 coefficients. Equal to `helpers::mul_acc_scalar`.
pub(crate) fn mul_acc(acc: &mut [i32; 256], a: &[i32; 256], b: &[i32; 256]) {
    // SAFETY: this module is compiled only where NEON is enabled for the whole target.
    unsafe { mul_acc_neon(acc, a, b) }
}

#[target_feature(enable = "neon")]
fn mul_acc_neon(acc: &mut [i32; 256], a: &[i32; 256], b: &[i32; 256]) {
    let (q, qinv) = (vdupq_n_s32(Q), vdupq_n_s32(QINV));
    let mut j = 0;
    while j < 256 {
        let (x, y) = (load(a, j), load(b, j));
        let hi = vqdmulhq_s32(x, y);
        let t = vmulq_s32(vmulq_s32(x, y), qinv);
        let product = vhsubq_s32(hi, vqdmulhq_s32(t, q));
        store(acc, j, vaddq_s32(load(acc, j), product));
        j += 4;
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::{inv_ntt_poly, mul_acc, ntt_poly};
    use crate::helpers::mul_acc_scalar;
    use crate::ntt::{inv_ntt_poly_scalar, ntt_poly_scalar};
    use crate::Q;
    use std::vec::Vec;

    /// A small generator of varied numbers (xorshift); nothing here needs to be unpredictable.
    struct Varied(u64);
    impl Varied {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        /// A coefficient in `-bound..=bound`.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_possible_wrap,
            clippy::cast_sign_loss
        )]
        fn within(&mut self, bound: i32) -> i32 {
            let span = 2 * i64::from(bound) + 1;
            ((self.next() % span as u64) as i64 - i64::from(bound)) as i32
        }

        fn poly(&mut self, bound: i32) -> [i32; 256] {
            core::array::from_fn(|_| self.within(bound))
        }
    }

    /// Polynomials that press on the ranges: every coefficient at one end, alternating ends, one spike.
    fn extremes(bound: i32) -> [[i32; 256]; 6] {
        [
            [bound; 256],
            [-bound; 256],
            core::array::from_fn(|n| if n % 2 == 0 { bound } else { -bound }),
            core::array::from_fn(|n| if n % 4 < 2 { -bound } else { bound }),
            core::array::from_fn(|n| if n == 255 { bound } else { 0 }),
            [0; 256],
        ]
    }

    #[test]
    fn the_forward_transform_is_the_plain_one_bit_for_bit() {
        let mut varied = Varied(0x9e37_79b9_7f4a_7c15);
        // Inputs as the signing and verifying code hands them over (within q), and far beyond: each of the
        // eight layers adds less than q, so 2^31 - 9q is the widest input the plain code itself can take.
        for bound in [Q - 1, 2 * Q, 1 << 26, i32::MAX - 9 * Q] {
            let mut inputs: Vec<[i32; 256]> = extremes(bound).to_vec();
            inputs.extend((0..400).map(|_| varied.poly(bound)));
            for input in inputs {
                let (mut plain, mut vector) = (input, input);
                ntt_poly_scalar(&mut plain);
                ntt_poly(&mut vector);
                assert_eq!(plain, vector, "bound {bound}");
            }
        }
    }

    #[test]
    fn the_inverse_transform_is_the_plain_one_bit_for_bit() {
        let mut varied = Varied(0x2545_f491_4f6c_dd1d);
        // The plain code's own limit: sums double at each of eight layers, so inputs stay within q.
        for bound in [Q - 1, Q / 2, 1 << 10] {
            let mut inputs: Vec<[i32; 256]> = extremes(bound).to_vec();
            inputs.extend((0..600).map(|_| varied.poly(bound)));
            for input in inputs {
                let (mut plain, mut vector) = (input, input);
                inv_ntt_poly_scalar(&mut plain);
                inv_ntt_poly(&mut vector);
                assert_eq!(plain, vector, "bound {bound}");
            }
        }
    }

    #[test]
    fn there_and_back_gives_the_polynomial_again() {
        let mut varied = Varied(0x1234_5678_9abc_def1);
        for _ in 0..200 {
            let input = varied.poly(Q - 1);
            let mut w = input;
            ntt_poly(&mut w);
            for x in &mut w {
                *x = crate::helpers::full_reduce32(*x); // as the callers reduce between the two
            }
            inv_ntt_poly(&mut w);
            let expected: [i32; 256] = core::array::from_fn(|n| input[n].rem_euclid(Q));
            assert_eq!(w, expected);
        }
    }

    #[test]
    fn the_multiply_and_add_is_the_plain_one_bit_for_bit() {
        let mut varied = Varied(0x0f0f_1e1e_2d2d_3c3c);
        // The plain product's own limit is |a * b| under about 2^31 * q (`mont_reduce`'s input): one operand
        // as wide as a transform's output can be with the other within q, and a narrower one with the other
        // under 2q, as `to_mont` leaves it. The running sum is anywhere it can be after several terms.
        for (wide, narrow) in [(Q - 1, Q - 1), (i32::MAX - 9 * Q, Q - 1), (1 << 28, 2 * Q)] {
            let mut cases: Vec<([i32; 256], [i32; 256], [i32; 256])> = Vec::new();
            for a in extremes(wide) {
                for b in extremes(narrow) {
                    cases.push((a, b, varied.poly(8 * Q)));
                }
            }
            cases.extend(
                (0..400).map(|_| (varied.poly(wide), varied.poly(narrow), varied.poly(8 * Q))),
            );
            for (a, b, start) in cases {
                let (mut plain, mut vector) = (start, start);
                mul_acc_scalar(&mut plain, &a, &b);
                mul_acc(&mut vector, &a, &b);
                assert_eq!(plain, vector, "bounds {wide}, {narrow}");
            }
        }
    }
}
