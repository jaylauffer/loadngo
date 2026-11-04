//! Version control on the Archive CAS: a repository is a history of signed versions of
//! a tree of files, every piece named by the BLAKE3 hash of its bytes. The design and
//! the commands are in `docs/REPOSITORY.md`.
//!
//! - A **tree** ([`TREE_FORMAT`]) is one directory: its entries sorted by name, each a
//!   file, an executable file, a symlink or another tree (with that tree's file and byte
//!   totals). A version that changes one file writes new trees only along that file's
//!   path; every other directory is the same object as before.
//! - A **version** ([`VERSION_FORMAT`]) names its parents, its root tree, its author,
//!   time and message, the changes from its first parent, and, for an imported commit,
//!   where it came from. It holds no signature and no import time, so the same history
//!   gives the same version hashes on every machine.
//! - A **head** ([`HEAD_FORMAT`]) is a signed claim that a branch of a repository is at
//!   a version, made with a post-quantum key (Dilithium2, `loadngo-pq-crypto`). A
//!   version is trusted when it is, or is an ancestor of, the version of a head whose
//!   signature verifies against a trusted key. One signature covers the whole history
//!   below it, because every version names its parents and its tree by hash and every
//!   read is checked against its hash.
//!
//! Objects are stored in an Archive CAS root's `objects/` (compression, the free-space
//! floor and its hashing apply unchanged). Heads, and every head claim ever made, are in
//! `repositories/<id>/heads/<branch>.json` and `repositories/<id>/signatures/`.
//! `archive_cas_gc` and `archive_cas_purge` count what [`live_objects`] lists as live.

pub mod ignore;
pub mod import;
pub mod transfer;
pub mod work_tree;

use crate::archive_cas::{ArchiveCasStorage, ArchiveChange, ArchiveMove, ArchiveObject};
use crate::cas::CasHash;
use anyhow::{anyhow, bail, Context, Result};
use loadngo_pq_crypto::{default_registry, PqSchemeRegistry, PrivateKey, PublicKey, Signature};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub const TREE_FORMAT: &str = "loadngo-repository-tree-v1";
pub const VERSION_FORMAT: &str = "loadngo-repository-version-v1";
pub const HEAD_FORMAT: &str = "loadngo-repository-head-v1";
/// The `loadngo-anchor` domain a head signature is made in, so it cannot be replayed as
/// a signature over any other kind of record.
pub const HEAD_DOMAIN: &str = "loadngo.repository.head";
pub const HEAD_DOMAIN_VERSION: u16 = 1;
/// Under a store's root: one folder per repository.
pub const REPOSITORIES_DIR: &str = "repositories";
/// Names no tree may hold: git's own folder (a checkout must never plant one) and the
/// file that marks a checkout.
pub const RESERVED_NAMES: &[&str] = &[".git", work_tree::MARKER_FILE];

/// One entry of a [`Tree`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TreeEntry {
    File {
        name: String,
        object: ArchiveObject,
    },
    Executable {
        name: String,
        object: ArchiveObject,
    },
    Symlink {
        name: String,
        target: String,
    },
    /// Another tree, with the number of files (and symlinks) and the bytes of the files
    /// below it, so a listing can show a folder's size without opening it.
    Directory {
        name: String,
        tree: CasHash,
        files: u64,
        bytes: u64,
    },
}

impl TreeEntry {
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::File { name, .. }
            | Self::Executable { name, .. }
            | Self::Symlink { name, .. }
            | Self::Directory { name, .. } => name,
        }
    }

    /// This entry's file and byte totals.
    #[must_use]
    pub fn totals(&self) -> (u64, u64) {
        match self {
            Self::File { object, .. } | Self::Executable { object, .. } => (1, object.size),
            Self::Symlink { .. } => (1, 0),
            Self::Directory { files, bytes, .. } => (*files, *bytes),
        }
    }

    /// What the entry holds, without its name: equal identities are the same content
    /// placed somewhere (used to find moves).
    fn identity(&self) -> (u8, String) {
        match self {
            Self::File { object, .. } => (b'f', object.hash.to_hex()),
            Self::Executable { object, .. } => (b'x', object.hash.to_hex()),
            Self::Symlink { target, .. } => (b'l', target.clone()),
            Self::Directory { tree, .. } => (b'd', tree.to_hex()),
        }
    }
}

/// One directory of a version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tree {
    pub format: String,
    pub entries: Vec<TreeEntry>,
}

impl Tree {
    /// A tree of `entries`, sorted by name.
    ///
    /// # Errors
    /// A name that is empty, `.`, `..`, reserved or holds `/` or NUL; a name twice; an
    /// empty or NUL-holding symlink target.
    pub fn new(mut entries: Vec<TreeEntry>) -> Result<Self> {
        entries.sort_by(|a, b| a.name().as_bytes().cmp(b.name().as_bytes()));
        let tree = Self {
            format: TREE_FORMAT.to_string(),
            entries,
        };
        tree.validate()?;
        Ok(tree)
    }

    fn validate(&self) -> Result<()> {
        if self.format != TREE_FORMAT {
            bail!("unsupported tree format {:?}", self.format);
        }
        for pair in self.entries.windows(2) {
            if pair[0].name().as_bytes() >= pair[1].name().as_bytes() {
                bail!(
                    "tree entries are not sorted and unique: {:?} then {:?}",
                    pair[0].name(),
                    pair[1].name()
                );
            }
        }
        for entry in &self.entries {
            validate_name(entry.name())?;
            if let TreeEntry::Symlink { target, name } = entry {
                if target.is_empty() || target.contains('\0') {
                    bail!("symlink {name:?} has an empty or NUL-holding target");
                }
            }
        }
        Ok(())
    }

    /// # Errors
    /// An invalid tree (see [`Tree::new`]).
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        self.validate()?;
        serde_json::to_vec(self).context("failed to serialize a tree")
    }

    /// Files (and symlinks) and file bytes in this tree and every tree below it.
    #[must_use]
    pub fn totals(&self) -> (u64, u64) {
        self.entries.iter().fold((0, 0), |(files, bytes), entry| {
            let (f, b) = entry.totals();
            (files + f, bytes + b)
        })
    }
}

/// Checks one path component a tree may hold.
///
/// # Errors
/// When it may not.
pub fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\0') {
        bail!("{name:?} is not a valid file name in a tree");
    }
    // `\` and `:` are path syntax on Windows (`..\x`, `C:x`, `name::$DATA`), and Windows
    // drops a trailing dot or space, so `a.` would land on `a`.
    if name.contains('\\') || name.contains(':') || name.ends_with('.') || name.ends_with(' ') {
        bail!("{name:?} cannot be checked out safely on every platform");
    }
    // What a file system may treat as one of the reserved names: any case, HFS+'s
    // ignored code points, and NTFS's short name for `.git`.
    let folded: String = name
        .chars()
        .filter(|c| !is_hfs_ignorable(*c))
        .collect::<String>()
        .to_ascii_lowercase();
    if RESERVED_NAMES
        .iter()
        .any(|reserved| folded == reserved.to_ascii_lowercase())
        || folded == "git~1"
    {
        bail!("{name:?} is reserved and cannot be held in a tree");
    }
    Ok(())
}

/// Whether Windows would not create `name` as an ordinary file: a device name (`CON`,
/// `NUL`, `COM1`... with or without an extension) or a character Windows forbids. Trees
/// may hold these (git on Linux does), so a checkout on Windows refuses them.
#[must_use]
pub fn is_windows_unsafe(name: &str) -> bool {
    if name
        .chars()
        .any(|c| matches!(c, '<' | '>' | '"' | '|' | '?' | '*') || u32::from(c) < 0x20)
    {
        return true;
    }
    let base = name
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    matches!(
        base.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ((base.starts_with("COM") || base.starts_with("LPT"))
        && base.len() == 4
        && base.as_bytes()[3].is_ascii_digit()
        && base.as_bytes()[3] != b'0')
}

/// Code points HFS+ ignores when comparing names (as git's `is_hfs_dotgit` does).
fn is_hfs_ignorable(c: char) -> bool {
    matches!(
        c,
        '\u{200c}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{206a}'..='\u{206f}' | '\u{feff}'
    )
}

/// Where an imported version came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Origin {
    /// `git` for a commit imported by `loadngo-repo import`.
    pub system: String,
    /// The commit's id in that system.
    pub commit: String,
    pub committer: String,
    pub committed_at_unix_secs: u64,
}

/// One version of a repository. Its hash is the BLAKE3 hash of its canonical bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Version {
    pub format: String,
    pub repository_id: String,
    /// None for the first version; the first parent is the one `changes` are against.
    pub parents: Vec<CasHash>,
    pub tree: CasHash,
    pub author: String,
    pub authored_at_unix_secs: u64,
    pub message: String,
    /// What changed from the first parent: created, changed, deleted and moved paths (a
    /// folder created or deleted whole is one path).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changes: Vec<ArchiveChange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
}

impl Version {
    fn validate(&self) -> Result<()> {
        if self.format != VERSION_FORMAT {
            bail!("unsupported version format {:?}", self.format);
        }
        validate_id(&self.repository_id, "repository id")?;
        let mut seen = HashSet::new();
        if let Some(twice) = self.parents.iter().find(|parent| !seen.insert(**parent)) {
            bail!("parent {twice} is named twice");
        }
        if self.author.trim().is_empty() {
            bail!("a version needs an author");
        }
        Ok(())
    }

    /// # Errors
    /// An invalid version.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        self.validate()?;
        serde_json::to_vec(self).context("failed to serialize a version")
    }

    /// The first line of the message.
    #[must_use]
    pub fn subject(&self) -> &str {
        self.message.lines().next().unwrap_or("").trim_end()
    }
}

/// Repository ids, branch names and signer identities: lowercase letters, digits and
/// hyphens, as archive ids are.
///
/// # Errors
/// Anything else.
pub fn validate_id(value: &str, what: &str) -> Result<()> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        bail!("{what} {value:?} must be lowercase letters, digits and hyphens");
    }
    Ok(())
}

/// A key that signs heads.
pub struct Signer {
    pub identity: String,
    pub public: PublicKey,
    pub private: PrivateKey,
}

impl Signer {
    /// # Errors
    /// An invalid identity, unreadable keys, or keys of different schemes.
    pub fn from_files(identity: &str, public: &Path, private: &Path) -> Result<Self> {
        validate_id(identity, "signer identity")?;
        let public = crate::archive_cas_sign::read_public_key(public)?;
        let private = crate::archive_cas_sign::read_private_key(private)?;
        if public.scheme != private.scheme {
            bail!(
                "public and private keys are of different schemes: {} and {}",
                public.scheme,
                private.scheme
            );
        }
        Ok(Self {
            identity: identity.to_string(),
            public,
            private,
        })
    }

    /// The one `<identity>.dilithium2.key` with its `.pub` in `~/.loadngo/keys`, where
    /// `archive_cas_sign` keys live. `None` when there is no such pair or more than one.
    ///
    /// # Errors
    /// When the pair found cannot be read.
    pub fn from_default_keys() -> Result<Option<Self>> {
        let Some(dir) = keys_dir() else {
            return Ok(None);
        };
        let Ok(entries) = fs::read_dir(&dir) else {
            return Ok(None);
        };
        let mut pairs = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if let Some(identity) = name.strip_suffix(".dilithium2.key") {
                let public = dir.join(format!("{identity}.dilithium2.pub"));
                if public.is_file() {
                    pairs.push((identity.to_string(), public, path.clone()));
                }
            }
        }
        match pairs.as_slice() {
            [(identity, public, private)] => Self::from_files(identity, public, private).map(Some),
            _ => Ok(None),
        }
    }
}

fn keys_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(Path::new(&home).join(".loadngo").join("keys"))
}

/// The public keys whose head signatures are accepted.
pub struct Trust {
    keys: Vec<PublicKey>,
}

impl Trust {
    #[must_use]
    pub fn new(keys: Vec<PublicKey>) -> Self {
        Self { keys }
    }

    /// # Errors
    /// An unreadable key file.
    pub fn from_file(path: &Path) -> Result<Self> {
        Ok(Self::new(vec![crate::archive_cas_sign::read_public_key(
            path,
        )?]))
    }

    /// The one `*.dilithium2.pub` in `~/.loadngo/keys`.
    ///
    /// # Errors
    /// When there is none, or more than one (then a key must be named).
    pub fn from_default_keys() -> Result<Self> {
        match crate::archive_cas_sign::default_trusted_key()? {
            Some(key) => Ok(Self::new(vec![key])),
            None => bail!(
                "no trusted key: name one with --trusted-public-key, or keep exactly one \
                 *.dilithium2.pub in ~/.loadngo/keys"
            ),
        }
    }

    fn trusts(&self, key: &PublicKey) -> Result<bool> {
        let bytes = key.to_bytes()?;
        for trusted in &self.keys {
            if trusted.to_bytes()? == bytes {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

/// A signed claim that `branch` of `repository_id` is at `version`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedHead {
    pub format: String,
    pub repository_id: String,
    pub branch: String,
    pub version: CasHash,
    pub signer_identity: String,
    pub signed_at_unix_secs: u64,
    pub signature_scheme: String,
    pub public_key_hex: String,
    pub signature_hex: String,
}

/// The fields a head signature covers, in the order they are serialized.
#[derive(Serialize)]
struct HeadClaim<'a> {
    format: &'a str,
    repository_id: &'a str,
    branch: &'a str,
    version: String,
    signer_identity: &'a str,
    signed_at_unix_secs: u64,
}

fn head_message(
    repository_id: &str,
    branch: &str,
    version: CasHash,
    signer_identity: &str,
    signed_at_unix_secs: u64,
) -> Result<Vec<u8>> {
    let claim = HeadClaim {
        format: HEAD_FORMAT,
        repository_id,
        branch,
        version: version.to_hex(),
        signer_identity,
        signed_at_unix_secs,
    };
    let payload = serde_json::to_vec(&claim).context("failed to serialize a head claim")?;
    loadngo_anchor::anchor_frame(HEAD_DOMAIN, HEAD_DOMAIN_VERSION, &payload)
        .context("failed to frame a head claim")
}

impl SignedHead {
    /// Signs `branch` of `repository_id` at `version`.
    ///
    /// # Errors
    /// Invalid names, or a key the registry cannot sign with.
    pub fn sign(
        repository_id: &str,
        branch: &str,
        version: CasHash,
        signer: &Signer,
        signed_at_unix_secs: u64,
    ) -> Result<Self> {
        validate_id(repository_id, "repository id")?;
        validate_id(branch, "branch")?;
        let registry = default_registry();
        let scheme = registry
            .get(&signer.private.scheme)
            .ok_or_else(|| anyhow!("signature scheme not registered: {}", signer.private.scheme))?;
        let message = head_message(
            repository_id,
            branch,
            version,
            &signer.identity,
            signed_at_unix_secs,
        )?;
        let signature = scheme
            .sign(&signer.private, &message)
            .context("failed to sign the head")?;
        let head = Self {
            format: HEAD_FORMAT.to_string(),
            repository_id: repository_id.to_string(),
            branch: branch.to_string(),
            version,
            signer_identity: signer.identity.clone(),
            signed_at_unix_secs,
            signature_scheme: signer.public.scheme.to_string(),
            public_key_hex: hex::encode(signer.public.to_bytes()?),
            signature_hex: hex::encode(signature.to_bytes()?),
        };
        // Never hand out a claim that does not verify.
        head.verify(&Trust::new(vec![signer.public.clone()]))
            .context("a fresh head signature does not verify")?;
        Ok(head)
    }

    /// The version this head names, once its signature verifies and its key is trusted.
    ///
    /// # Errors
    /// An unknown format, a key not trusted, or a signature that does not match the claim.
    pub fn verify(&self, trust: &Trust) -> Result<CasHash> {
        if self.format != HEAD_FORMAT {
            bail!("unsupported head format {:?}", self.format);
        }
        let public = PublicKey::from_bytes(
            &hex::decode(self.public_key_hex.trim()).context("invalid public_key_hex")?,
        )
        .context("invalid public key in the head")?;
        if !trust.trusts(&public)? {
            bail!(
                "branch {} of {} is signed by {:?} with a key that is not trusted",
                self.branch,
                self.repository_id,
                self.signer_identity
            );
        }
        let signature = Signature::from_bytes(
            &hex::decode(self.signature_hex.trim()).context("invalid signature_hex")?,
        )
        .context("invalid signature in the head")?;
        if signature.scheme != public.scheme || self.signature_scheme != public.scheme.to_string() {
            bail!(
                "signature scheme mismatch: declared {}, key {}, signature {}",
                self.signature_scheme,
                public.scheme,
                signature.scheme
            );
        }
        let registry = default_registry();
        let scheme = registry
            .get(&public.scheme)
            .ok_or_else(|| anyhow!("signature scheme not registered: {}", public.scheme))?;
        let message = head_message(
            &self.repository_id,
            &self.branch,
            self.version,
            &self.signer_identity,
            self.signed_at_unix_secs,
        )?;
        scheme
            .verify(&public, &message, &signature)
            .map_err(|error| {
                anyhow!(
                    "the signature on branch {} of {} does not match its claim: {error}",
                    self.branch,
                    self.repository_id
                )
            })?;
        Ok(self.version)
    }
}

/// What [`Repository::verify`] checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyReport {
    pub head: CasHash,
    pub signer_identity: String,
    pub versions: usize,
    pub trees: usize,
    pub files: usize,
    pub bytes: u64,
}

/// One repository in a store.
pub struct Repository {
    store: ArchiveCasStorage,
    id: String,
    dir: PathBuf,
}

/// Whether `root` holds a store already (an Archive CAS root).
#[must_use]
pub fn is_store(root: &Path) -> bool {
    root.join("objects").is_dir()
}

impl Repository {
    /// Opens repository `id` in the store at `root`, making the store when `create` is
    /// set.
    ///
    /// # Errors
    /// An invalid id, or no store at `root` when `create` is not set.
    pub fn open(root: &Path, id: &str, create: bool) -> Result<Self> {
        validate_id(id, "repository id")?;
        if !create && !is_store(root) {
            bail!("no store at {}", root.display());
        }
        let store = ArchiveCasStorage::new(root)
            .with_context(|| format!("failed to open the store at {}", root.display()))?;
        let dir = root.join(REPOSITORIES_DIR).join(id);
        Ok(Self {
            store,
            id: id.to_string(),
            dir,
        })
    }

    #[must_use]
    pub fn store(&self) -> &ArchiveCasStorage {
        &self.store
    }

    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    fn heads_dir(&self) -> PathBuf {
        self.dir.join("heads")
    }

    fn signatures_dir(&self) -> PathBuf {
        self.dir.join("signatures")
    }

    /// An object's bytes, checked against its hash.
    fn read_object(&self, hash: CasHash, what: &str) -> Result<Vec<u8>> {
        let mut reader = self
            .store
            .open_object(hash)
            .with_context(|| format!("{what} {hash} is not in the store"))?;
        let mut bytes = Vec::new();
        reader
            .read_to_end(&mut bytes)
            .with_context(|| format!("failed to read {what} {hash}"))?;
        let actual = CasHash::digest(&bytes);
        if actual != hash {
            bail!("{what} {hash} is damaged: its bytes hash to {actual}");
        }
        Ok(bytes)
    }

    /// Stores a tree; returns its hash.
    ///
    /// # Errors
    /// An invalid tree, or a store write that fails (the free-space floor included).
    pub fn put_tree(&self, tree: &Tree) -> Result<CasHash> {
        Ok(self
            .store
            .add_content(&tree.canonical_bytes()?)?
            .object
            .hash)
    }

    /// A stored tree, checked against its hash and the tree rules.
    ///
    /// # Errors
    /// A missing, damaged, non-canonical or invalid tree.
    pub fn tree(&self, hash: CasHash) -> Result<Tree> {
        let bytes = self.read_object(hash, "tree")?;
        let tree: Tree =
            serde_json::from_slice(&bytes).with_context(|| format!("tree {hash} is not a tree"))?;
        if tree.canonical_bytes()? != bytes {
            bail!("tree {hash} is not in canonical form");
        }
        Ok(tree)
    }

    /// Stores a version; returns its hash.
    ///
    /// # Errors
    /// An invalid version, one of another repository, or a store write that fails.
    pub fn put_version(&self, version: &Version) -> Result<CasHash> {
        if version.repository_id != self.id {
            bail!(
                "version of {:?} cannot be stored in {:?}",
                version.repository_id,
                self.id
            );
        }
        Ok(self
            .store
            .add_content(&version.canonical_bytes()?)?
            .object
            .hash)
    }

    /// A stored version of this repository, checked against its hash.
    ///
    /// # Errors
    /// A missing, damaged, non-canonical or foreign version.
    pub fn version(&self, hash: CasHash) -> Result<Version> {
        let bytes = self.read_object(hash, "version")?;
        let version: Version = serde_json::from_slice(&bytes)
            .with_context(|| format!("version {hash} is not a version"))?;
        if version.canonical_bytes()? != bytes {
            bail!("version {hash} is not in canonical form");
        }
        if version.repository_id != self.id {
            bail!(
                "version {hash} belongs to {:?}, not {:?}",
                version.repository_id,
                self.id
            );
        }
        Ok(version)
    }

    /// The head of `branch` as stored, unverified; `None` when the branch has none.
    ///
    /// # Errors
    /// An unreadable head, or one that names another repository or branch.
    pub fn head(&self, branch: &str) -> Result<Option<SignedHead>> {
        validate_id(branch, "branch")?;
        let path = self.heads_dir().join(format!("{branch}.json"));
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error).with_context(|| format!("failed to read {}", path.display()))
            }
        };
        let head: SignedHead = serde_json::from_slice(&bytes)
            .with_context(|| format!("{} is not a head", path.display()))?;
        if head.repository_id != self.id || head.branch != branch {
            bail!(
                "{} names branch {} of {}",
                path.display(),
                head.branch,
                head.repository_id
            );
        }
        Ok(Some(head))
    }

    /// Every branch with a head, sorted.
    ///
    /// # Errors
    /// An unreadable heads folder.
    pub fn branches(&self) -> Result<Vec<String>> {
        let mut branches = Vec::new();
        let entries = match fs::read_dir(self.heads_dir()) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(branches),
            Err(error) => return Err(error.into()),
        };
        for entry in entries {
            let name = entry?.file_name();
            if let Some(branch) = name.to_str().and_then(|name| name.strip_suffix(".json")) {
                if validate_id(branch, "branch").is_ok() {
                    branches.push(branch.to_string());
                }
            }
        }
        branches.sort();
        Ok(branches)
    }

    /// The version `branch` is at, once its head's signature verifies against `trust`.
    ///
    /// # Errors
    /// No head, or one that does not verify.
    pub fn trusted_head(&self, branch: &str, trust: &Trust) -> Result<(CasHash, SignedHead)> {
        let head = self
            .head(branch)?
            .ok_or_else(|| anyhow!("{} has no branch {branch}", self.id))?;
        let version = head.verify(trust)?;
        Ok((version, head))
    }

    /// Moves `branch` to the version `head` names, if the branch is still at `expected`
    /// (`None`: the branch must not exist yet). The claim is also kept, for good, in
    /// `signatures/`.
    ///
    /// # Errors
    /// A head for another repository, a branch that moved meanwhile, a change already in
    /// progress (a lock file), or a failed write.
    pub fn set_head(&self, head: &SignedHead, expected: Option<CasHash>) -> Result<()> {
        if head.repository_id != self.id {
            bail!(
                "a head of {:?} cannot be set in {:?}",
                head.repository_id,
                self.id
            );
        }
        validate_id(&head.branch, "branch")?;
        validate_id(&head.signer_identity, "signer identity")?;
        let heads = self.heads_dir();
        let signatures = self.signatures_dir();
        if !heads.is_dir() || !signatures.is_dir() {
            fs::create_dir_all(&heads)?;
            fs::create_dir_all(&signatures)?;
            // New folders survive a power cut only once their parents are synced.
            for folder in [&self.dir, &self.dir.join(".."), &self.dir.join("../..")] {
                sync_dir(folder)?;
            }
        }
        let lock_path = heads.join(format!("{}.lock", head.branch));
        let _lock = HeadLock::take(&lock_path)?;
        let current = self.head(&head.branch)?.map(|head| head.version);
        if current != expected {
            bail!(
                "branch {} moved: expected {}, found {}",
                head.branch,
                expected.map_or_else(|| "no head".into(), |v| v.to_hex()),
                current.map_or_else(|| "no head".into(), |v| v.to_hex())
            );
        }
        let bytes = serde_json::to_vec_pretty(head)?;
        let claim = signatures.join(format!(
            "{}.{}.{}.{}.json",
            head.version.to_hex(),
            head.branch,
            head.signer_identity,
            head.signed_at_unix_secs
        ));
        write_file_synced(&claim, &bytes)?;
        write_file_synced(&heads.join(format!("{}.json", head.branch)), &bytes)?;
        Ok(())
    }

    /// Every head claim kept in `signatures/`, unverified.
    ///
    /// # Errors
    /// An unreadable claim.
    pub fn claims(&self) -> Result<Vec<SignedHead>> {
        let mut claims = Vec::new();
        let entries = match fs::read_dir(self.signatures_dir()) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(claims),
            Err(error) => return Err(error.into()),
        };
        for entry in entries {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let bytes = fs::read(&path)?;
            claims.push(
                serde_json::from_slice(&bytes)
                    .with_context(|| format!("{} is not a head claim", path.display()))?,
            );
        }
        Ok(claims)
    }

    /// `from` and its first parents, newest first.
    ///
    /// # Errors
    /// A missing or damaged version.
    pub fn first_parent_history(&self, from: CasHash) -> Result<Vec<(CasHash, Version)>> {
        let mut history = Vec::new();
        let mut seen = HashSet::new();
        let mut next = Some(from);
        while let Some(hash) = next {
            if !seen.insert(hash) {
                bail!("version {hash} is its own ancestor");
            }
            let version = self.version(hash)?;
            next = version.parents.first().copied();
            history.push((hash, version));
        }
        Ok(history)
    }

    /// Every version reachable from `from` through all parents, `from` first.
    ///
    /// # Errors
    /// A missing or damaged version.
    pub fn ancestry(&self, from: CasHash) -> Result<Vec<(CasHash, Version)>> {
        self.ancestry_until(from, |_| false).map(|(all, _)| all)
    }

    /// As [`Self::ancestry`], stopping early (and saying so) at the first version `stop`
    /// accepts.
    fn ancestry_until(
        &self,
        from: CasHash,
        mut stop: impl FnMut(CasHash) -> bool,
    ) -> Result<(Vec<(CasHash, Version)>, bool)> {
        let mut all = Vec::new();
        let mut seen = HashSet::new();
        let mut queue = VecDeque::from([from]);
        while let Some(hash) = queue.pop_front() {
            if !seen.insert(hash) {
                continue;
            }
            if stop(hash) {
                return Ok((all, true));
            }
            let version = self.version(hash)?;
            queue.extend(version.parents.iter().copied());
            all.push((hash, version));
        }
        Ok((all, false))
    }

    /// Whether `ancestor` is `descendant` or one of its ancestors.
    ///
    /// # Errors
    /// A missing or damaged version on the way.
    pub fn is_ancestor(&self, ancestor: CasHash, descendant: CasHash) -> Result<bool> {
        Ok(self.ancestry_until(descendant, |hash| hash == ancestor)?.1)
    }

    /// The branch whose trusted head has `version` in its history, if any.
    ///
    /// # Errors
    /// A damaged version on the way.
    pub fn trusted_branch_holding(
        &self,
        version: CasHash,
        trust: &Trust,
    ) -> Result<Option<String>> {
        for branch in self.branches()? {
            let Ok((head, _)) = self.trusted_head(&branch, trust) else {
                continue;
            };
            if self.is_ancestor(version, head)? {
                return Ok(Some(branch));
            }
        }
        Ok(None)
    }

    /// Checks `branch` from its signature down: the head verifies against `trust`; every
    /// version reachable from it, every tree below them (with its totals) and every file
    /// object hash correctly.
    ///
    /// # Errors
    /// The first failure found.
    pub fn verify(&self, branch: &str, trust: &Trust) -> Result<VerifyReport> {
        let (head, signed) = self.trusted_head(branch, trust)?;
        let versions = self.ancestry(head)?;
        let mut trees = HashMap::new();
        let mut files = BTreeMap::new();
        for (_, version) in &versions {
            self.check_tree(version.tree, &mut trees, &mut files)?;
        }
        let mut bytes = 0;
        for (&hash, &size) in &files {
            self.store
                .verify_object(ArchiveObject { hash, size })
                .with_context(|| format!("file object {hash} is missing or damaged"))?;
            bytes += size;
        }
        Ok(VerifyReport {
            head,
            signer_identity: signed.signer_identity,
            versions: versions.len(),
            trees: trees.len(),
            files: files.len(),
            bytes,
        })
    }

    /// Reads `hash` and every tree below it once, checking each directory entry's totals
    /// against the tree it names; collects the file objects. Returns the tree's totals.
    fn check_tree(
        &self,
        hash: CasHash,
        seen: &mut HashMap<CasHash, (u64, u64)>,
        files: &mut BTreeMap<CasHash, u64>,
    ) -> Result<(u64, u64)> {
        if let Some(totals) = seen.get(&hash) {
            return Ok(*totals);
        }
        let tree = self.tree(hash)?;
        for entry in &tree.entries {
            match entry {
                TreeEntry::File { object, .. } | TreeEntry::Executable { object, .. } => {
                    if let Some(size) = files.insert(object.hash, object.size) {
                        if size != object.size {
                            bail!(
                                "file object {} is listed as {size} and as {} bytes",
                                object.hash,
                                object.size
                            );
                        }
                    }
                }
                TreeEntry::Symlink { .. } => {}
                TreeEntry::Directory {
                    name,
                    tree: child,
                    files: count,
                    bytes,
                } => {
                    let actual = self.check_tree(*child, seen, files)?;
                    if actual != (*count, *bytes) {
                        bail!(
                            "tree {hash} says {name:?} holds {count} files and {bytes} bytes; it holds {} and {}",
                            actual.0,
                            actual.1
                        );
                    }
                }
            }
        }
        let totals = tree.totals();
        seen.insert(hash, totals);
        Ok(totals)
    }

    /// The changes that make tree `new` from tree `old` (`None`: nothing before), with
    /// moves found by equal content against `parent`.
    ///
    /// # Errors
    /// A missing or damaged tree.
    pub fn changes_between(
        &self,
        parent: Option<CasHash>,
        old: Option<CasHash>,
        new: CasHash,
        cache: &mut TreeCache,
    ) -> Result<Vec<ArchiveChange>> {
        let mut diff = TreeDiff::default();
        self.diff_into(old, Some(new), "", cache, &mut diff)?;
        Ok(diff.into_changes(parent))
    }

    fn diff_into(
        &self,
        old: Option<CasHash>,
        new: Option<CasHash>,
        prefix: &str,
        cache: &mut TreeCache,
        diff: &mut TreeDiff,
    ) -> Result<()> {
        if old == new {
            return Ok(());
        }
        let empty = Vec::new();
        let old_tree = old.map(|hash| cache.get(self, hash)).transpose()?;
        let new_tree = new.map(|hash| cache.get(self, hash)).transpose()?;
        let old_entries = old_tree.as_ref().map_or(&empty, |tree| &tree.entries);
        let new_entries = new_tree.as_ref().map_or(&empty, |tree| &tree.entries);
        let (mut i, mut j) = (0, 0);
        while i < old_entries.len() || j < new_entries.len() {
            let order = match (old_entries.get(i), new_entries.get(j)) {
                (Some(a), Some(b)) => a.name().as_bytes().cmp(b.name().as_bytes()),
                (Some(_), None) => std::cmp::Ordering::Less,
                _ => std::cmp::Ordering::Greater,
            };
            match order {
                std::cmp::Ordering::Less => {
                    let entry = &old_entries[i];
                    diff.deleted
                        .push((join(prefix, entry.name()), entry.identity()));
                    i += 1;
                }
                std::cmp::Ordering::Greater => {
                    let entry = &new_entries[j];
                    diff.created
                        .push((join(prefix, entry.name()), entry.identity()));
                    j += 1;
                }
                std::cmp::Ordering::Equal => {
                    let (a, b) = (&old_entries[i], &new_entries[j]);
                    let path = join(prefix, a.name());
                    match (a, b) {
                        _ if a == b => {}
                        (
                            TreeEntry::Directory {
                                tree: old_child, ..
                            },
                            TreeEntry::Directory {
                                tree: new_child, ..
                            },
                        ) => {
                            self.diff_into(Some(*old_child), Some(*new_child), &path, cache, diff)?;
                        }
                        (TreeEntry::Directory { .. }, _) | (_, TreeEntry::Directory { .. }) => {
                            diff.deleted.push((path.clone(), a.identity()));
                            diff.created.push((path, b.identity()));
                        }
                        _ => diff.changed.push(path),
                    }
                    i += 1;
                    j += 1;
                }
            }
        }
        Ok(())
    }
}

/// Trees read once and kept, for diffs over many versions (an import).
#[derive(Default)]
pub struct TreeCache {
    trees: HashMap<CasHash, std::rc::Rc<Tree>>,
}

impl TreeCache {
    fn get(&mut self, repository: &Repository, hash: CasHash) -> Result<std::rc::Rc<Tree>> {
        if let Some(tree) = self.trees.get(&hash) {
            return Ok(tree.clone());
        }
        let tree = std::rc::Rc::new(repository.tree(hash)?);
        self.trees.insert(hash, tree.clone());
        Ok(tree)
    }

    /// Keeps a tree just written, so a diff need not read it back.
    pub fn insert(&mut self, hash: CasHash, tree: Tree) {
        self.trees.insert(hash, std::rc::Rc::new(tree));
    }
}

#[derive(Default)]
struct TreeDiff {
    created: Vec<(String, (u8, String))>,
    deleted: Vec<(String, (u8, String))>,
    changed: Vec<String>,
}

impl TreeDiff {
    /// Pairs each deleted path with a created one of the same content, when each side
    /// has exactly one such path, as a move.
    fn into_changes(self, parent: Option<CasHash>) -> Vec<ArchiveChange> {
        let mut created_by: HashMap<&(u8, String), Vec<&str>> = HashMap::new();
        for (path, identity) in &self.created {
            created_by.entry(identity).or_default().push(path);
        }
        let mut deleted_by: HashMap<&(u8, String), Vec<&str>> = HashMap::new();
        for (path, identity) in &self.deleted {
            deleted_by.entry(identity).or_default().push(path);
        }
        let mut moves = Vec::new();
        let mut moved_from = HashSet::new();
        let mut moved_to = HashSet::new();
        if let Some(parent) = parent {
            for (identity, from) in &deleted_by {
                if let (&[from], Some(&[to])) =
                    (from.as_slice(), created_by.get(identity).map(Vec::as_slice))
                {
                    moves.push(ArchiveMove {
                        parent,
                        from: from.to_string(),
                        to: to.to_string(),
                    });
                    moved_from.insert(from);
                    moved_to.insert(to);
                }
            }
        }
        moves.sort_by(|a, b| a.to.cmp(&b.to));
        let mut created: Vec<String> = self
            .created
            .iter()
            .filter(|(path, _)| !moved_to.contains(path.as_str()))
            .map(|(path, _)| path.clone())
            .collect();
        let mut deleted: Vec<String> = self
            .deleted
            .iter()
            .filter(|(path, _)| !moved_from.contains(path.as_str()))
            .map(|(path, _)| path.clone())
            .collect();
        let mut changed = self.changed;
        created.sort();
        deleted.sort();
        changed.sort();
        let mut changes = Vec::new();
        if !created.is_empty() {
            changes.push(ArchiveChange::Created { paths: created });
        }
        if !changed.is_empty() {
            changes.push(ArchiveChange::Changed { paths: changed });
        }
        if !deleted.is_empty() {
            changes.push(ArchiveChange::Deleted { paths: deleted });
        }
        if !moves.is_empty() {
            changes.push(ArchiveChange::Moved { moves });
        }
        changes
    }
}

fn join(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}/{name}")
    }
}

/// Every object the repositories in `store` need: each version reachable from a head or
/// from a kept claim, every tree below them and every file object. `archive_cas_gc` and
/// `archive_cas_purge` keep these.
///
/// # Errors
/// Any version or tree that cannot be read: then nothing can be known to be unneeded.
pub fn live_objects(store: &ArchiveCasStorage) -> Result<BTreeSet<CasHash>> {
    let mut live = BTreeSet::new();
    let root = store.root().join(REPOSITORIES_DIR);
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(live),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to list {}", root.display()))
        }
    };
    for entry in entries {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            bail!(
                "{} is a symlink; refusing to work out what it needs",
                entry.path().display()
            );
        }
        if !kind.is_dir() {
            continue;
        }
        let Some(id) = entry.file_name().to_str().map(str::to_string) else {
            bail!("{} is not a repository name", entry.path().display());
        };
        let repository = Repository::open(store.root(), &id, false)
            .with_context(|| format!("repository {id}"))?;
        let mut starts: Vec<CasHash> = repository
            .claims()?
            .iter()
            .map(|claim| claim.version)
            .collect();
        for branch in repository.branches()? {
            if let Some(head) = repository.head(&branch)? {
                starts.push(head.version);
            }
        }
        let mut trees = HashMap::new();
        let mut files = BTreeMap::new();
        // Per repository: `live` also holds other repositories' objects, and one of
        // those may have the same bytes as a version here.
        let mut walked = HashSet::new();
        for start in starts {
            if walked.contains(&start) {
                continue;
            }
            for (hash, version) in repository
                .ancestry(start)
                .with_context(|| format!("repository {id}: the history of {start}"))?
            {
                walked.insert(hash);
                live.insert(hash);
                repository
                    .check_tree(version.tree, &mut trees, &mut files)
                    .with_context(|| format!("repository {id}: the tree of version {hash}"))?;
            }
        }
        live.extend(trees.keys().copied());
        live.extend(files.keys().copied());
    }
    Ok(live)
}

/// Holds `<branch>.lock` while a head changes.
struct HeadLock(PathBuf);

impl HeadLock {
    fn take(path: &Path) -> Result<Self> {
        match OpenOptions::new().write(true).create_new(true).open(path) {
            Ok(mut file) => {
                let _ = writeln!(file, "{}", std::process::id());
                Ok(Self(path.to_path_buf()))
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => bail!(
                "{} exists: another change to this branch is running, or one stopped part way \
                 (remove the file if nothing is running)",
                path.display()
            ),
            Err(error) => {
                Err(error).with_context(|| format!("failed to create {}", path.display()))
            }
        }
    }
}

impl Drop for HeadLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// Writes `bytes` to `path` through a synced temporary sibling, replacing what is there.
fn write_file_synced(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("{} has no parent", path.display()))?;
    // A unique name, created exclusively: a tree cannot plant a symlink at it to have
    // this write land outside the folder (create_new refuses an existing path,
    // symlinks included).
    let temporary = parent.join(format!(
        ".{}.{}-{}.partial",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("file"),
        std::process::id(),
        unique_suffix()
    ));
    {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .with_context(|| format!("failed to create {}", temporary.display()))?;
        let written = file.write_all(bytes).and_then(|()| file.sync_all());
        if let Err(error) = written {
            let _ = fs::remove_file(&temporary);
            return Err(error).with_context(|| format!("failed to write {}", temporary.display()));
        }
    }
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(error).with_context(|| format!("failed to write {}", path.display()));
    }
    sync_dir(parent)
}

/// A suffix no other call in this process returns.
fn unique_suffix() -> u128 {
    static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    (nanos << 32) | u128::from(COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
}

#[cfg(unix)]
fn sync_dir(dir: &Path) -> Result<()> {
    File::open(dir)
        .and_then(|dir| dir.sync_all())
        .with_context(|| format!("failed to synchronize {}", dir.display()))
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> Result<()> {
    Ok(())
}

/// Seconds since the Unix epoch, now.
#[must_use]
pub fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

/// `unix` as `YYYY-MM-DDTHH:MM:SSZ`.
#[must_use]
pub fn format_utc(unix: u64) -> String {
    let days = i64::try_from(unix / 86_400).unwrap_or(i64::MAX);
    let seconds = unix % 86_400;
    // Days to a civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )
}

/// The store a command uses when none is named: `$LOADNGO_REPO_STORE`, else
/// `~/.loadngo/store`.
///
/// # Errors
/// When neither is known.
pub fn default_store() -> Result<PathBuf> {
    if let Some(store) = std::env::var_os("LOADNGO_REPO_STORE") {
        return Ok(PathBuf::from(store));
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .ok_or_else(|| anyhow!("no home folder: name a store with --store"))?;
    Ok(Path::new(&home).join(".loadngo").join("store"))
}

/// Paths below a version's tree, flattened: what each file path holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Leaf {
    File(ArchiveObject),
    Executable(ArchiveObject),
    Symlink(String),
}

impl Repository {
    /// Every file and symlink below tree `hash`, by path.
    ///
    /// # Errors
    /// A missing or damaged tree.
    pub fn leaves(&self, hash: CasHash) -> Result<BTreeMap<String, Leaf>> {
        let mut leaves = BTreeMap::new();
        let mut stack = vec![(String::new(), hash)];
        while let Some((prefix, hash)) = stack.pop() {
            for entry in self.tree(hash)?.entries {
                match entry {
                    TreeEntry::File { name, object } => {
                        leaves.insert(join(&prefix, &name), Leaf::File(object));
                    }
                    TreeEntry::Executable { name, object } => {
                        leaves.insert(join(&prefix, &name), Leaf::Executable(object));
                    }
                    TreeEntry::Symlink { name, target } => {
                        leaves.insert(join(&prefix, &name), Leaf::Symlink(target));
                    }
                    TreeEntry::Directory { name, tree, .. } => {
                        stack.push((join(&prefix, &name), tree));
                    }
                }
            }
        }
        Ok(leaves)
    }

    /// Writes the trees that hold `leaves` and returns the root's hash. Folders with
    /// nothing in them are not kept, as git keeps none.
    ///
    /// # Errors
    /// An invalid path or a failed store write.
    pub fn put_leaves(
        &self,
        leaves: &BTreeMap<String, Leaf>,
        cache: &mut TreeCache,
    ) -> Result<CasHash> {
        #[derive(Default)]
        struct Folder {
            leaves: Vec<(String, Leaf)>,
            folders: BTreeMap<String, Folder>,
        }
        let mut root = Folder::default();
        for (path, leaf) in leaves {
            let mut folder = &mut root;
            let mut parts: Vec<&str> = path.split('/').collect();
            let name = parts.pop().unwrap_or_default();
            for part in parts {
                folder = folder.folders.entry(part.to_string()).or_default();
            }
            folder.leaves.push((name.to_string(), leaf.clone()));
        }
        fn put(
            repository: &Repository,
            folder: Folder,
            cache: &mut TreeCache,
        ) -> Result<(CasHash, u64, u64)> {
            let mut entries = Vec::new();
            for (name, leaf) in folder.leaves {
                entries.push(match leaf {
                    Leaf::File(object) => TreeEntry::File { name, object },
                    Leaf::Executable(object) => TreeEntry::Executable { name, object },
                    Leaf::Symlink(target) => TreeEntry::Symlink { name, target },
                });
            }
            for (name, child) in folder.folders {
                let (tree, files, bytes) = put(repository, child, cache)?;
                entries.push(TreeEntry::Directory {
                    name,
                    tree,
                    files,
                    bytes,
                });
            }
            let tree = Tree::new(entries)?;
            let (files, bytes) = tree.totals();
            let hash = repository.put_tree(&tree)?;
            cache.insert(hash, tree);
            Ok((hash, files, bytes))
        }
        Ok(put(self, root, cache)?.0)
    }
}
