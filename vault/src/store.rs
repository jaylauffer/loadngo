//! The vault's file: every entry in one file, sealed under a master
//! password.
//!
//! ```text
//!  0   8  "LNGVAULT"
//!  8   4  format version, 1                 (integers little-endian)
//! 12   4  key derivation, 1 = Argon2id version 1.3
//! 16   4  Argon2id memory, in KiB
//! 20   4  Argon2id passes
//! 24   4  Argon2id lanes
//! 28  32  salt
//! 60  24  nonce
//! 84   .  the entries as JSON, sealed with XChaCha20-Poly1305, then its
//!         16-byte tag; the 60 bytes before the nonce are bound in as
//!         associated data
//! ```
//!
//! The key is Argon2id(master password as typed, in UTF-8; salt) at the
//! costs the file names, 32 bytes. Every write draws a new nonce; the salt
//! and so the key stay for the life of the file. Nothing in the file is
//! readable without the key: not the names, not how many entries there are
//! beyond what the length gives away.
//!
//! Both primitives are the RustCrypto crates' (`argon2`, `chacha20poly1305`);
//! `docs/VAULT.md` says why they are not loadngo's own.

use std::io::Write;
use std::path::{Path, PathBuf};

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

use crate::{Error, Random};

const MAGIC: &[u8; 8] = b"LNGVAULT";
const VERSION: u32 = 1;
const ARGON2ID: u32 = 1;
const SALT: usize = 32;
const NONCE: usize = 24;
const TAG: usize = 16;
/// The bytes before the nonce, which the seal binds in.
const BOUND: usize = 28 + SALT;
const HEADER: usize = BOUND + NONCE;

/// What deriving the key costs. A guess at the master password costs an
/// attacker the same.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Costs {
    pub memory_kib: u32,
    pub passes: u32,
    pub lanes: u32,
}

impl Costs {
    /// 64 MiB and three passes: RFC 9106's second recommendation with one
    /// lane, about a tenth of a second on a 2024 Mac and under a second on
    /// a Raspberry Pi 4.
    pub const STANDARD: Costs = Costs {
        memory_kib: 64 * 1024,
        passes: 3,
        lanes: 1,
    };

    /// The least Argon2 allows: for tests, which open many vaults.
    pub const TRIVIAL: Costs = Costs {
        memory_kib: 8,
        passes: 1,
        lanes: 1,
    };

    /// A file may ask for no more than this, so that a file from somewhere
    /// else cannot make the launcher take all the memory there is.
    const MOST: Costs = Costs {
        memory_kib: 1024 * 1024,
        passes: 16,
        lanes: 16,
    };

    fn allowed(self) -> bool {
        self.memory_kib <= Costs::MOST.memory_kib
            && self.passes <= Costs::MOST.passes
            && self.lanes <= Costs::MOST.lanes
            && self.passes >= 1
            && self.lanes >= 1
            && self.memory_kib >= 8 * self.lanes
    }
}

/// One thing the vault keeps: what it is for, who signs in, where, and the
/// password. All of it is wiped when it is dropped.
#[derive(Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub name: String,
    #[serde(default)]
    pub account: String,
    #[serde(default)]
    pub address: String,
    pub password: String,
    /// When the password was last set, in seconds since 1970.
    #[serde(default)]
    pub changed: u64,
}

impl Drop for Entry {
    fn drop(&mut self) {
        self.name.zeroize();
        self.account.zeroize();
        self.address.zeroize();
        self.password.zeroize();
    }
}

impl std::fmt::Debug for Entry {
    /// Never the password.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Entry")
            .field("name", &self.name)
            .field("account", &self.account)
            .field("address", &self.address)
            .field("changed", &self.changed)
            .finish_non_exhaustive()
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Contents {
    entries: Vec<Entry>,
}

/// An open vault: the entries, and the key that seals them again. Dropping
/// it wipes both, which is what locking is.
pub struct Vault {
    path: PathBuf,
    costs: Costs,
    salt: [u8; SALT],
    key: Zeroizing<[u8; 32]>,
    entries: Vec<Entry>,
    /// The file as this vault last read or wrote it. A write is refused
    /// when the file on disk is no longer this.
    on_disk: Vec<u8>,
}

impl std::fmt::Debug for Vault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vault")
            .field("path", &self.path)
            .field("entries", &self.entries.len())
            .finish_non_exhaustive()
    }
}

/// A vault's file, sealed and ready to be written. Writing blocks on the
/// disk, so a window hands it to another thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sealed {
    path: PathBuf,
    bytes: Vec<u8>,
    /// What must be on disk for the write to go ahead; empty for "no file".
    expected: Vec<u8>,
}

fn derive(master: &str, salt: &[u8], costs: Costs) -> Result<Zeroizing<[u8; 32]>, Error> {
    if !costs.allowed() {
        return Err(Error::Costs);
    }
    let params = Params::new(costs.memory_kib, costs.passes, costs.lanes, Some(32))
        .map_err(|_| Error::Costs)?;
    let mut key = Zeroizing::new([0u8; 32]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(master.as_bytes(), salt, &mut key[..])
        .map_err(|error| Error::Sealing(error.to_string()))?;
    Ok(key)
}

fn header(costs: Costs, salt: &[u8; SALT]) -> [u8; BOUND] {
    let mut bound = [0u8; BOUND];
    bound[..8].copy_from_slice(MAGIC);
    bound[8..12].copy_from_slice(&VERSION.to_le_bytes());
    bound[12..16].copy_from_slice(&ARGON2ID.to_le_bytes());
    bound[16..20].copy_from_slice(&costs.memory_kib.to_le_bytes());
    bound[20..24].copy_from_slice(&costs.passes.to_le_bytes());
    bound[24..28].copy_from_slice(&costs.lanes.to_le_bytes());
    bound[28..].copy_from_slice(salt);
    bound
}

fn word(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

/// Opens sealed bytes with `key`.
fn unseal(bytes: &[u8], key: &[u8; 32]) -> Result<Vec<Entry>, Error> {
    let cipher = XChaCha20Poly1305::new(key.into());
    let plain = cipher
        .decrypt(
            XNonce::from_slice(&bytes[BOUND..HEADER]),
            Payload {
                msg: &bytes[HEADER..],
                aad: &bytes[..BOUND],
            },
        )
        .map(Zeroizing::new)
        .map_err(|_| Error::Refused)?;
    let contents: Contents =
        serde_json::from_slice(&plain).map_err(|error| Error::Contents(error.to_string()))?;
    Ok(contents.entries)
}

/// Entries in the order they are listed: by name, then by account, capitals
/// and small letters together.
fn sort(entries: &mut [Entry]) {
    fn folded(text: &str) -> impl Iterator<Item = char> + '_ {
        text.chars().flat_map(char::to_lowercase)
    }
    entries.sort_by(|a, b| {
        folded(&a.name)
            .cmp(folded(&b.name))
            .then_with(|| folded(&a.account).cmp(folded(&b.account)))
            .then_with(|| a.name.cmp(&b.name))
    });
}

impl Vault {
    /// Whether a vault's file is at `path`.
    #[must_use]
    pub fn exists(path: &Path) -> bool {
        path.exists()
    }

    /// Makes a new, empty vault at `path` under `master`, and writes it.
    /// Refuses if a file is already there. Blocks: on the key derivation
    /// and on the disk.
    pub fn create(
        path: &Path,
        master: &str,
        costs: Costs,
        random: &mut dyn Random,
    ) -> Result<Vault, Error> {
        if path.exists() {
            return Err(Error::Exists);
        }
        let mut salt = [0u8; SALT];
        random.fill(&mut salt)?;
        let key = derive(master, &salt, costs)?;
        let mut vault = Vault {
            path: path.to_path_buf(),
            costs,
            salt,
            key,
            entries: Vec::new(),
            on_disk: Vec::new(),
        };
        let written = vault.seal(random)?.write()?;
        vault.wrote(written);
        Ok(vault)
    }

    /// Opens the vault at `path` with `master`. Blocks: on the disk and on
    /// the key derivation.
    pub fn open(path: &Path, master: &str) -> Result<Vault, Error> {
        let bytes = std::fs::read(path)
            .map_err(|error| Error::File(format!("Could not read the vault: {error}")))?;
        if bytes.len() < HEADER + TAG || &bytes[..8] != MAGIC {
            return Err(Error::NotAVault);
        }
        let version = word(&bytes, 8);
        if version > VERSION {
            return Err(Error::Newer(version));
        }
        if version == 0 || word(&bytes, 12) != ARGON2ID {
            return Err(Error::NotAVault);
        }
        let costs = Costs {
            memory_kib: word(&bytes, 16),
            passes: word(&bytes, 20),
            lanes: word(&bytes, 24),
        };
        let mut salt = [0u8; SALT];
        salt.copy_from_slice(&bytes[28..BOUND]);
        let key = derive(master, &salt, costs)?;
        let mut entries = unseal(&bytes, &key)?;
        sort(&mut entries);
        Ok(Vault {
            path: path.to_path_buf(),
            costs,
            salt,
            key,
            entries,
            on_disk: bytes,
        })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The entries, in the order they are listed.
    #[must_use]
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Adds an entry and returns where it is listed. In memory only, until
    /// [`Vault::seal`] and [`Sealed::write`].
    pub fn add(&mut self, entry: Entry) -> usize {
        let key = entry.clone();
        self.entries.push(entry);
        sort(&mut self.entries);
        self.place_of(&key)
    }

    /// Puts `entry` in the place of the one listed at `index`, and returns
    /// where it is listed now.
    pub fn replace(&mut self, index: usize, entry: Entry) -> usize {
        if index >= self.entries.len() {
            return self.add(entry);
        }
        let key = entry.clone();
        self.entries[index] = entry;
        sort(&mut self.entries);
        self.place_of(&key)
    }

    /// Takes out the entry listed at `index`.
    pub fn remove(&mut self, index: usize) {
        if index < self.entries.len() {
            self.entries.remove(index);
        }
    }

    fn place_of(&self, entry: &Entry) -> usize {
        self.entries
            .iter()
            .position(|other| other == entry)
            .unwrap_or(0)
    }

    /// Seals the entries as they are now, under a new nonce, and checks
    /// that what was sealed opens again before anything is written.
    pub fn seal(&self, random: &mut dyn Random) -> Result<Sealed, Error> {
        // One allocation, sized so the entries are written out once and
        // the buffer that held them can be wiped: a JSON string is at most
        // six bytes a byte.
        let room: usize = self
            .entries
            .iter()
            .map(|entry| {
                6 * (entry.name.len()
                    + entry.account.len()
                    + entry.address.len()
                    + entry.password.len())
                    + 128
            })
            .sum::<usize>()
            + 64;
        let mut plain = Zeroizing::new(Vec::with_capacity(room));
        serde_json::to_writer(
            &mut *plain,
            &BorrowedContents {
                entries: &self.entries,
            },
        )
        .map_err(|error| Error::Sealing(error.to_string()))?;

        let bound = header(self.costs, &self.salt);
        let mut nonce = [0u8; NONCE];
        random.fill(&mut nonce)?;
        let cipher = XChaCha20Poly1305::new((&*self.key).into());
        let sealed = cipher
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: &plain,
                    aad: &bound,
                },
            )
            .map_err(|error| Error::Sealing(error.to_string()))?;
        let mut bytes = Vec::with_capacity(HEADER + sealed.len());
        bytes.extend_from_slice(&bound);
        bytes.extend_from_slice(&nonce);
        bytes.extend_from_slice(&sealed);

        // Before it can replace a good file: does it open, to these entries?
        if unseal(&bytes, &self.key)? != self.entries {
            return Err(Error::Sealing(
                "what was sealed did not open to the same entries".to_string(),
            ));
        }
        Ok(Sealed {
            path: self.path.clone(),
            bytes,
            expected: self.on_disk.clone(),
        })
    }

    /// Records that [`Sealed::write`] put these bytes on disk.
    pub fn wrote(&mut self, bytes: Vec<u8>) {
        self.on_disk = bytes;
    }
}

#[derive(Serialize)]
struct BorrowedContents<'a> {
    entries: &'a [Entry],
}

fn failed(doing: &str, path: &Path, error: &std::io::Error) -> Error {
    Error::File(format!("Could not {doing} {}: {error}", path.display()))
}

/// `path` with `.suffix` added to its file name.
fn beside(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".");
    name.push(suffix);
    path.with_file_name(name)
}

impl Sealed {
    /// Writes the file and returns the bytes written, for [`Vault::wrote`].
    ///
    /// The new file is written beside the old under another name, forced to
    /// disk and read back; only then does it take the old one's name, in
    /// one step, so at every moment the name holds a whole vault. The file
    /// it replaces is kept as `<name>.before`. Blocks on the disk.
    pub fn write(self) -> Result<Vec<u8>, Error> {
        let path = &self.path;
        // Is the file still the one the vault knows?
        match std::fs::read(path) {
            Ok(_) if self.expected.is_empty() => return Err(Error::Exists),
            Ok(found) if found != self.expected => return Err(Error::Changed),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if !self.expected.is_empty() {
                    return Err(Error::Changed);
                }
            }
            Err(error) => return Err(failed("read", path, &error)),
        }
        if let Some(folder) = path.parent() {
            std::fs::create_dir_all(folder).map_err(|error| failed("make", folder, &error))?;
        }

        let fresh = beside(path, "new");
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // The owner alone.
            options.mode(0o600);
        }
        let mut file = options
            .open(&fresh)
            .map_err(|error| failed("write", &fresh, &error))?;
        file.write_all(&self.bytes)
            .and_then(|()| file.sync_all())
            .map_err(|error| failed("write", &fresh, &error))?;
        drop(file);
        let read_back = std::fs::read(&fresh).map_err(|error| failed("read", &fresh, &error))?;
        if read_back != self.bytes {
            let _ = std::fs::remove_file(&fresh);
            return Err(Error::File(format!(
                "What was written to {} did not read back the same",
                fresh.display()
            )));
        }

        if !self.expected.is_empty() {
            // Keep the file being replaced, under a second name for the
            // same bytes where the disk allows, a copy where it does not.
            let before = beside(path, "before");
            let _ = std::fs::remove_file(&before);
            if std::fs::hard_link(path, &before).is_err() {
                std::fs::copy(path, &before).map_err(|error| failed("keep", &before, &error))?;
            }
        }
        std::fs::rename(&fresh, path).map_err(|error| failed("replace", path, &error))?;
        #[cfg(unix)]
        if let Some(folder) = path.parent() {
            // The new name is on disk when the folder is.
            if let Ok(folder) = std::fs::File::open(folder) {
                let _ = folder.sync_all();
            }
        }
        Ok(self.bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Repeatable;

    fn entry(name: &str, account: &str, password: &str) -> Entry {
        Entry {
            name: name.to_string(),
            account: account.to_string(),
            address: String::new(),
            password: password.to_string(),
            changed: 1_800_000_000,
        }
    }

    fn save(vault: &mut Vault, random: &mut Repeatable) {
        let written = vault.seal(random).unwrap().write().unwrap();
        vault.wrote(written);
    }

    #[test]
    fn what_goes_in_comes_out_with_the_master_password_and_not_without() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("passwords.vault");
        let mut random = Repeatable(1);
        let mut vault = Vault::create(&path, "the master", Costs::TRIVIAL, &mut random).unwrap();
        assert!(Vault::exists(&path));
        vault.add(entry("Mail", "jay", "p@ss \"quoted\" \\ and ünïcode"));
        vault.add(entry("bank", "", "7Kp2"));
        save(&mut vault, &mut random);
        drop(vault);

        let opened = Vault::open(&path, "the master").unwrap();
        let names: Vec<&str> = opened.entries().iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["bank", "Mail"]);
        assert_eq!(
            opened.entries()[1].password,
            "p@ss \"quoted\" \\ and ünïcode"
        );
        assert_eq!(opened.entries()[1].changed, 1_800_000_000);

        for wrong in ["the master ", "The master", ""] {
            assert_eq!(Vault::open(&path, wrong).unwrap_err(), Error::Refused);
        }
    }

    #[test]
    fn nothing_kept_can_be_read_in_the_file() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("passwords.vault");
        let mut random = Repeatable(2);
        let mut vault = Vault::create(&path, "master", Costs::TRIVIAL, &mut random).unwrap();
        vault.add(entry("FINDME-NAME", "FINDME-ACCOUNT", "FINDME-PASSWORD"));
        save(&mut vault, &mut random);
        let bytes = std::fs::read(&path).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains("FINDME"));
        assert!(!text.contains("entries"));
        assert_eq!(&bytes[..8], b"LNGVAULT");
    }

    #[test]
    fn a_changed_byte_anywhere_is_refused() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("passwords.vault");
        let mut random = Repeatable(3);
        let mut vault = Vault::create(&path, "master", Costs::TRIVIAL, &mut random).unwrap();
        vault.add(entry("one", "a", "secret"));
        save(&mut vault, &mut random);
        let good = std::fs::read(&path).unwrap();
        // Every byte of the header and the first and last of the rest. The
        // salt and the costs change the key; the nonce and the sealed bytes
        // fail the tag; the magic and the version are refused by name.
        let places: Vec<usize> = (0..HEADER).chain([HEADER, good.len() - 1]).collect();
        for at in places {
            let mut bad = good.clone();
            bad[at] ^= 0x01;
            std::fs::write(&path, &bad).unwrap();
            let error = Vault::open(&path, "master").unwrap_err();
            assert!(
                matches!(
                    error,
                    Error::Refused | Error::NotAVault | Error::Newer(_) | Error::Costs
                ),
                "byte {at}: {error:?}"
            );
        }
        // Cut short.
        std::fs::write(&path, &good[..good.len() - 1]).unwrap();
        assert_eq!(Vault::open(&path, "master").unwrap_err(), Error::Refused);
        std::fs::write(&path, &good[..40]).unwrap();
        assert_eq!(Vault::open(&path, "master").unwrap_err(), Error::NotAVault);
        std::fs::write(&path, b"{\"entries\": []}").unwrap();
        assert_eq!(Vault::open(&path, "master").unwrap_err(), Error::NotAVault);
    }

    #[test]
    fn a_file_asking_for_too_much_work_is_refused_before_any_is_done() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("passwords.vault");
        let mut random = Repeatable(4);
        Vault::create(&path, "master", Costs::TRIVIAL, &mut random).unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        // 2 GiB: a size Argon2 itself would accept, and this vault will not.
        bytes[16..20].copy_from_slice(&(2u32 * 1024 * 1024).to_le_bytes());
        std::fs::write(&path, &bytes).unwrap();
        let started = std::time::Instant::now();
        assert_eq!(Vault::open(&path, "master").unwrap_err(), Error::Costs);
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }

    #[test]
    fn every_write_looks_different_and_keeps_the_file_before_it() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("passwords.vault");
        let mut random = Repeatable(5);
        let mut vault = Vault::create(&path, "master", Costs::TRIVIAL, &mut random).unwrap();
        let empty = std::fs::read(&path).unwrap();
        // The same entries sealed twice share the header and nothing after.
        let first = vault.seal(&mut random).unwrap();
        let second = vault.seal(&mut random).unwrap();
        assert_eq!(first.bytes[..BOUND], second.bytes[..BOUND]);
        assert_ne!(first.bytes[BOUND..HEADER], second.bytes[BOUND..HEADER]);
        assert_ne!(first.bytes[HEADER..], second.bytes[HEADER..]);

        vault.add(entry("one", "", "1"));
        save(&mut vault, &mut random);
        let before = folder.path().join("passwords.vault.before");
        assert_eq!(std::fs::read(&before).unwrap(), empty);
        assert!(!folder.path().join("passwords.vault.new").exists());
        // The file before still opens, to what it held.
        assert!(Vault::open(&before, "master").unwrap().entries().is_empty());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn a_write_is_refused_when_the_file_is_not_the_one_the_vault_knows() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("passwords.vault");
        let mut random = Repeatable(6);
        let mut here = Vault::create(&path, "master", Costs::TRIVIAL, &mut random).unwrap();
        // A second window opens the same vault and adds to it.
        let mut there = Vault::open(&path, "master").unwrap();
        there.add(entry("theirs", "", "x"));
        save(&mut there, &mut random);
        // The first, not having seen that, must not write over it.
        here.add(entry("mine", "", "y"));
        assert_eq!(
            here.seal(&mut random).unwrap().write().unwrap_err(),
            Error::Changed
        );
        let now = Vault::open(&path, "master").unwrap();
        assert_eq!(now.entries().len(), 1);
        assert_eq!(now.entries()[0].name, "theirs");
        // Nor when the file has gone.
        std::fs::remove_file(&path).unwrap();
        assert_eq!(
            there.seal(&mut random).unwrap().write().unwrap_err(),
            Error::Changed
        );
        // And a new vault does not take the place of one that is there.
        std::fs::write(&path, b"something").unwrap();
        assert_eq!(
            Vault::create(&path, "master", Costs::TRIVIAL, &mut random).unwrap_err(),
            Error::Exists
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"something");
    }

    #[test]
    fn entries_are_listed_by_name_and_edits_say_where_they_landed() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("passwords.vault");
        let mut random = Repeatable(7);
        let mut vault = Vault::create(&path, "master", Costs::TRIVIAL, &mut random).unwrap();
        assert_eq!(vault.add(entry("mail", "b", "1")), 0);
        assert_eq!(vault.add(entry("Bank", "", "2")), 0);
        assert_eq!(vault.add(entry("Mail", "a", "3")), 1);
        assert_eq!(vault.add(entry("zoo", "", "4")), 3);
        let names: Vec<String> = vault
            .entries()
            .iter()
            .map(|entry| format!("{} {}", entry.name, entry.account))
            .collect();
        assert_eq!(names, ["Bank ", "Mail a", "mail b", "zoo "]);
        assert_eq!(vault.replace(0, entry("Yard", "", "5")), 2);
        vault.remove(0);
        assert_eq!(vault.entries().len(), 3);
        assert_eq!(vault.entries()[0].account, "b");
        vault.remove(99);
        assert_eq!(vault.entries().len(), 3);
    }

    #[test]
    fn the_key_is_argon2id_by_a_known_answer() {
        // The reference implementation's own test (phc-winner-argon2,
        // test.c: version 1.3, two passes, 64 MiB, one lane), which
        // OpenSSL 3.6 gives too:
        //   openssl kdf -keylen 32 -kdfopt pass:password -kdfopt salt:somesalt \
        //     -kdfopt iter:2 -kdfopt memcost:65536 -kdfopt lanes:1 ARGON2ID
        let costs = Costs {
            memory_kib: 65536,
            passes: 2,
            lanes: 1,
        };
        let key = derive("password", b"somesalt", costs).unwrap();
        let hex: String = key.iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(
            hex,
            "09316115d5cf24ed5a15a31a3ba326e5cf32edc24702987c02b6566f61913cf7"
        );
    }

    /// Not a check: prints how long the key takes at the standard costs, to
    /// measure a machine with.
    /// `cargo test -p loadngo-vault --release -- --ignored --nocapture standard_costs`
    #[test]
    #[ignore = "a measurement, run by hand"]
    fn how_long_the_standard_costs_take() {
        let salt = [7u8; SALT];
        for _ in 0..3 {
            let started = std::time::Instant::now();
            derive("a master password", &salt, Costs::STANDARD).unwrap();
            println!("Argon2id at the standard costs: {:?}", started.elapsed());
        }
    }

    #[test]
    fn the_password_the_salt_and_the_costs_each_change_the_key() {
        let salt = [7u8; SALT];
        let base = derive("master", &salt, Costs::TRIVIAL).unwrap();
        assert_eq!(*base, *derive("master", &salt, Costs::TRIVIAL).unwrap());
        assert_ne!(*base, *derive("Master", &salt, Costs::TRIVIAL).unwrap());
        assert_ne!(
            *base,
            *derive("master", &[8u8; SALT], Costs::TRIVIAL).unwrap()
        );
        let more = Costs {
            passes: 2,
            ..Costs::TRIVIAL
        };
        assert_ne!(*base, *derive("master", &salt, more).unwrap());
        assert_eq!(
            derive("master", &salt, Costs { lanes: 0, ..more }).unwrap_err(),
            Error::Costs
        );
    }

    #[test]
    fn debug_shows_no_password() {
        let shown = format!("{:?}", entry("Mail", "jay", "FINDME"));
        assert!(
            shown.contains("Mail") && !shown.contains("FINDME"),
            "{shown}"
        );
    }
}
