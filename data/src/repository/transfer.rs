//! Sending a branch from one store to another, on this machine or over ssh.
//!
//! The sender asks the receiving store which heads it has (`loadngo-repo heads`), works
//! out the versions, trees and files the receiver lacks, and writes them as one stream
//! to `loadngo-repo receive`, ending with the signed head. The receiver hashes every
//! object as it stores it, verifies the head's signature against its own trusted key,
//! checks that everything the head needs is present and intact, and moves its branch
//! only forward (the old head must be in the new head's history).
//!
//! Stream: the line `loadngo-repository-stream-v1`, then frames: `O` + 32-byte hash +
//! u64 little-endian length + bytes (an object), `H` + u64 length + JSON (the signed
//! head), `E` (end).
//!
//! Over ssh, the key exchange is pinned to the post-quantum hybrids
//! (`mlkem768x25519-sha256`, `sntrup761x25519-sha512@openssh.com`), so the stream is
//! not readable later by breaking an elliptic-curve exchange. The receiver's checks do
//! not depend on the transport.

use super::{Repository, SignedHead, TreeEntry, Trust};
use crate::cas::CasHash;
use anyhow::{anyhow, bail, Context, Result};
use std::collections::HashSet;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

pub const STREAM_MAGIC: &[u8] = b"loadngo-repository-stream-v1\n";
/// Key exchanges `send --to-ssh` allows: hybrid post-quantum ones only.
pub const SSH_KEX: &str = "mlkem768x25519-sha256,sntrup761x25519-sha512@openssh.com";
const LARGEST_HEAD: u64 = 1 << 20;

/// How to run `loadngo-repo` where the receiving store is.
pub enum Remote {
    /// This machine: run this program.
    Local(OsString),
    /// Another machine: `ssh <host> <program>`.
    Ssh { host: String, program: String },
}

impl Remote {
    fn command(&self, args: &[String]) -> Command {
        match self {
            Self::Local(program) => {
                let mut command = Command::new(program);
                command.args(args);
                command
            }
            Self::Ssh { host, program } => {
                let mut command = Command::new("ssh");
                command
                    .args(["-o", "BatchMode=yes", "-o"])
                    .arg(format!("KexAlgorithms={SSH_KEX}"))
                    .arg(host)
                    .arg("--")
                    .arg(shell_quote(program));
                for arg in args {
                    command.arg(shell_quote(arg));
                }
                command
            }
        }
    }
}

/// Quotes `text` for the remote shell ssh runs the command in.
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// What [`send`] sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendReport {
    pub head: CasHash,
    pub versions: usize,
    pub objects: usize,
    pub bytes: u64,
    /// What the receiver printed.
    pub receiver_said: String,
}

/// Sends `branch` of `repository` to the store `remote_store` reached through `remote`.
/// `remote_trust` names, on the receiving side, the public key it should trust (else
/// it uses its own default).
///
/// # Errors
/// An untrusted local head, a receiver whose branch has versions this store lacks, a
/// failed command, or a receiver that refuses.
pub fn send(
    repository: &Repository,
    branch: &str,
    trust: &Trust,
    remote: &Remote,
    remote_store: &str,
    remote_trust: Option<&str>,
    everything: bool,
) -> Result<SendReport> {
    let (head, signed) = repository.trusted_head(branch, trust)?;
    let listing = run(
        remote,
        &[
            "heads".into(),
            "--store".into(),
            remote_store.into(),
            "--repository".into(),
            repository.id().into(),
        ],
        None,
    )?;
    let mut known_versions = Vec::new();
    for line in listing.lines() {
        let Some((their_branch, version)) = line.split_once('\t') else {
            continue;
        };
        let version: CasHash = version
            .trim()
            .parse()
            .with_context(|| format!("the receiver listed {line:?}"))?;
        let known = repository.is_known(version);
        if their_branch == branch {
            if !known {
                bail!(
                    "the receiving store's {branch} is at {version}, which this store does not have; \
                     it holds versions this one lacks"
                );
            }
            if !repository.is_ancestor(version, head)? {
                bail!(
                    "the receiving store's {branch} ({version}) is not in the history of {head}; \
                     it would not move forward"
                );
            }
        }
        if known {
            known_versions.push(version);
        }
    }
    // `everything`: send every object, so a receiver holding damaged copies repairs them.
    if everything {
        known_versions.clear();
    }
    let plan = repository.plan_send(head, &known_versions)?;
    let mut args = vec!["receive".to_string(), "--store".into(), remote_store.into()];
    if let Some(key) = remote_trust {
        args.push("--trusted-public-key".into());
        args.push(key.into());
    }
    let mut counts = (0, 0);
    let receiver_said = run(
        remote,
        &args,
        Some(&mut |out: &mut dyn Write| {
            counts = repository.write_stream(&plan, &signed, out)?;
            Ok(())
        }),
    )?;
    Ok(SendReport {
        head,
        versions: plan.versions.len(),
        objects: counts.0,
        bytes: counts.1,
        receiver_said,
    })
}

type Feed<'a> = &'a mut dyn FnMut(&mut dyn Write) -> Result<()>;

/// Runs `loadngo-repo <args>` through `remote`, letting `feed` write its input; returns
/// what it printed.
fn run(remote: &Remote, args: &[String], feed: Option<Feed<'_>>) -> Result<String> {
    let mut command = remote.command(args);
    command
        .stdin(if feed.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .with_context(|| format!("failed to run {:?}", command.get_program()))?;
    if let Some(feed) = feed {
        let stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;
        let mut writer = std::io::BufWriter::with_capacity(1 << 20, stdin);
        let written = feed(&mut writer).and_then(|()| writer.flush().map_err(Into::into));
        drop(writer);
        if let Err(error) = written {
            let output = child.wait_with_output()?;
            bail!(
                "the receiver stopped reading ({error:#}): {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        bail!(
            "`{} {}` failed ({}): {}",
            command.get_program().to_string_lossy(),
            args.first().map_or("", String::as_str),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The objects a send writes, in order: file objects, trees, versions.
pub struct SendPlan {
    pub files: Vec<crate::archive_cas::ArchiveObject>,
    pub trees: Vec<CasHash>,
    pub versions: Vec<CasHash>,
}

impl Repository {
    fn is_known(&self, version: CasHash) -> bool {
        self.store().has_object(version) && self.version(version).is_ok()
    }

    /// What a receiver that holds `known` (and their histories) lacks of `head`.
    ///
    /// # Errors
    /// A missing or damaged version or tree.
    pub fn plan_send(&self, head: CasHash, known: &[CasHash]) -> Result<SendPlan> {
        let mut known_versions = HashSet::new();
        let mut known_trees = HashSet::new();
        let mut known_files = HashSet::new();
        for start in known {
            for (hash, version) in self.ancestry(*start)? {
                known_versions.insert(hash);
                self.collect(
                    version.tree,
                    &mut known_trees,
                    &mut known_files,
                    &mut Vec::new(),
                    &mut Vec::new(),
                )?;
            }
        }
        let mut plan = SendPlan {
            files: Vec::new(),
            trees: Vec::new(),
            versions: Vec::new(),
        };
        // Every version the receiver lacks: a known version ends that line of the walk,
        // not the walk (the other side of a merge may still be unknown).
        let mut versions = Vec::new();
        let mut seen = HashSet::new();
        let mut queue = std::collections::VecDeque::from([head]);
        while let Some(hash) = queue.pop_front() {
            if known_versions.contains(&hash) || !seen.insert(hash) {
                continue;
            }
            let version = self.version(hash)?;
            queue.extend(version.parents.iter().copied());
            versions.push((hash, version));
        }
        for (hash, version) in versions.iter().rev() {
            self.collect(
                version.tree,
                &mut known_trees,
                &mut known_files,
                &mut plan.trees,
                &mut plan.files,
            )?;
            plan.versions.push(*hash);
        }
        Ok(plan)
    }

    /// Adds `tree` and what is below it to `trees` and `files`, skipping what `seen_*`
    /// already hold (and marking it seen). Trees come out children first.
    fn collect(
        &self,
        tree: CasHash,
        seen_trees: &mut HashSet<CasHash>,
        seen_files: &mut HashSet<CasHash>,
        trees: &mut Vec<CasHash>,
        files: &mut Vec<crate::archive_cas::ArchiveObject>,
    ) -> Result<()> {
        if !seen_trees.insert(tree) {
            return Ok(());
        }
        for entry in self.tree(tree)?.entries {
            match entry {
                TreeEntry::File { object, .. } | TreeEntry::Executable { object, .. } => {
                    if seen_files.insert(object.hash) {
                        files.push(object);
                    }
                }
                TreeEntry::Symlink { .. } => {}
                TreeEntry::Directory { tree: child, .. } => {
                    self.collect(child, seen_trees, seen_files, trees, files)?;
                }
            }
        }
        trees.push(tree);
        Ok(())
    }

    /// Writes `plan` and `head` as a stream; returns the objects and bytes written.
    ///
    /// # Errors
    /// A missing object or a failed write.
    pub fn write_stream(
        &self,
        plan: &SendPlan,
        head: &SignedHead,
        out: &mut dyn Write,
    ) -> Result<(usize, u64)> {
        out.write_all(STREAM_MAGIC)?;
        let mut objects = 0;
        let mut bytes = 0;
        let hashes = plan
            .files
            .iter()
            .map(|object| object.hash)
            .chain(plan.trees.iter().copied())
            .chain(plan.versions.iter().copied());
        let mut buffer = Vec::new();
        for hash in hashes {
            buffer.clear();
            self.store()
                .open_object(hash)
                .with_context(|| format!("object {hash} is not in the store"))?
                .read_to_end(&mut buffer)?;
            if CasHash::digest(&buffer) != hash {
                bail!("object {hash} is damaged; not sending it");
            }
            out.write_all(b"O")?;
            out.write_all(hash.as_bytes())?;
            out.write_all(&(buffer.len() as u64).to_le_bytes())?;
            out.write_all(&buffer)?;
            objects += 1;
            bytes += buffer.len() as u64;
        }
        let head = serde_json::to_vec(head)?;
        out.write_all(b"H")?;
        out.write_all(&(head.len() as u64).to_le_bytes())?;
        out.write_all(&head)?;
        out.write_all(b"E")?;
        out.flush()?;
        Ok((objects, bytes))
    }
}

/// What [`receive`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiveReport {
    pub repository_id: String,
    pub branch: String,
    pub head: CasHash,
    pub objects: usize,
    pub moved: bool,
}

/// Reads a stream into the store at `root`: every object hash-checked as it is stored,
/// then the head verified against `trust`, everything below it checked, and the branch
/// moved forward to it.
///
/// # Errors
/// A malformed stream, an object whose bytes do not match its hash, an untrusted or
/// invalid head, a missing or damaged object below it, or a branch that would not move
/// forward.
pub fn receive(root: &Path, input: &mut impl Read, trust: &Trust) -> Result<ReceiveReport> {
    let mut input = BufReader::new(input);
    let mut magic = vec![0_u8; STREAM_MAGIC.len()];
    input
        .read_exact(&mut magic)
        .context("the stream is empty or cut short")?;
    if magic != STREAM_MAGIC {
        bail!("not a repository stream");
    }
    let store = crate::archive_cas::ArchiveCasStorage::with_buffer_size(root, 1 << 20)
        .with_context(|| format!("failed to open the store at {}", root.display()))?;
    let mut objects = 0;
    let mut head = None;
    let mut buffer = vec![0_u8; 1 << 20];
    loop {
        let mut tag = [0_u8; 1];
        input
            .read_exact(&mut tag)
            .context("the stream is cut short")?;
        match tag[0] {
            b'O' => {
                let mut hash = [0_u8; 32];
                input.read_exact(&mut hash)?;
                let hash = CasHash::from_bytes(hash);
                let length = read_length(&mut input)?;
                crate::disk::ensure_room(root, length, crate::disk::floor_from_env())?;
                receive_object(&store, hash, length, &mut input, &mut buffer)?;
                objects += 1;
            }
            b'H' => {
                let length = read_length(&mut input)?;
                if length > LARGEST_HEAD {
                    bail!("the head is {length} bytes; not a head");
                }
                let mut bytes = vec![0_u8; usize::try_from(length)?];
                input.read_exact(&mut bytes)?;
                head = Some(
                    serde_json::from_slice::<SignedHead>(&bytes)
                        .context("the head is not a head")?,
                );
            }
            b'E' => break,
            other => bail!("unknown frame {other:#04x} in the stream"),
        }
    }
    if !input.fill_buf()?.is_empty() {
        bail!("bytes after the end of the stream");
    }
    let head = head.ok_or_else(|| anyhow!("the stream holds no head"))?;
    let version = head.verify(trust)?;
    let repository = Repository::open(root, &head.repository_id, false)?;
    let current = repository
        .head(&head.branch)?
        .map(|current| current.version);
    if current == Some(version) {
        // Nothing to move, but say so only if what the branch names is intact here.
        repository.check_closure(version).with_context(|| {
            format!(
                "branch {} is already at {version}, but this store's copy is damaged; \
                 send again with --all to repair it",
                head.branch
            )
        })?;
        return Ok(ReceiveReport {
            repository_id: head.repository_id,
            branch: head.branch,
            head: version,
            objects,
            moved: false,
        });
    }
    // Everything the head needs must be here and intact before the branch names it.
    repository.check_closure(version)?;
    if let Some(current) = current {
        if !repository.is_ancestor(current, version)? {
            bail!(
                "branch {} is at {current}, which is not in the history of {version}; refusing to move it back or sideways",
                head.branch
            );
        }
    }
    repository.set_head(&head, current)?;
    Ok(ReceiveReport {
        repository_id: head.repository_id,
        branch: head.branch,
        head: version,
        objects,
        moved: true,
    })
}

/// Stores one object frame. The bytes go to a partial file first and must hash to
/// `hash`; only then is the store touched. A copy already stored is kept when it reads
/// back intact; otherwise it is moved aside, the proven bytes stored, and the old copy
/// put back if storing fails. So a stream can only ever put back the one correct
/// content, never remove it. `buffer` is reused across frames.
fn receive_object(
    store: &crate::archive_cas::ArchiveCasStorage,
    hash: CasHash,
    length: u64,
    input: &mut impl Read,
    buffer: &mut [u8],
) -> Result<()> {
    let partial = store.scratch_path("receive");
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&partial)
            .with_context(|| format!("failed to create {}", partial.display()))?;
        let mut hasher = blake3::Hasher::new();
        let mut body = input.take(length);
        let mut copied = 0_u64;
        loop {
            let read = body.read(buffer)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
            file.write_all(&buffer[..read])?;
            copied += read as u64;
        }
        if copied != length {
            bail!("object {hash} is cut short");
        }
        let actual = CasHash::from_bytes(*hasher.finalize().as_bytes());
        if actual != hash {
            bail!("object {hash} arrived as bytes hashing to {actual}");
        }
        file.sync_all()?;
        drop(file);
        if store.has_object(hash) && stored_copy_is_intact(store, hash, buffer) {
            return Ok(());
        }
        // Move whatever copy is stored aside (both forms, raw and compressed), so that
        // if storing the proven bytes fails it can be put back as it was.
        let mut aside = Vec::new();
        for path in [store.object_path(hash), store.compressed_object_path(hash)] {
            if path.exists() {
                let moved = store.scratch_path("damaged");
                fs::rename(&path, &moved)
                    .with_context(|| format!("failed to move {} aside", path.display()))?;
                aside.push((path, moved));
            }
        }
        let stored = store
            .add_stream(&mut File::open(&partial)?)
            .with_context(|| format!("failed to store {hash}"));
        match stored {
            Ok(stored) if stored.object.hash == hash => {
                for (_, moved) in aside {
                    let _ = fs::remove_file(moved);
                }
                Ok(())
            }
            other => {
                for (path, moved) in aside {
                    let _ = fs::rename(moved, path);
                }
                other.and_then(|_| bail!("object {hash} changed while it was stored"))
            }
        }
    })();
    let _ = fs::remove_file(&partial);
    result
}

/// Whether the stored copy of `hash` reads back hashing to it.
fn stored_copy_is_intact(
    store: &crate::archive_cas::ArchiveCasStorage,
    hash: CasHash,
    buffer: &mut [u8],
) -> bool {
    let Ok(mut reader) = store.open_object(hash) else {
        return false;
    };
    let mut hasher = blake3::Hasher::new();
    loop {
        match reader.read(buffer) {
            Ok(0) => break,
            Ok(read) => {
                hasher.update(&buffer[..read]);
            }
            Err(_) => return false,
        }
    }
    CasHash::from_bytes(*hasher.finalize().as_bytes()) == hash
}

fn read_length(input: &mut impl Read) -> Result<u64> {
    let mut length = [0_u8; 8];
    input.read_exact(&mut length)?;
    Ok(u64::from_le_bytes(length))
}

impl Repository {
    /// Checks that `version`, its whole history and everything below them are present
    /// and hash correctly.
    ///
    /// # Errors
    /// The first object missing or damaged.
    pub fn check_closure(&self, version: CasHash) -> Result<()> {
        let mut trees = std::collections::HashMap::new();
        let mut files = std::collections::BTreeMap::new();
        for (_, version) in self.ancestry(version)? {
            self.check_tree(version.tree, &mut trees, &mut files)?;
        }
        for (&hash, &size) in &files {
            self.store()
                .verify_object(crate::archive_cas::ArchiveObject { hash, size })
                .with_context(|| format!("file object {hash} is missing or damaged"))?;
        }
        Ok(())
    }
}

/// `heads` output: one `<branch>\t<version>` line per branch of `id` in the store at
/// `root`; nothing when there is no such store or repository.
///
/// # Errors
/// An unreadable head.
pub fn list_heads(root: &Path, id: &str) -> Result<String> {
    super::validate_id(id, "repository id")?;
    let mut text = String::new();
    if !super::is_store(root) {
        return Ok(text);
    }
    let repository = Repository::open(root, id, false)?;
    for branch in repository.branches()? {
        if let Some(head) = repository.head(&branch)? {
            text.push_str(&format!("{branch}\t{}\n", head.version));
        }
    }
    Ok(text)
}
