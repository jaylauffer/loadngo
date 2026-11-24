//! Passwords that are prime numbers.
//!
//! A password is read as a numeral: each character is a digit, worth its
//! place in the alphabet, and the leftmost counts most. `7Kp1` in the
//! alphabet of digits and letters (base 62) is
//! 7 * 62^3 + 20 * 62^2 + 51 * 62 + 1 = 1,748,339, which is prime. A
//! password is made by drawing
//! numerals of the wanted length from the system's random source, every
//! numeral as likely as another, until one spells a prime
//! ([`crate::prime`]). So each prime of that length is as likely as any
//! other, and nothing about one password says anything about the next.
//!
//! Two different primes share no factor. That is the sense in which each
//! password stands alone, and it can be checked: [`read`] says of any
//! password whether it is a prime, without being told where it came from.

use zeroize::{Zeroize, Zeroizing};

use crate::number::Number;
use crate::prime::{self, Verdict};
use crate::{Error, Random};

/// The characters a password is written in. Each is in ASCII order, so the
/// larger number is the later password in a sorted list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Alphabet {
    /// `0` to `9`: base 10.
    Digits,
    /// `0-9`, `A-Z`, `a-z`: base 62.
    Letters,
    /// Every character a keyboard prints but the space, `!` to `~`: base 94.
    Keyboard,
}

impl Alphabet {
    pub const ALL: [Alphabet; 3] = [Alphabet::Digits, Alphabet::Letters, Alphabet::Keyboard];

    #[must_use]
    pub const fn base(self) -> u64 {
        match self {
            Alphabet::Digits => 10,
            Alphabet::Letters => 62,
            Alphabet::Keyboard => 94,
        }
    }

    /// What the alphabet is called where one is chosen.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Alphabet::Digits => "Digits",
            Alphabet::Letters => "Letters and digits",
            Alphabet::Keyboard => "Letters, digits and signs",
        }
    }

    /// The character worth `value`, for a value below the base.
    #[must_use]
    pub fn character(self, value: u64) -> char {
        debug_assert!(value < self.base());
        let byte = match self {
            Alphabet::Digits => b'0' + value as u8,
            Alphabet::Letters => match value {
                0..=9 => b'0' + value as u8,
                10..=35 => b'A' + (value - 10) as u8,
                _ => b'a' + (value - 36) as u8,
            },
            Alphabet::Keyboard => b'!' + value as u8,
        };
        char::from(byte)
    }

    /// What `character` is worth, if the alphabet has it.
    #[must_use]
    pub fn value(self, character: char) -> Option<u64> {
        let value = match (self, character) {
            (Alphabet::Digits | Alphabet::Letters, '0'..='9') => character as u64 - '0' as u64,
            (Alphabet::Letters, 'A'..='Z') => character as u64 - 'A' as u64 + 10,
            (Alphabet::Letters, 'a'..='z') => character as u64 - 'a' as u64 + 36,
            (Alphabet::Keyboard, '!'..='~') => character as u64 - '!' as u64,
            _ => return None,
        };
        Some(value)
    }

    /// The smallest alphabet that has every character of `text`.
    #[must_use]
    pub fn holding(text: &str) -> Option<Alphabet> {
        if text.is_empty() {
            return None;
        }
        Alphabet::ALL.into_iter().find(|alphabet| {
            text.chars()
                .all(|character| alphabet.value(character).is_some())
        })
    }

    /// The lengths a password in this alphabet is made at. The shortest is
    /// the shortest that leaves room for one character of each kind and
    /// some choice; the longest fits a [`Number`].
    #[must_use]
    pub const fn lengths(self) -> std::ops::RangeInclusive<usize> {
        match self {
            Alphabet::Digits => 4..=64,
            Alphabet::Letters | Alphabet::Keyboard => 8..=64,
        }
    }

    /// The kinds of character a password in this alphabet has one of each
    /// of, so that a site asking for "a capital, a digit and a sign" is
    /// satisfied and so that a password reads back in the alphabet it was
    /// made in.
    fn kinds(self) -> &'static [Kind] {
        match self {
            Alphabet::Digits => &[Kind::Digit],
            Alphabet::Letters => &[Kind::Digit, Kind::Capital, Kind::Small],
            Alphabet::Keyboard => &[Kind::Digit, Kind::Capital, Kind::Small, Kind::Sign],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Digit,
    Capital,
    Small,
    Sign,
}

impl Kind {
    fn of(character: char) -> Kind {
        match character {
            '0'..='9' => Kind::Digit,
            'A'..='Z' => Kind::Capital,
            'a'..='z' => Kind::Small,
            _ => Kind::Sign,
        }
    }

    /// How many characters of the kind the keyboard has.
    const fn count(self) -> f64 {
        match self {
            Kind::Digit => 10.0,
            Kind::Capital | Kind::Small => 26.0,
            Kind::Sign => 32.0,
        }
    }
}

/// What to make: an alphabet and a number of characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Recipe {
    pub alphabet: Alphabet,
    pub length: usize,
}

impl Default for Recipe {
    /// Every kind of character, 23 of them: 143.9 bits.
    fn default() -> Self {
        Recipe {
            alphabet: Alphabet::Keyboard,
            length: 23,
        }
    }
}

impl Recipe {
    /// The recipe with its length brought inside what the alphabet offers.
    #[must_use]
    pub fn fitted(self) -> Recipe {
        let lengths = self.alphabet.lengths();
        Recipe {
            alphabet: self.alphabet,
            length: self.length.clamp(*lengths.start(), *lengths.end()),
        }
    }

    /// How hard a password of this recipe is to guess, in bits: the base-2
    /// logarithm of how many passwords the recipe can make.
    ///
    /// The numerals of the length that do not begin with the zero character
    /// number B^L - B^(L-1). By the prime number theorem about one in
    /// ln(B^L) of them is prime, and of those the share with a character of
    /// every kind is found by inclusion and exclusion. An estimate: for six
    /// digits it says 65,144 where there are 68,906.
    #[must_use]
    pub fn bits(self) -> f64 {
        let base = self.alphabet.base() as f64;
        let length = self.length as f64;
        let numerals = length * base.log2() + (1.0 - 1.0 / base).log2();
        let primes = numerals - (length * base.ln()).log2();
        // The share of all strings of the length with every kind present.
        let kinds = self.alphabet.kinds();
        let mut share = 0.0;
        for missing in 0u32..(1 << kinds.len()) {
            let gone: f64 = kinds
                .iter()
                .enumerate()
                .filter(|(index, _)| missing >> index & 1 == 1)
                .map(|(_, kind)| kind.count())
                .sum();
            let sign = if missing.count_ones() % 2 == 0 {
                1.0
            } else {
                -1.0
            };
            share += sign * ((base - gone) / base).powf(length);
        }
        primes + share.log2()
    }
}

/// Whether `text`, in `alphabet`, has a character of every kind the
/// alphabet asks for.
fn mixed(alphabet: Alphabet, text: &str) -> bool {
    alphabet
        .kinds()
        .iter()
        .all(|kind| text.chars().any(|character| Kind::of(character) == *kind))
}

/// The number `text` spells in `alphabet`, if every character is in it and
/// the number fits.
fn spelled(alphabet: Alphabet, text: &str) -> Option<Number> {
    let mut number = Number::ZERO;
    for character in text.chars() {
        number = number.times_plus(alphabet.base(), alphabet.value(character)?)?;
    }
    Some(number)
}

/// How many numerals are tried before giving up. About one numeral in
/// `length * ln(base)` is prime, at most one in 291, so this is not
/// reached with a random source that works.
const TRIES: usize = 200_000;

/// Makes a password to `recipe`: a prime, each prime the recipe allows as
/// likely as another.
pub fn make(recipe: Recipe, random: &mut dyn Random) -> Result<Zeroizing<String>, Error> {
    if !recipe.alphabet.lengths().contains(&recipe.length) {
        return Err(Error::Length);
    }
    let base = recipe.alphabet.base();
    // A byte below this is as likely to leave any remainder as any other.
    let fair = (256 / base * base) as u8;
    let mut pool = Zeroizing::new([0u8; 256]);
    let mut used = pool.len();
    let mut text = Zeroizing::new(String::with_capacity(recipe.length));
    for _ in 0..TRIES {
        text.zeroize();
        let mut draws = 0;
        while text.len() < recipe.length {
            if used == pool.len() {
                random.fill(&mut pool[..])?;
                used = 0;
                draws += 1;
                if draws > 64 {
                    // A source stuck on bytes that are never fair.
                    return Err(Error::NoPrime);
                }
            }
            let byte = pool[used];
            used += 1;
            if byte >= fair {
                continue;
            }
            let value = u64::from(byte) % base;
            if text.is_empty() && value == 0 {
                // A numeral does not begin with its zero.
                continue;
            }
            text.push(recipe.alphabet.character(value));
        }
        if !mixed(recipe.alphabet, &text) {
            continue;
        }
        let Some(number) = spelled(recipe.alphabet, &text) else {
            return Err(Error::Length);
        };
        if prime::test(&number, random)?.is_prime() {
            return Ok(text);
        }
    }
    Err(Error::NoPrime)
}

/// What a password is, read as a number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reading {
    /// It is a prime, written in `alphabet`. `proven` when the test was a
    /// proof (the number is below 2^64).
    Prime {
        alphabet: Alphabet,
        proven: bool,
        number: Number,
    },
    /// It is a numeral, and the number is not prime (or the numeral begins
    /// with its alphabet's zero, which a number's numeral does not).
    NotPrime,
    /// It has a character no alphabet here has, or is empty, or is too long
    /// to be a number here.
    NotANumber,
}

impl Reading {
    #[must_use]
    pub fn is_prime(&self) -> bool {
        matches!(self, Reading::Prime { .. })
    }
}

/// Reads `text` as a number in the smallest alphabet that has all its
/// characters, and tests it.
pub fn read(text: &str, random: &mut dyn Random) -> Result<Reading, Error> {
    let Some(alphabet) = Alphabet::holding(text) else {
        return Ok(Reading::NotANumber);
    };
    let Some(number) = spelled(alphabet, text) else {
        return Ok(Reading::NotANumber);
    };
    if text.chars().next().and_then(|first| alphabet.value(first)) == Some(0) {
        return Ok(Reading::NotPrime);
    }
    Ok(match prime::test(&number, random)? {
        Verdict::Composite => Reading::NotPrime,
        Verdict::Prime => Reading::Prime {
            alphabet,
            proven: true,
            number,
        },
        Verdict::ProbablePrime => Reading::Prime {
            alphabet,
            proven: false,
            number,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Repeatable;
    use std::collections::HashSet;

    #[test]
    fn each_alphabet_counts_from_zero_in_ascii_order() {
        for alphabet in Alphabet::ALL {
            let characters: Vec<char> = (0..alphabet.base())
                .map(|value| alphabet.character(value))
                .collect();
            assert!(characters.windows(2).all(|pair| pair[0] < pair[1]));
            for (value, character) in characters.iter().enumerate() {
                assert_eq!(alphabet.value(*character), Some(value as u64));
            }
            assert_eq!(alphabet.value(' '), None);
            assert_eq!(alphabet.value('é'), None);
        }
        assert_eq!(Alphabet::Keyboard.character(0), '!');
        assert_eq!(Alphabet::Keyboard.character(93), '~');
        assert_eq!(Alphabet::Letters.character(61), 'z');
        assert_eq!(Alphabet::Digits.value('a'), None);
        assert_eq!(Alphabet::Letters.value('-'), None);
    }

    #[test]
    fn a_numeral_spells_its_number() {
        // 7 * 62^3 + 20 * 62^2 + 51 * 62 + 1.
        assert_eq!(
            spelled(Alphabet::Letters, "7Kp1").unwrap().decimal(),
            "1748339"
        );
        assert_eq!(spelled(Alphabet::Digits, "0042").unwrap().decimal(), "42");
        // '!' is zero and '~' is 93: "~~" is 93 * 94 + 93.
        assert_eq!(spelled(Alphabet::Keyboard, "~~").unwrap().decimal(), "8835");
        assert!(spelled(Alphabet::Digits, "12a").is_none());
        // 78 characters of base 94 are 511.3 bits and fit; 79 do not.
        assert!(spelled(Alphabet::Keyboard, &"~".repeat(79)).is_none());
        assert!(spelled(Alphabet::Keyboard, &"~".repeat(78)).is_some());
    }

    #[test]
    fn what_is_made_is_a_prime_of_the_length_with_every_kind_of_character() {
        let mut random = Repeatable(2026);
        for alphabet in Alphabet::ALL {
            for length in [*alphabet.lengths().start(), 13, 23, 64] {
                let recipe = Recipe { alphabet, length };
                let password = make(recipe, &mut random).unwrap();
                assert_eq!(password.chars().count(), length);
                assert!(mixed(alphabet, &password), "{alphabet:?} {length}");
                assert_ne!(alphabet.value(password.chars().next().unwrap()), Some(0));
                // It reads back as a prime, in the alphabet it was made in.
                match read(&password, &mut random).unwrap() {
                    Reading::Prime {
                        alphabet: read_in, ..
                    } => assert_eq!(read_in, alphabet),
                    other => panic!("{alphabet:?} {length}: {other:?}"),
                }
            }
        }
    }

    #[test]
    fn at_the_shortest_length_too_every_kind_of_character_is_there() {
        // Eight characters drawn freely have every kind less than half the
        // time (letters and digits: 73 in 100; with signs: 42 in 100), so
        // sixty in a row with every kind is the rule at work, not luck.
        let mut random = Repeatable(77);
        for alphabet in [Alphabet::Letters, Alphabet::Keyboard] {
            let recipe = Recipe {
                alphabet,
                length: 8,
            };
            for _ in 0..60 {
                let password = make(recipe, &mut random).unwrap();
                assert!(mixed(alphabet, &password), "{alphabet:?}");
                assert_eq!(Alphabet::holding(&password), Some(alphabet));
            }
        }
    }

    #[test]
    fn a_made_password_is_prime_by_a_test_that_shares_no_code_with_the_maker() {
        // Six digits: small enough to settle by dividing.
        let mut random = Repeatable(11);
        let recipe = Recipe {
            alphabet: Alphabet::Digits,
            length: 6,
        };
        let mut seen = HashSet::new();
        for _ in 0..300 {
            let password = make(recipe, &mut random).unwrap();
            let value: u64 = password.parse().unwrap();
            assert!((100_000..1_000_000).contains(&value));
            assert!(
                (2..1000).all(|divisor| !value.is_multiple_of(divisor)),
                "{value}"
            );
            seen.insert(value);
        }
        // 300 draws from 68,906 primes: a repeat or two is possible, many
        // are not.
        assert!(seen.len() > 290, "{}", seen.len());
    }

    #[test]
    fn every_prime_of_the_length_is_made_about_as_often_as_another() {
        // The 1,061 primes of four digits, 60 draws each on average. With
        // every prime as likely as another the counts are Poisson around
        // 60: none should be far out, and none missing.
        let mut random = Repeatable(5);
        let recipe = Recipe {
            alphabet: Alphabet::Digits,
            length: 4,
        };
        let mut counts = std::collections::HashMap::new();
        let draws = 1061 * 60;
        for _ in 0..draws {
            let password = make(recipe, &mut random).unwrap();
            *counts.entry(password.to_string()).or_insert(0u32) += 1;
        }
        assert_eq!(counts.len(), 1061);
        let least = counts.values().min().unwrap();
        let most = counts.values().max().unwrap();
        // Six standard deviations either side of 60.
        assert!(*least > 13 && *most < 107, "{least} to {most}");
        // Chi-squared against the even spread: 1,060 degrees of freedom,
        // mean 1,060, standard deviation 46.
        let chi: f64 = counts
            .values()
            .map(|&count| (f64::from(count) - 60.0).powi(2) / 60.0)
            .sum();
        assert!((860.0..1260.0).contains(&chi), "{chi}");
    }

    #[test]
    fn the_same_source_makes_the_same_password_and_another_source_another() {
        let recipe = Recipe::default();
        let first = make(recipe, &mut Repeatable(1)).unwrap();
        let again = make(recipe, &mut Repeatable(1)).unwrap();
        let other = make(recipe, &mut Repeatable(2)).unwrap();
        assert_eq!(*first, *again);
        assert_ne!(*first, *other);
    }

    #[test]
    fn a_length_the_alphabet_does_not_offer_is_refused() {
        for (alphabet, length) in [
            (Alphabet::Digits, 3),
            (Alphabet::Letters, 7),
            (Alphabet::Keyboard, 65),
        ] {
            let recipe = Recipe { alphabet, length };
            assert_eq!(make(recipe, &mut Repeatable(1)).unwrap_err(), Error::Length);
            assert!(alphabet.lengths().contains(&recipe.fitted().length));
        }
    }

    #[test]
    fn a_source_stuck_on_one_byte_ends_in_an_error_not_a_loop() {
        struct Stuck(u8);
        impl Random for Stuck {
            fn fill(&mut self, bytes: &mut [u8]) -> Result<(), Error> {
                bytes.fill(self.0);
                Ok(())
            }
        }
        for byte in [0u8, 255, b'A'] {
            let made = make(Recipe::default(), &mut Stuck(byte));
            assert_eq!(made.unwrap_err(), Error::NoPrime, "{byte}");
        }
    }

    #[test]
    fn a_byte_that_would_favour_some_characters_is_never_used() {
        // 250 to 255 are the bytes left over when 256 is cut into tens:
        // used, they would make 0 to 5 likelier than 6 to 9. Taken modulo
        // ten these four spell 1013, a prime, so a maker that used them
        // would hand it back. This one draws again, and finds nothing.
        struct Leftovers;
        impl Random for Leftovers {
            fn fill(&mut self, bytes: &mut [u8]) -> Result<(), Error> {
                for (index, byte) in bytes.iter_mut().enumerate() {
                    *byte = [251, 250, 251, 253][index % 4];
                }
                Ok(())
            }
        }
        let recipe = Recipe {
            alphabet: Alphabet::Digits,
            length: 4,
        };
        assert_eq!(make(recipe, &mut Leftovers).unwrap_err(), Error::NoPrime);
    }

    #[test]
    fn reading_says_what_a_password_is() {
        let mut random = Repeatable(9);
        // 1000003 is prime; 1000001 = 101 * 9901.
        assert!(matches!(
            read("1000003", &mut random).unwrap(),
            Reading::Prime {
                alphabet: Alphabet::Digits,
                proven: true,
                ..
            }
        ));
        assert_eq!(read("1000001", &mut random).unwrap(), Reading::NotPrime);
        // A numeral does not begin with zero, whatever follows.
        assert_eq!(read("01000003", &mut random).unwrap(), Reading::NotPrime);
        assert_eq!(read("", &mut random).unwrap(), Reading::NotANumber);
        assert_eq!(
            read("correct horse", &mut random).unwrap(),
            Reading::NotANumber
        );
        assert_eq!(read("pässword", &mut random).unwrap(), Reading::NotANumber);
        assert_eq!(
            read(&"9".repeat(200), &mut random).unwrap(),
            Reading::NotANumber
        );
        // One character changed in a made password: almost never prime.
        let made = make(Recipe::default(), &mut random).unwrap();
        let mut changed = made.to_string();
        let last = changed.pop().unwrap();
        changed.push(if last == 'x' { 'y' } else { 'x' });
        let mut primes = 0;
        for index in 0..made.len() {
            let mut typo: Vec<char> = made.chars().collect();
            typo[index] = if typo[index] == 'Q' { 'R' } else { 'Q' };
            let typo: String = typo.into_iter().collect();
            primes += u32::from(read(&typo, &mut random).unwrap().is_prime());
        }
        assert!(
            primes <= 2,
            "{primes} of {} typos read as primes",
            made.len()
        );
    }

    #[test]
    fn strength_is_about_the_count_of_primes_the_recipe_can_make() {
        // Six digits: 68,906 primes, 16.07 bits; the estimate says 15.99.
        let six = Recipe {
            alphabet: Alphabet::Digits,
            length: 6,
        }
        .bits();
        assert!((six - 68_906f64.log2()).abs() < 0.1, "{six}");
        // Four digits: 1,061 primes.
        let four = Recipe {
            alphabet: Alphabet::Digits,
            length: 4,
        }
        .bits();
        assert!((four - 1_061f64.log2()).abs() < 0.15, "{four}");
        // The default: 23 characters of 94 is 150.8 bits of numerals, less
        // 6.7 for being prime and a little for having every kind.
        let standard = Recipe::default().bits();
        assert!((143.5..144.5).contains(&standard), "{standard}");
        // Longer is stronger, and a larger alphabet is stronger.
        let letters = Recipe {
            alphabet: Alphabet::Letters,
            length: 23,
        }
        .bits();
        assert!(letters < standard);
        assert!(
            Recipe {
                alphabet: Alphabet::Keyboard,
                length: 24
            }
            .bits()
                > standard
        );
    }
}
