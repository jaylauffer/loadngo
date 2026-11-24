//! loadngo's addition to this copy of `fips204` (2026-11-24): erasing a polynomial with one write.
//!
//! Every polynomial in this crate erases itself when it goes out of use (`types::R`, `types::T`). Upstream derives
//! that from `zeroize`, whose arrays are erased one element at a time: for a polynomial, 256 volatile writes and
//! 256 compiler fences. A signature creates and erases some 430 polynomials, and that was a sixth of its time.
//!
//! [`wipe`] erases the same memory with one volatile write of the whole array and one fence. It is the technique
//! `zeroize` itself uses for whole values (`Option<Z>`, `MaybeUninit<Z>`): a volatile write may not be removed,
//! merged or reordered away by the compiler, whatever happens to the memory afterwards; being one write of 1,024
//! bytes, the compiler is free to carry it out with wide stores. What is erased, and when, is unchanged.
#![allow(unsafe_code)]

use core::sync::atomic::{compiler_fence, Ordering};

/// Sets all 256 coefficients to zero in a way the compiler may not optimise away.
#[inline]
pub(crate) fn wipe(block: &mut [i32; 256]) {
    // SAFETY: `block` is a valid, aligned, exclusive reference, and all zeros is a valid `[i32; 256]`.
    unsafe { core::ptr::write_volatile(block, [0i32; 256]) };
    compiler_fence(Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::wipe;
    use crate::types::{R, T};
    use core::mem::MaybeUninit;
    use zeroize::Zeroize;

    #[test]
    fn a_wiped_block_is_all_zeros() {
        let mut block = [0x5a5a_5a5a_i32; 256];
        wipe(&mut block);
        assert!(block.iter().all(|&x| x == 0));
        let mut r = R([-7; 256]);
        r.zeroize();
        assert!(r.0.iter().all(|&x| x == 0));
        let mut t = T([i32::MAX; 256]);
        t.zeroize();
        assert!(t.0.iter().all(|&x| x == 0));
    }

    /// A polynomial that goes out of use leaves zeros behind, also in an optimised build, where a plain
    /// assignment to memory nobody reads again would be dropped by the compiler.
    #[test]
    fn a_dropped_polynomial_leaves_zeros_in_its_memory() {
        let mut slot = MaybeUninit::<R>::uninit();
        let _ = slot.write(R([0x1234_5678; 256]));
        // SAFETY: the slot holds an initialised `R`, dropped exactly once here.
        unsafe { core::ptr::drop_in_place(slot.as_mut_ptr()) };
        // SAFETY: the slot's memory is still owned; every bit pattern is a valid `[i32; 256]`, and the drop wrote it.
        let left: [i32; 256] =
            unsafe { core::ptr::read_volatile(slot.as_ptr().cast::<[i32; 256]>()) };
        assert!(
            left.iter().all(|&x| x == 0),
            "an R was dropped without being erased"
        );

        let mut slot = MaybeUninit::<T>::uninit();
        let _ = slot.write(T([-0x0bad_cafe; 256]));
        // SAFETY: as above.
        unsafe { core::ptr::drop_in_place(slot.as_mut_ptr()) };
        // SAFETY: as above.
        let left: [i32; 256] =
            unsafe { core::ptr::read_volatile(slot.as_ptr().cast::<[i32; 256]>()) };
        assert!(
            left.iter().all(|&x| x == 0),
            "a T was dropped without being erased"
        );
    }
}
