//! Whole numbers of up to 512 bits, and the arithmetic a primality test
//! needs on them: enough for a password of 64 characters read as a numeral
//! in base 94 (420 bits).
//!
//! Nothing here runs in constant time. The numbers are secrets, but they are
//! tested on the machine that made them, by the program that will show them;
//! see "What it does not defend against" in `docs/VAULT.md`.

use std::cmp::Ordering;

use zeroize::Zeroize;

/// 64-bit limbs in a [`Number`].
pub const LIMBS: usize = 8;
/// Bits in a [`Number`].
pub const BITS: usize = LIMBS * 64;

/// An unsigned whole number below 2^512, least significant limb first. It is
/// wiped when it is dropped.
#[derive(Clone, PartialEq, Eq)]
pub struct Number {
    limbs: [u64; LIMBS],
}

impl Drop for Number {
    fn drop(&mut self) {
        self.limbs.zeroize();
    }
}

impl std::fmt::Debug for Number {
    /// Never the value: a number here is, or is on its way to being, a
    /// password.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Number({} bits)", self.bits())
    }
}

impl PartialOrd for Number {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Number {
    fn cmp(&self, other: &Self) -> Ordering {
        for i in (0..LIMBS).rev() {
            match self.limbs[i].cmp(&other.limbs[i]) {
                Ordering::Equal => {}
                unequal => return unequal,
            }
        }
        Ordering::Equal
    }
}

impl Number {
    pub const ZERO: Number = Number { limbs: [0; LIMBS] };

    #[must_use]
    pub const fn from_u64(value: u64) -> Self {
        let mut limbs = [0; LIMBS];
        limbs[0] = value;
        Number { limbs }
    }

    /// The number these bytes spell, least significant first; bytes past the
    /// 64th are not read.
    #[must_use]
    pub fn from_le_bytes(bytes: &[u8]) -> Self {
        let mut limbs = [0u64; LIMBS];
        for (index, byte) in bytes.iter().take(LIMBS * 8).enumerate() {
            limbs[index / 8] |= u64::from(*byte) << (8 * (index % 8));
        }
        Number { limbs }
    }

    /// Keeps the lowest `bits` bits and clears the rest.
    pub fn keep_low(&mut self, bits: usize) {
        for (index, limb) in self.limbs.iter_mut().enumerate() {
            let from = index * 64;
            if bits <= from {
                *limb = 0;
            } else if bits < from + 64 {
                *limb &= (1u64 << (bits - from)) - 1;
            }
        }
    }

    /// The value, when it fits in 64 bits.
    #[must_use]
    pub fn as_u64(&self) -> Option<u64> {
        self.limbs[1..]
            .iter()
            .all(|&limb| limb == 0)
            .then_some(self.limbs[0])
    }

    #[must_use]
    pub fn is_zero(&self) -> bool {
        self.limbs.iter().all(|&limb| limb == 0)
    }

    #[must_use]
    pub fn is_even(&self) -> bool {
        self.limbs[0] & 1 == 0
    }

    /// How many bits the number takes: 0 for zero.
    #[must_use]
    pub fn bits(&self) -> usize {
        for i in (0..LIMBS).rev() {
            if self.limbs[i] != 0 {
                return i * 64 + 64 - self.limbs[i].leading_zeros() as usize;
            }
        }
        0
    }

    #[must_use]
    pub fn bit(&self, index: usize) -> bool {
        index < BITS && (self.limbs[index / 64] >> (index % 64)) & 1 == 1
    }

    /// `self * factor + addend`, or `None` if that does not fit.
    #[must_use]
    pub fn times_plus(&self, factor: u64, addend: u64) -> Option<Number> {
        let mut out = Number::ZERO;
        let mut carry = u128::from(addend);
        for i in 0..LIMBS {
            let wide = u128::from(self.limbs[i]) * u128::from(factor) + carry;
            out.limbs[i] = wide as u64;
            carry = wide >> 64;
        }
        (carry == 0).then_some(out)
    }

    /// Divides in place by `divisor` (not zero) and returns the remainder.
    pub fn divide(&mut self, divisor: u64) -> u64 {
        assert!(divisor != 0, "division by zero");
        let mut rest = 0u128;
        for i in (0..LIMBS).rev() {
            let wide = (rest << 64) | u128::from(self.limbs[i]);
            self.limbs[i] = (wide / u128::from(divisor)) as u64;
            rest = wide % u128::from(divisor);
        }
        rest as u64
    }

    /// The remainder on division by `divisor` (not zero).
    #[must_use]
    pub fn remainder(&self, divisor: u64) -> u64 {
        assert!(divisor != 0, "division by zero");
        let mut rest = 0u128;
        for i in (0..LIMBS).rev() {
            rest = ((rest << 64) | u128::from(self.limbs[i])) % u128::from(divisor);
        }
        rest as u64
    }

    /// `self - other`, wrapping below zero, and whether it wrapped.
    fn minus(&self, other: &Number) -> (Number, bool) {
        let mut out = Number::ZERO;
        let mut borrow = false;
        for i in 0..LIMBS {
            let (first, under_a) = self.limbs[i].overflowing_sub(other.limbs[i]);
            let (second, under_b) = first.overflowing_sub(u64::from(borrow));
            out.limbs[i] = second;
            borrow = under_a || under_b;
        }
        (out, borrow)
    }

    /// `self - other`, for `other <= self`.
    #[must_use]
    pub fn less(&self, other: &Number) -> Number {
        let (out, wrapped) = self.minus(other);
        debug_assert!(!wrapped, "subtraction below zero");
        out
    }

    /// Halves in place, dropping the lowest bit.
    pub fn halve(&mut self) {
        let mut carry = 0;
        for i in (0..LIMBS).rev() {
            let next = self.limbs[i] << 63;
            self.limbs[i] = (self.limbs[i] >> 1) | carry;
            carry = next;
        }
    }

    /// Doubles `self` modulo `modulus`, for `self < modulus`.
    fn doubled_within(&self, modulus: &Number) -> Number {
        let mut out = Number::ZERO;
        let mut carry = 0;
        for i in 0..LIMBS {
            out.limbs[i] = (self.limbs[i] << 1) | carry;
            carry = self.limbs[i] >> 63;
        }
        if carry == 1 || out >= *modulus {
            // With the carry, the true value is 2^512 + out, and taking the
            // modulus away once wraps to the right answer.
            out = out.minus(modulus).0;
        }
        out
    }

    /// The number as decimal digits.
    #[must_use]
    pub fn decimal(&self) -> String {
        if self.is_zero() {
            return "0".to_string();
        }
        let mut rest = self.clone();
        let mut digits = Vec::with_capacity(160);
        while !rest.is_zero() {
            digits.push(b'0' + rest.divide(10) as u8);
        }
        digits.reverse();
        let text = String::from_utf8_lossy(&digits).into_owned();
        digits.zeroize();
        text
    }
}

/// Multiplication and powers modulo one odd number, by Montgomery's method:
/// numbers are kept multiplied by R = 2^512, where reducing a product needs
/// no division.
pub struct Modulus {
    modulus: Number,
    /// `-modulus^-1 mod 2^64`.
    low_inverse: u64,
    /// `R mod modulus`: one, in Montgomery form.
    one: Number,
    /// `R^2 mod modulus`, which carries a number into Montgomery form.
    square: Number,
}

impl Modulus {
    /// For an odd `modulus` above one.
    #[must_use]
    pub fn new(modulus: &Number) -> Option<Self> {
        if modulus.is_even() || *modulus <= Number::from_u64(1) {
            return None;
        }
        // Newton's iteration for the inverse of an odd number modulo 2^64:
        // each step doubles the correct low bits, starting from three.
        let low = modulus.limbs[0];
        let mut inverse = low;
        for _ in 0..5 {
            inverse = inverse.wrapping_mul(2u64.wrapping_sub(low.wrapping_mul(inverse)));
        }
        // R and R^2 modulo the modulus, by doubling one 512 and 1024 times.
        let mut power = Number::from_u64(1);
        if power >= *modulus {
            return None;
        }
        for _ in 0..BITS {
            power = power.doubled_within(modulus);
        }
        let one = power.clone();
        for _ in 0..BITS {
            power = power.doubled_within(modulus);
        }
        Some(Modulus {
            modulus: modulus.clone(),
            low_inverse: inverse.wrapping_neg(),
            one,
            square: power,
        })
    }

    /// One, in Montgomery form.
    #[must_use]
    pub fn one(&self) -> &Number {
        &self.one
    }

    /// `modulus - 1`, in Montgomery form.
    #[must_use]
    pub fn minus_one(&self) -> Number {
        self.modulus.less(&self.one)
    }

    /// `value` (below the modulus) into Montgomery form.
    #[must_use]
    pub fn enter(&self, value: &Number) -> Number {
        self.times(value, &self.square)
    }

    /// Out of Montgomery form.
    #[must_use]
    pub fn leave(&self, value: &Number) -> Number {
        self.times(value, &Number::from_u64(1))
    }

    /// The product of two numbers in Montgomery form, in Montgomery form
    /// (coarsely integrated operand scanning).
    #[must_use]
    // The inner loops walk three arrays by one index, as the method is
    // written down.
    #[allow(clippy::needless_range_loop)]
    pub fn times(&self, a: &Number, b: &Number) -> Number {
        let modulus = &self.modulus.limbs;
        let mut work = [0u64; LIMBS + 2];
        for i in 0..LIMBS {
            let mut carry = 0u128;
            for j in 0..LIMBS {
                let wide =
                    u128::from(work[j]) + u128::from(a.limbs[j]) * u128::from(b.limbs[i]) + carry;
                work[j] = wide as u64;
                carry = wide >> 64;
            }
            let wide = u128::from(work[LIMBS]) + carry;
            work[LIMBS] = wide as u64;
            work[LIMBS + 1] = (wide >> 64) as u64;

            // The multiple of the modulus that clears the lowest limb; the
            // sum is then shifted down one limb.
            let factor = work[0].wrapping_mul(self.low_inverse);
            let wide = u128::from(work[0]) + u128::from(factor) * u128::from(modulus[0]);
            let mut carry = wide >> 64;
            for j in 1..LIMBS {
                let wide =
                    u128::from(work[j]) + u128::from(factor) * u128::from(modulus[j]) + carry;
                work[j - 1] = wide as u64;
                carry = wide >> 64;
            }
            let wide = u128::from(work[LIMBS]) + carry;
            work[LIMBS - 1] = wide as u64;
            work[LIMBS] = work[LIMBS + 1] + (wide >> 64) as u64;
        }
        let mut out = Number::ZERO;
        out.limbs.copy_from_slice(&work[..LIMBS]);
        if work[LIMBS] != 0 || out >= self.modulus {
            out = out.minus(&self.modulus).0;
        }
        work.zeroize();
        out
    }

    /// `base` (in Montgomery form) to the power `exponent`, in Montgomery
    /// form.
    #[must_use]
    pub fn power(&self, base: &Number, exponent: &Number) -> Number {
        let mut out = self.one.clone();
        for index in (0..exponent.bits()).rev() {
            out = self.times(&out, &out);
            if exponent.bit(index) {
                out = self.times(&out, base);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn number(decimal: &str) -> Number {
        decimal.bytes().fold(Number::ZERO, |sum, digit| {
            sum.times_plus(10, u64::from(digit - b'0')).unwrap()
        })
    }

    #[test]
    fn a_number_reads_back_as_the_decimal_it_was_made_from() {
        for text in [
            "0",
            "7",
            "18446744073709551615",
            "18446744073709551616",
            "340282366920938463463374607431768211507",
            "115792089237316195423570985008687907853269984665640564039457584007913129639936",
        ] {
            assert_eq!(number(text).decimal(), text);
        }
    }

    #[test]
    fn a_number_that_does_not_fit_is_refused() {
        // 2^512 - 1 fits; times two does not.
        let most = Number {
            limbs: [u64::MAX; LIMBS],
        };
        assert_eq!(most.bits(), 512);
        assert!(most.times_plus(1, 0).is_some());
        assert!(most.times_plus(2, 0).is_none());
        assert!(most.times_plus(1, 1).is_none());
    }

    #[test]
    fn dividing_gives_the_quotient_and_the_remainder() {
        let mut value = number("123456789012345678901234567890123456789");
        assert_eq!(value.remainder(97), 61);
        assert_eq!(value.divide(1_000_000_007), 741_412_909);
        assert_eq!(value.decimal(), "123456788148148161864197434840");
    }

    #[test]
    fn bytes_and_low_bits() {
        let mut bytes = [0u8; 64];
        bytes[0] = 0x39;
        bytes[1] = 0x30;
        bytes[8] = 1;
        let mut value = Number::from_le_bytes(&bytes);
        assert_eq!(value.decimal(), "18446744073709563961"); // 2^64 + 12345
        value.keep_low(64);
        assert_eq!(value.as_u64(), Some(12345));
        value.keep_low(4);
        assert_eq!(value.as_u64(), Some(9));
        value.keep_low(0);
        assert!(value.is_zero());
    }

    #[test]
    fn halving_and_bits() {
        let mut value = number("36893488147419103232"); // 2^65
        assert_eq!(value.bits(), 66);
        assert!(value.bit(65) && !value.bit(64));
        value.halve();
        assert_eq!(value.decimal(), "18446744073709551616");
        assert_eq!(Number::ZERO.bits(), 0);
    }

    /// Powers by plain 128-bit arithmetic, to hold the Montgomery form to.
    fn plain_power(base: u64, mut exponent: u64, modulus: u64) -> u64 {
        let wide = u128::from(modulus);
        let mut out = 1u128;
        let mut square = u128::from(base) % wide;
        while exponent > 0 {
            if exponent & 1 == 1 {
                out = out * square % wide;
            }
            square = square * square % wide;
            exponent >>= 1;
        }
        out as u64
    }

    #[test]
    fn powers_agree_with_plain_arithmetic_on_small_moduli() {
        let moduli = [
            3u64,
            5,
            97,
            65_537,
            4_294_967_297,
            18_446_744_073_709_551_557,
        ];
        let bases = [2u64, 3, 10, 65_536, 1_234_567_891_011];
        let exponents = [0u64, 1, 2, 64, 65_535, 18_446_744_073_709_551_556];
        for modulus in moduli {
            let ring = Modulus::new(&Number::from_u64(modulus)).unwrap();
            for base in bases {
                for exponent in exponents {
                    let reduced = Number::from_u64(base % modulus);
                    let power = ring.power(&ring.enter(&reduced), &Number::from_u64(exponent));
                    assert_eq!(
                        ring.leave(&power).as_u64(),
                        Some(plain_power(base, exponent, modulus)),
                        "{base}^{exponent} mod {modulus}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_power_modulo_a_large_number_is_the_known_answer() {
        // Fermat: a^(p-1) = 1 modulo the prime p = 2^255 - 19, and the
        // square root of one that is not one is p - 1.
        let p =
            number("57896044618658097711785492504343953926634992332820282019728792003956564819949");
        let ring = Modulus::new(&p).unwrap();
        let mut exponent = p.less(&Number::from_u64(1));
        let seven = ring.enter(&Number::from_u64(7));
        assert_eq!(ring.power(&seven, &exponent), *ring.one());
        // 7 is not a square modulo this prime, so 7^((p-1)/2) = -1.
        exponent.halve();
        assert_eq!(ring.power(&seven, &exponent), ring.minus_one());
        // 3^200 modulo 2^255 - 19, as Python's pow(3, 200, 2**255 - 19) has it.
        let power = ring.power(&ring.enter(&Number::from_u64(3)), &Number::from_u64(200));
        assert_eq!(
            ring.leave(&power).decimal(),
            "29899603888533214015297764514001059750171527264958905210651069474919969664040"
        );
    }

    #[test]
    fn an_even_modulus_and_one_are_refused() {
        assert!(Modulus::new(&Number::from_u64(10)).is_none());
        assert!(Modulus::new(&Number::from_u64(1)).is_none());
        assert!(Modulus::new(&Number::ZERO).is_none());
    }

    #[test]
    fn a_full_width_modulus_works() {
        // 2^512 - 569 is the largest prime below 2^512.
        let mut top = Number {
            limbs: [u64::MAX; LIMBS],
        };
        top = top.less(&Number::from_u64(568));
        let ring = Modulus::new(&top).unwrap();
        let exponent = top.less(&Number::from_u64(1));
        let base = ring.enter(&Number::from_u64(2));
        assert_eq!(ring.power(&base, &exponent), *ring.one());
    }

    #[test]
    fn debug_does_not_print_the_value() {
        let shown = format!("{:?}", number("123456789"));
        assert_eq!(shown, "Number(27 bits)");
    }
}
