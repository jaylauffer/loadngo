//! A password vault.
//!
//! - [`generate`] makes passwords. Each one is a prime number: its
//!   characters are the digits of a numeral, and the number that numeral
//!   spells is prime ([`prime`], on the arithmetic in [`number`]).
//! - [`store`] keeps them in one file, sealed under a master password.
//!
//! `docs/VAULT.md` has the algorithm, the file's format and what the vault
//! does and does not defend against. The loadngo launcher is the window on
//! it.

pub mod generate;
pub mod number;
pub mod prime;
pub mod store;

pub use generate::{Alphabet, Reading, Recipe};
pub use number::Number;
pub use store::{Costs, Entry, Sealed, Vault};

/// Why the vault could not do something, in words a window can show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The system gave no random bytes.
    Random(String),
    /// No prime turned up in a great many tries, which a working random
    /// source does not let happen.
    NoPrime,
    /// A password of that length is not offered for that alphabet.
    Length,
    /// Reading or writing the file failed.
    File(String),
    /// The file is not a vault.
    NotAVault,
    /// The file was written by a later version of the vault.
    Newer(u32),
    /// The file asks for more work to open it than this program will do.
    Costs,
    /// The master password is wrong, or the file has been damaged: the two
    /// cannot be told apart.
    Refused,
    /// The file is no longer the one this vault read or wrote last.
    Changed,
    /// A vault is already there.
    Exists,
    /// The file opened, and what was inside could not be read.
    Contents(String),
    /// Sealing failed.
    Sealing(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Random(why) => write!(f, "The system gave no random numbers: {why}"),
            Error::NoPrime => write!(f, "No prime was found, which should not happen"),
            Error::Length => write!(f, "That length is not offered for that alphabet"),
            Error::File(why) => write!(f, "{why}"),
            Error::NotAVault => write!(f, "The file is not a vault"),
            Error::Newer(version) => write!(
                f,
                "The vault was written by a newer launcher (format {version})"
            ),
            Error::Costs => write!(f, "The file asks for more work to open than is allowed"),
            Error::Refused => write!(f, "Wrong master password, or the file is damaged"),
            Error::Changed => write!(
                f,
                "The vault's file was changed by something else; lock and unlock to read it again"
            ),
            Error::Exists => write!(f, "A vault is already there"),
            Error::Contents(why) => write!(f, "The vault opened but could not be read: {why}"),
            Error::Sealing(why) => write!(f, "Sealing failed: {why}"),
        }
    }
}

impl std::error::Error for Error {}

/// Where random bytes come from.
pub trait Random {
    fn fill(&mut self, bytes: &mut [u8]) -> Result<(), Error>;
}

/// The operating system's source of random bytes: the one to use.
#[derive(Debug, Clone, Copy, Default)]
pub struct System;

impl Random for System {
    fn fill(&mut self, bytes: &mut [u8]) -> Result<(), Error> {
        getrandom::getrandom(bytes).map_err(|error| Error::Random(error.to_string()))
    }
}

/// A source that repeats: for tests only, which need the same numbers twice.
/// SplitMix64, which is not fit to make a secret.
#[cfg(any(test, feature = "test-random"))]
#[derive(Debug, Clone)]
pub struct Repeatable(pub u64);

#[cfg(any(test, feature = "test-random"))]
impl Random for Repeatable {
    fn fill(&mut self, bytes: &mut [u8]) -> Result<(), Error> {
        for chunk in bytes.chunks_mut(8) {
            self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut mixed = self.0;
            mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            mixed ^= mixed >> 31;
            chunk.copy_from_slice(&mixed.to_le_bytes()[..chunk.len()]);
        }
        Ok(())
    }
}
