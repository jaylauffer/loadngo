// This file implements functionality from FIPS 204 section 7.5 `NTT` and `invNTT`
//
// loadngo (2026-11-24): each transform's work on one polynomial is a function of its own, `ntt_poly` and
// `inv_ntt_poly`, so that it can run on the processor's vector instructions where there are any (`neon.rs`). The
// plain forms, `ntt_poly_scalar` and `inv_ntt_poly_scalar`, are upstream's loops unchanged: they run everywhere
// else, and they are the reference the vector forms are held to bit for bit.

use crate::helpers::{full_reduce32, mont_reduce, mul_acc, ZETA_TABLE_MONT};
use crate::types::{R, R0, T};
use crate::Q;

/// # Algorithm 41 NTT(w) on page 43.
/// Computes the Number-Theoretic Transform. An inner loop over `w/w_hat` has
/// been refactored into this function, so it processes an array of elements.
///
/// **Input**: polynomial `w(X) = ∑_{j=0}^{255} w_j X^j ∈ R_q` <br>
/// **Output**: `w_hat = (w_hat[0], ... , w_hat[255]) ∈ T_q`
pub(crate) fn ntt<const KL: usize>(w: &[R; KL]) -> [T; KL] {
    // 1: for j from 0 to 255 do
    // 2: w_hat[j] ← w_j
    // 3: end for
    let mut w_hat: [T; KL] = core::array::from_fn(|x| T(core::array::from_fn(|n| w[x].0[n])));

    // for each element of w_hat
    for w_poly in &mut w_hat {
        ntt_poly(&mut w_poly.0);
    }

    // 20: return ŵ
    w_hat
}

/// One polynomial's forward transform, in place: steps 4 to 19 of Algorithm 41.
#[inline]
pub(crate) fn ntt_poly(w: &mut [i32; 256]) {
    #[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
    crate::neon::ntt_poly(w);
    #[cfg(not(all(target_arch = "aarch64", target_feature = "neon")))]
    ntt_poly_scalar(w);
}

/// The plain form of [`ntt_poly`], and the reference its vector twin is held to.
#[cfg_attr(
    all(target_arch = "aarch64", target_feature = "neon", not(test)),
    allow(dead_code)
)]
pub(crate) fn ntt_poly_scalar(w: &mut [i32; 256]) {
    //
    // 4: m ← 0
    let mut m = 0;

    // 5: len ← 128
    let mut len = 128;

    // 6: while len ≥ 1 do
    while len >= 1 {
        //
        // 7: start ← 0
        let mut start = 0;

        // 8: while start < 256 do
        while start < 256 {
            //
            // 9: m ← m + 1
            m += 1;

            // 10: zeta ← ζ^{brv(k)} mod q
            let zeta = i64::from(ZETA_TABLE_MONT[m]);

            // 11: for j from start to start + len − 1 do
            for j in start..(start + len) {
                //
                // 12: t ← zeta · w_hat[j + len]
                let t = mont_reduce(zeta * i64::from(w[j + len]));

                // 13: w_hat[j + len] ← w_hat[j] − t
                w[j + len] = w[j] - t;

                // 14: w_hat[j] ← w_hat[j] + t
                w[j] += t;

                // 15: end for
            }

            // 16: start ← start + 2 · len
            start += 2 * len;

            // 17: end while
        }

        // 18: len ← ⌊len/2⌋
        len >>= 1;

        // 19: end while
    }
}

/// # Algorithm 42 NTT−1 (`w_hat`) on page 44.
/// Computes the inverse of the Number-Theoretic Transform. An inner loop over `w/w_hat` has
/// been refactored into this function, so it processes an array of elements.
///
/// **Input**: `w_hat` = `(w_hat[0], . . . , w_hat[255]) ∈ T_q` <br>
/// **Output**: polynomial `w(X) = ∑_{j=0}^{255} w_j X^j ∈ R_q`
pub(crate) fn inv_ntt<const KL: usize>(w_hat: &[T; KL]) -> [R; KL] {
    //
    // 1: for j from 0 to 255 do
    // 2: w_j ← w_hat[j]
    // 3: end for
    let mut w_out: [R; KL] = core::array::from_fn(|x| R(core::array::from_fn(|n| w_hat[x].0[n])));

    // for each element of w_hat
    for w_poly in &mut w_out {
        inv_ntt_poly(&mut w_poly.0);
    }

    // 25: return w
    w_out
}

/// One polynomial's inverse transform, in place, scaled by `256^-1` and reduced into `0..q`: steps 4 to 24 of
/// Algorithm 42.
#[inline]
pub(crate) fn inv_ntt_poly(w: &mut [i32; 256]) {
    #[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
    crate::neon::inv_ntt_poly(w);
    #[cfg(not(all(target_arch = "aarch64", target_feature = "neon")))]
    inv_ntt_poly_scalar(w);
}

/// The plain form of [`inv_ntt_poly`], and the reference its vector twin is held to.
#[cfg_attr(
    all(target_arch = "aarch64", target_feature = "neon", not(test)),
    allow(dead_code)
)]
pub(crate) fn inv_ntt_poly_scalar(w: &mut [i32; 256]) {
    //
    #[allow(clippy::cast_possible_truncation)]
    const F_MONT: i64 = 8_347_681_i128.wrapping_mul(1 << 32).rem_euclid(Q as i128) as i64;

    // 4: m ← 256
    let mut m = 256;

    // 5: len ← 1
    let mut len = 1;

    // 6: while len < 256 do
    while len < 256 {
        //
        // 7: start ← 0
        let mut start = 0;

        // 8: while start < 256 do
        while start < 256 {
            //
            // 9: m ← m − 1
            m -= 1;

            // 10: zeta ← −ζ^{brv(k)} mod q    ▷ 𝑧 ← −𝜁 BitRev8 (𝑚) mod 𝑞
            let zeta = -ZETA_TABLE_MONT[m];

            // 11: for j from start to start + len − 1 do
            for j in start..(start + len) {
                //
                // 12: t ← w_j
                let t = w[j];

                // 13: w_j ← t + w_{j+len}
                w[j] = t + w[j + len];

                // 14: w_{j+len} ← t − w_{j+len}
                w[j + len] = t - w[j + len];

                // 15: w_{j+len} ← zeta · w_{j+len}
                w[j + len] = mont_reduce(i64::from(zeta) * i64::from(w[j + len]));

                // 16: end for
            }

            // 17: start ← start + 2 · len
            start += 2 * len;

            // 18: end while
        }

        // 19: len ← 2 · len
        len <<= 1;

        // 20: end while
    }

    // 21: f ← 8347681          ▷ f = 256^{−1} mod q
    // 22: for j from 0 to 255 do
    // 23: wj ← f · wj
    for i in w.iter_mut() {
        *i = full_reduce32(mont_reduce(F_MONT * i64::from(*i)));
    }

    // 24: end for
}

/// loadngo: `inv_ntt(&mat_vec_mul(a_hat, u_hat))` for a `u_hat_mont` already in Montgomery form
/// (`helpers::to_mont_in_place`). Each row's sum is formed in the polynomial it is returned in and transformed back
/// there: the same products, added in the same order, with no vector built and erased in between.
pub(crate) fn mat_vec_mul_inv_ntt<const K: usize, const L: usize>(
    a_hat: &[[T; L]; K],
    u_hat_mont: &[T; L],
) -> [R; K] {
    let mut w = [R0; K];
    for (w_i, a_i) in w.iter_mut().zip(a_hat.iter()) {
        for (a_ij, u_j) in a_i.iter().zip(u_hat_mont.iter()) {
            mul_acc(&mut w_i.0, &a_ij.0, &u_j.0);
        }
        inv_ntt_poly(&mut w_i.0);
    }
    w
}

/// loadngo: the inverse transform of `c_hat` times each polynomial of `s_hat_mont`, coefficient by coefficient:
/// `inv_ntt` of upstream's `mont_reduce(c_hat[n] * s_hat_mont[k][n])`, each product formed in the polynomial it
/// is returned in (added to zero, which changes nothing) and transformed back there.
pub(crate) fn mul_inv_ntt<const KL: usize>(c_hat: &T, s_hat_mont: &[T; KL]) -> [R; KL] {
    let mut out = [R0; KL];
    for (o, s) in out.iter_mut().zip(s_hat_mont.iter()) {
        mul_acc(&mut o.0, &c_hat.0, &s.0);
        inv_ntt_poly(&mut o.0);
    }
    out
}
