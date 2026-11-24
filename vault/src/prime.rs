//! Is a number prime?
//!
//! 1. Division by every prime below 2048. A number below 2048 squared that
//!    none of them divides is prime, and that is a proof.
//! 2. The Miller-Rabin test. Write n - 1 as d * 2^s with d odd. For a prime
//!    n and any base a, either a^d = 1, or a^(d * 2^r) = -1 for some r below
//!    s (modulo n): the only square roots of one modulo a prime are one and
//!    minus one. A composite n fails this for at least three bases in four.
//!    - Below 2^64 the twelve bases 2, 3, ..., 37 are known to catch every
//!      composite (Sorenson and Webster, 2015), so the answer is a proof.
//!    - Above, base 2 and then 63 bases drawn at random: a composite passes
//!      all 64 with probability below 4^-64 = 2^-128 (Rabin; Monier). That
//!      is a bound, not a proof, and it is the test FIPS 186-5 prescribes
//!      for the primes of signature keys.

use std::sync::OnceLock;

use crate::number::{Modulus, Number};
use crate::{Error, Random};

/// Bases tried on a number of 64 bits or more.
pub const ROUNDS: usize = 64;
/// Every prime below this is tried as a divisor first.
const SIEVE: u64 = 2048;
/// These bases settle every number below 2^64.
const SURE_BELOW_2_64: [u64; 12] = [2, 3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37];

/// What the test found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Not a prime: a proof, always.
    Composite,
    /// A prime, proven.
    Prime,
    /// A prime, unless a chance below 2^-128 came up.
    ProbablePrime,
}

impl Verdict {
    #[must_use]
    pub fn is_prime(self) -> bool {
        self != Verdict::Composite
    }
}

/// The primes below [`SIEVE`], by the sieve of Eratosthenes.
fn small_primes() -> &'static [u64] {
    static PRIMES: OnceLock<Vec<u64>> = OnceLock::new();
    PRIMES.get_or_init(|| {
        let mut composite = vec![false; SIEVE as usize];
        let mut primes = Vec::new();
        for candidate in 2..SIEVE as usize {
            if !composite[candidate] {
                primes.push(candidate as u64);
                let mut multiple = candidate * candidate;
                while multiple < SIEVE as usize {
                    composite[multiple] = true;
                    multiple += candidate;
                }
            }
        }
        primes
    })
}

/// Tests `number`. `random` supplies the bases for a number of 64 bits or
/// more.
pub fn test(number: &Number, random: &mut dyn Random) -> Result<Verdict, Error> {
    if *number < Number::from_u64(2) {
        return Ok(Verdict::Composite);
    }
    for &prime in small_primes() {
        if number.remainder(prime) == 0 {
            return Ok(if number.as_u64() == Some(prime) {
                Verdict::Prime
            } else {
                Verdict::Composite
            });
        }
    }
    let small = number.as_u64();
    if small.is_some_and(|value| value < SIEVE * SIEVE) {
        return Ok(Verdict::Prime);
    }

    // number - 1 = odd * 2^twos.
    let one = Number::from_u64(1);
    let last = number.less(&one);
    let mut odd = last.clone();
    let mut twos = 0;
    while odd.is_even() {
        odd.halve();
        twos += 1;
    }
    let Some(ring) = Modulus::new(number) else {
        // Even numbers went with the division by two.
        return Ok(Verdict::Composite);
    };

    if small.is_some() {
        for base in SURE_BELOW_2_64 {
            if !passes(&ring, &odd, twos, &Number::from_u64(base)) {
                return Ok(Verdict::Composite);
            }
        }
        return Ok(Verdict::Prime);
    }

    if !passes(&ring, &odd, twos, &Number::from_u64(2)) {
        return Ok(Verdict::Composite);
    }
    let bits = number.bits();
    let two = Number::from_u64(2);
    let most = last.less(&one);
    let mut bytes = [0u8; 64];
    for _ in 1..ROUNDS {
        // A base from 2 to number - 2, each as likely as another: draw as
        // many bits as the number has and draw again when out of range.
        let base = loop {
            random.fill(&mut bytes)?;
            let mut base = Number::from_le_bytes(&bytes);
            base.keep_low(bits);
            if base >= two && base <= most {
                break base;
            }
        };
        if !passes(&ring, &odd, twos, &base) {
            return Ok(Verdict::Composite);
        }
    }
    Ok(Verdict::ProbablePrime)
}

/// Whether the number of `ring` passes Miller-Rabin to `base`.
fn passes(ring: &Modulus, odd: &Number, twos: usize, base: &Number) -> bool {
    let minus_one = ring.minus_one();
    let mut power = ring.power(&ring.enter(base), odd);
    if power == *ring.one() || power == minus_one {
        return true;
    }
    for _ in 1..twos {
        power = ring.times(&power, &power);
        if power == minus_one {
            return true;
        }
        if power == *ring.one() {
            // A square root of one that is neither one nor minus one.
            return false;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Repeatable;

    fn number(decimal: &str) -> Number {
        decimal.bytes().fold(Number::ZERO, |sum, digit| {
            sum.times_plus(10, u64::from(digit - b'0')).unwrap()
        })
    }

    fn verdict(decimal: &str) -> Verdict {
        test(&number(decimal), &mut Repeatable(7)).unwrap()
    }

    #[test]
    fn the_sieve_has_the_primes_below_2048() {
        let primes = small_primes();
        assert_eq!(primes.len(), 309);
        assert_eq!(&primes[..10], &[2, 3, 5, 7, 11, 13, 17, 19, 23, 29]);
        assert_eq!(primes.last(), Some(&2039));
    }

    #[test]
    fn every_number_below_ten_thousand_is_judged_as_trial_division_judges_it() {
        let mut primes = 0;
        for value in 0u64..10_000 {
            let by_division = value >= 2
                && (2..value)
                    .take_while(|d| d * d <= value)
                    .all(|d| value % d != 0);
            let found = test(&Number::from_u64(value), &mut Repeatable(1)).unwrap();
            assert_eq!(found.is_prime(), by_division, "{value}");
            primes += u32::from(by_division);
        }
        assert_eq!(primes, 1229);
    }

    #[test]
    fn known_primes_are_primes() {
        // Proven below 2^64.
        for prime in ["4194319", "1000000007", "18446744073709551557"] {
            assert_eq!(verdict(prime), Verdict::Prime, "{prime}");
        }
        // 2^89 - 1, 2^127 - 1, 2^128 + 51, 2^255 - 19, and 2^512 - 569,
        // the largest prime that fits.
        for prime in [
            "618970019642690137449562111",
            "170141183460469231731687303715884105727",
            "340282366920938463463374607431768211507",
            "57896044618658097711785492504343953926634992332820282019728792003956564819949",
            "13407807929942597099574024998205846127479365820592393377723561443721764030073546976801874298166903427690031858186486050853753882811946569946433649006083527",
        ] {
            assert_eq!(verdict(prime), Verdict::ProbablePrime, "{prime}");
        }
    }

    #[test]
    fn composites_that_fool_weaker_tests_are_caught() {
        for composite in [
            // 641 * 6700417, the Fermat number Euler factored.
            "4294967297",
            // Carmichael numbers, which pass Fermat's test to every base
            // they share no factor with.
            "232250619601",
            "9746347772161",
            // Strong pseudoprimes: 3215031751 passes Miller-Rabin to bases
            // 2, 3, 5 and 7, and 3825123056546413051 (three factors, the
            // least 149491) to the first nine primes.
            "3215031751",
            "3825123056546413051",
            // Above 2^64, the product of two twelve-digit primes, and it
            // passes to all twelve of the fixed bases: only the bases drawn
            // at random catch it.
            "318665857834031151167461",
            // Two large primes multiplied: (2^89 - 1) * (2^127 - 1).
            "105312291668557186697918027513529248857806893649219117400977309697",
            // A prime squared: (2^127 - 1)^2.
            "28948022309329048855892746252171976962977213799489202546401021394546514198529",
        ] {
            assert_eq!(verdict(composite), Verdict::Composite, "{composite}");
        }
    }

    #[test]
    fn the_numbers_beside_a_prime_are_not_prime() {
        let prime = number("170141183460469231731687303715884105727");
        let mut random = Repeatable(3);
        let above = prime.times_plus(1, 2).unwrap();
        let below = prime.less(&Number::from_u64(2));
        assert_eq!(test(&above, &mut random).unwrap(), Verdict::Composite);
        assert_eq!(test(&below, &mut random).unwrap(), Verdict::Composite);
    }

    #[test]
    fn a_source_that_fails_is_reported_and_not_taken_for_an_answer() {
        struct Broken;
        impl Random for Broken {
            fn fill(&mut self, _: &mut [u8]) -> Result<(), Error> {
                Err(Error::Random("no entropy".to_string()))
            }
        }
        let prime = number("170141183460469231731687303715884105727");
        assert!(matches!(test(&prime, &mut Broken), Err(Error::Random(_))));
        // Below 2^64 no random base is drawn.
        assert_eq!(
            test(&Number::from_u64(1_000_000_007), &mut Broken).unwrap(),
            Verdict::Prime
        );
    }
}
