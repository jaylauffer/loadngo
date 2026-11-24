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

use super::runtime::RepositoryRuntime;
use super::{Repository, SignedHead, TreeEntry, Trust};
use crate::archive_cas::worker::ArchiveCasWorkerPool;
use crate::cas::CasHash;
use anyhow::{anyhow, bail, Context, Result};
use loadngo_proactor::{PlatformPort, Proactor, ProactorHandle};
use std::collections::HashSet;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

pub const STREAM_MAGIC: &[u8] = b"loadngo-repository-stream-v1\n";
/// Key exchanges `send --to-ssh` allows: hybrid post-quantum ones only.
pub const SSH_KEX: &str = "mlkem768x25519-sha256,sntrup761x25519-sha512@openssh.com";
const LARGEST_HEAD: u64 = 1 << 20;
const STORED_COPY_VERIFY_BUFFER_BYTES: usize = 256 * 1024;
const STREAM_WRITE_BUFFER_BYTES: usize = 256 * 1024;

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
    send_internal(
        repository,
        branch,
        trust,
        remote,
        remote_store,
        remote_trust,
        everything,
        None,
    )
}

/// Like [`send`], streaming the planned CAS objects through the command proactor.
#[allow(clippy::too_many_arguments)]
pub fn send_with_runtime(
    repository: &Repository,
    branch: &str,
    trust: &Trust,
    remote: &Remote,
    remote_store: &str,
    remote_trust: Option<&str>,
    everything: bool,
    runtime: &mut RepositoryRuntime,
) -> Result<SendReport> {
    send_internal(
        repository,
        branch,
        trust,
        remote,
        remote_store,
        remote_trust,
        everything,
        Some(runtime),
    )
}

/// Writes a signed branch update to `out`, without receiver preflight. Known tips
/// are hints only; the receiver must verify the complete closure before advancing.
pub fn send_stream_with_runtime<W: Write + Send + 'static>(
    repository: &Repository,
    branch: &str,
    trust: &Trust,
    known: &[CasHash],
    out: W,
    runtime: &mut RepositoryRuntime,
) -> Result<SendReport> {
    let (head, signed) = repository.trusted_head_with_runtime(branch, trust, runtime)?;
    let mut known_versions = Vec::new();
    for version in known {
        if repository.is_known_with_runtime(*version, runtime)? {
            known_versions.push(*version);
        }
    }
    let plan = repository.plan_send_with_runtime(head, &known_versions, runtime)?;
    let versions = plan.versions.len();
    let (objects, bytes) = repository.write_stream_with_runtime(&plan, &signed, out, runtime)?;
    Ok(SendReport {
        head,
        versions,
        objects,
        bytes,
        receiver_said: String::new(),
    })
}

#[allow(clippy::too_many_arguments)]
fn send_internal(
    repository: &Repository,
    branch: &str,
    trust: &Trust,
    remote: &Remote,
    remote_store: &str,
    remote_trust: Option<&str>,
    everything: bool,
    mut runtime: Option<&mut RepositoryRuntime>,
) -> Result<SendReport> {
    let (head, signed) = if let Some(runtime) = runtime.as_deref_mut() {
        repository.trusted_head_with_runtime(branch, trust, runtime)?
    } else {
        repository.trusted_head(branch, trust)?
    };
    let head_args = [
        "heads".into(),
        "--store".into(),
        remote_store.into(),
        "--repository".into(),
        repository.id().into(),
    ];
    let listing = run(remote, &head_args, None, runtime.as_deref_mut())?;
    let listed_heads = if let Some(runtime) = runtime.as_deref_mut() {
        let task = runtime.try_submit_cpu(move || parse_heads_listing(&listing))?;
        runtime.wait_cpu(task)??
    } else {
        parse_heads_listing(&listing)?
    };
    let mut known_versions = Vec::new();
    for (their_branch, version) in listed_heads {
        let known = if let Some(runtime) = runtime.as_deref_mut() {
            repository.is_known_with_runtime(version, runtime)?
        } else {
            repository.is_known(version)
        };
        if their_branch == branch {
            if !known {
                bail!(
                    "the receiving store's {branch} is at {version}, which this store does not have; \
                     it holds versions this one lacks"
                );
            }
            let is_ancestor = if let Some(runtime) = runtime.as_deref_mut() {
                repository.is_ancestor_with_runtime(version, head, runtime)?
            } else {
                repository.is_ancestor(version, head)?
            };
            if !is_ancestor {
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
    let plan = if let Some(runtime) = runtime.as_deref_mut() {
        repository.plan_send_with_runtime(head, &known_versions, runtime)?
    } else {
        repository.plan_send(head, &known_versions)?
    };
    let mut args = vec!["receive".to_string(), "--store".into(), remote_store.into()];
    if let Some(key) = remote_trust {
        args.push("--trusted-public-key".into());
        args.push(key.into());
    }
    let mut counts = (0, 0);
    let feed: Feed<'_> = Box::new(|out, runtime| {
        counts = if let Some(runtime) = runtime {
            repository.write_stream_with_runtime(&plan, &signed, out, runtime)?
        } else {
            let mut out = out;
            repository.write_stream(&plan, &signed, &mut out)?
        };
        Ok(())
    });
    let receiver_said = run(remote, &args, Some(feed), runtime)?;
    Ok(SendReport {
        head,
        versions: plan.versions.len(),
        objects: counts.0,
        bytes: counts.1,
        receiver_said,
    })
}

type Feed<'a> =
    Box<dyn FnOnce(Box<dyn Write + Send>, Option<&mut RepositoryRuntime>) -> Result<()> + 'a>;

struct BlockingIoWriter<'a, W> {
    writer: Option<W>,
    buffer: Vec<u8>,
    proactor: &'a Proactor<PlatformPort>,
    handle: &'a ProactorHandle<PlatformPort>,
    workers: &'a mut ArchiveCasWorkerPool,
}

impl<'a, W: Write + Send + 'static> BlockingIoWriter<'a, W> {
    fn new(
        writer: W,
        proactor: &'a Proactor<PlatformPort>,
        handle: &'a ProactorHandle<PlatformPort>,
        workers: &'a mut ArchiveCasWorkerPool,
    ) -> Self {
        Self {
            writer: Some(writer),
            buffer: Vec::with_capacity(STREAM_WRITE_BUFFER_BYTES),
            proactor,
            handle,
            workers,
        }
    }

    fn run_io<T, F>(&mut self, work: F) -> io::Result<T>
    where
        T: Send + 'static,
        F: FnOnce(W) -> (W, io::Result<T>) + Send + 'static,
    {
        let writer = self
            .writer
            .take()
            .ok_or_else(|| io::Error::other("stream writer is already in use"))?;
        let mut task = self
            .workers
            .try_submit(self.handle, move || work(writer))
            .map_err(io::Error::other)?;
        let (writer, result) = loop {
            if let Some(result) = task.try_take() {
                break result.map_err(io::Error::other)?;
            }
            self.proactor.run_once()?;
        };
        self.writer = Some(writer);
        result
    }
}

impl<W: Write + Send + 'static> Write for BlockingIoWriter<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        let count = bytes.len().min(STREAM_WRITE_BUFFER_BYTES);
        let mut buffer = std::mem::take(&mut self.buffer);
        buffer.clear();
        buffer.extend_from_slice(&bytes[..count]);
        let buffer = self.run_io(move |mut writer| {
            let result = writer.write_all(&buffer);
            match result {
                Ok(()) => (writer, Ok(buffer)),
                Err(error) => (writer, Err(error)),
            }
        })?;
        self.buffer = buffer;
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.run_io(|mut writer| {
            let result = writer.flush();
            (writer, result)
        })
    }
}

/// Runs `loadngo-repo <args>` through `remote`, letting `feed` write its input; returns
/// what it printed.
fn run(
    remote: &Remote,
    args: &[String],
    feed: Option<Feed<'_>>,
    mut runtime: Option<&mut RepositoryRuntime>,
) -> Result<String> {
    let mut command = remote.command(args);
    let program = command.get_program().to_os_string();
    command
        .stdin(if feed.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let spawned = if let Some(runtime) = runtime.as_deref_mut() {
        runtime.admit_optional_work()?;
        runtime.run_blocking_io(move || command.spawn())?
    } else {
        command.spawn()
    };
    let mut child = spawned.with_context(|| format!("failed to run {program:?}"))?;
    if let Some(feed) = feed {
        let stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;
        let writer: Box<dyn Write + Send> =
            Box::new(std::io::BufWriter::with_capacity(1 << 20, stdin));
        let written = feed(writer, runtime.as_deref_mut());
        if let Err(error) = written {
            let output = wait_child_with_runtime(child, runtime.as_deref_mut())?;
            bail!(
                "the receiver stopped reading ({error:#}): {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
    }
    let output = wait_child_with_runtime(child, runtime)?;
    if !output.status.success() {
        bail!(
            "`{} {}` failed ({}): {}",
            program.to_string_lossy(),
            args.first().map_or("", String::as_str),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn wait_child_with_runtime(
    child: std::process::Child,
    runtime: Option<&mut RepositoryRuntime>,
) -> Result<std::process::Output> {
    if let Some(runtime) = runtime {
        runtime.admit_optional_work()?;
        Ok(runtime.run_blocking_io(move || child.wait_with_output())??)
    } else {
        child.wait_with_output().map_err(Into::into)
    }
}

fn parse_heads_listing(listing: &str) -> Result<Vec<(String, CasHash)>> {
    let mut heads = Vec::new();
    for line in listing.lines() {
        let Some((branch, version)) = line.split_once('\t') else {
            continue;
        };
        let version = version
            .trim()
            .parse()
            .with_context(|| format!("the receiver listed {line:?}"))?;
        heads.push((branch.to_string(), version));
    }
    Ok(heads)
}

/// The objects a send writes, in order: file objects, trees, versions.
pub struct SendPlan {
    pub files: Vec<crate::archive_cas::ArchiveObject>,
    pub trees: Vec<crate::archive_cas::ArchiveObject>,
    pub versions: Vec<crate::archive_cas::ArchiveObject>,
}

impl Repository {
    fn is_known(&self, version: CasHash) -> bool {
        self.store().has_object(version) && self.version(version).is_ok()
    }

    fn is_known_with_runtime(
        &self,
        version: CasHash,
        runtime: &mut RepositoryRuntime,
    ) -> Result<bool> {
        runtime.admit_optional_work()?;
        let raw = self.store().object_path(version);
        let compressed = self.store().compressed_object_path(version);
        let exists = runtime.run_blocking_io(move || raw.exists() || compressed.exists())?;
        if !exists {
            return Ok(false);
        }
        self.version_with_runtime(version, runtime)?;
        Ok(true)
    }

    /// What a receiver that holds `known` (and their histories) lacks of `head`.
    ///
    /// # Errors
    /// A missing or damaged version or tree.
    pub fn plan_send(&self, head: CasHash, known: &[CasHash]) -> Result<SendPlan> {
        self.plan_send_internal(head, known, None)
    }

    /// Like [`Self::plan_send`], reading version, tree, and manifest objects through runtime I/O.
    pub fn plan_send_with_runtime(
        &self,
        head: CasHash,
        known: &[CasHash],
        runtime: &mut RepositoryRuntime,
    ) -> Result<SendPlan> {
        self.plan_send_internal(head, known, Some(runtime))
    }

    fn plan_send_internal(
        &self,
        head: CasHash,
        known: &[CasHash],
        mut runtime: Option<&mut RepositoryRuntime>,
    ) -> Result<SendPlan> {
        let mut known_versions = HashSet::new();
        let mut known_trees = HashSet::new();
        let mut known_files = HashSet::new();
        for start in known {
            let ancestry = if let Some(runtime) = runtime.as_deref_mut() {
                self.ancestry_with_runtime(*start, runtime)?
            } else {
                self.ancestry(*start)?
            };
            for (hash, version) in ancestry {
                known_versions.insert(hash);
                self.collect_internal(
                    version.tree,
                    &mut known_trees,
                    &mut known_files,
                    &mut Vec::new(),
                    &mut Vec::new(),
                    runtime.as_deref_mut(),
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
            let version = if let Some(runtime) = runtime.as_deref_mut() {
                self.version_with_runtime(hash, runtime)?
            } else {
                self.version(hash)?
            };
            queue.extend(version.parents.iter().copied());
            versions.push((hash, version));
        }
        for (hash, version) in versions.iter().rev() {
            self.collect_internal(
                version.tree,
                &mut known_trees,
                &mut known_files,
                &mut plan.trees,
                &mut plan.files,
                runtime.as_deref_mut(),
            )?;
            plan.versions.push(crate::archive_cas::ArchiveObject {
                hash: *hash,
                size: version.canonical_bytes()?.len() as u64,
            });
        }
        Ok(plan)
    }

    /// Adds `tree` and what is below it to `trees` and `files`, skipping what `seen_*`
    /// already hold (and marking it seen). Trees come out children first.
    fn collect_internal(
        &self,
        tree: CasHash,
        seen_trees: &mut HashSet<CasHash>,
        seen_files: &mut HashSet<CasHash>,
        trees: &mut Vec<crate::archive_cas::ArchiveObject>,
        files: &mut Vec<crate::archive_cas::ArchiveObject>,
        mut runtime: Option<&mut RepositoryRuntime>,
    ) -> Result<()> {
        if !seen_trees.insert(tree) {
            return Ok(());
        }
        let tree_value = if let Some(runtime) = runtime.as_deref_mut() {
            self.tree_with_runtime(tree, runtime)?
        } else {
            self.tree(tree)?
        };
        let tree_size = tree_value.canonical_bytes()?.len() as u64;
        for entry in tree_value.entries {
            match entry {
                TreeEntry::File { object, .. } | TreeEntry::Executable { object, .. } => {
                    if seen_files.insert(object.hash) {
                        files.push(object);
                    }
                }
                TreeEntry::ChunkedFile { manifest, file, .. } => {
                    if seen_files.insert(manifest.hash) {
                        files.push(manifest);
                    }
                    let manifest_value = if let Some(runtime) = runtime.as_deref_mut() {
                        self.file_manifest_with_runtime(manifest, file, runtime)?
                    } else {
                        self.file_manifest(manifest, file)?
                    };
                    for chunk in manifest_value.chunks {
                        if seen_files.insert(chunk.object.hash) {
                            files.push(chunk.object);
                        }
                    }
                }
                TreeEntry::Symlink { .. } => {}
                TreeEntry::Directory { tree: child, .. } => {
                    self.collect_internal(
                        child,
                        seen_trees,
                        seen_files,
                        trees,
                        files,
                        runtime.as_deref_mut(),
                    )?;
                }
            }
        }
        trees.push(crate::archive_cas::ArchiveObject {
            hash: tree,
            size: tree_size,
        });
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
            .chain(plan.trees.iter().map(|object| object.hash))
            .chain(plan.versions.iter().map(|object| object.hash));
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

    /// Writes objects using the proactor for bounded source reads and thermal admission.
    /// The stream terminator is written only after every object and the signed head succeed.
    pub fn write_stream_with_runtime<W: Write + Send + 'static>(
        &self,
        plan: &SendPlan,
        head: &SignedHead,
        out: W,
        runtime: &mut RepositoryRuntime,
    ) -> Result<(usize, u64)> {
        let (proactor, handle, thermal, workers) = runtime.blocking_io_parts_mut()?;
        let output = std::io::BufWriter::with_capacity(1 << 20, out);
        let mut out = BlockingIoWriter::new(output, proactor, handle, workers);
        thermal.admit(proactor, handle)?;
        out.write_all(STREAM_MAGIC)?;
        let mut objects = 0;
        let mut bytes = 0_u64;
        for object in plan.files.iter().chain(&plan.trees).chain(&plan.versions) {
            thermal.admit(proactor, handle)?;
            out.write_all(b"O")?;
            out.write_all(object.hash.as_bytes())?;
            out.write_all(&object.size.to_le_bytes())?;
            self.store()
                .stream_object_to_with_proactor_admitted(
                    proactor,
                    handle,
                    *object,
                    &mut out,
                    |proactor, handle| thermal.admit(proactor, handle).map_err(anyhow::Error::from),
                )
                .with_context(|| format!("failed to stream repository object {}", object.hash))?;
            objects += 1;
            bytes = bytes
                .checked_add(object.size)
                .ok_or_else(|| anyhow!("sent object byte count exceeds u64"))?;
        }
        let head = serde_json::to_vec(head)?;
        thermal.admit(proactor, handle)?;
        out.write_all(b"H")?;
        out.write_all(&(head.len() as u64).to_le_bytes())?;
        out.write_all(&head)?;
        out.write_all(b"E")?;
        thermal.admit(proactor, handle)?;
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

/// Remote source configuration for [`fetch_with_runtime`].
pub struct FetchSource<'a> {
    pub remote: &'a Remote,
    pub store: &'a str,
    pub trusted_public_key: Option<&'a str>,
}

trait StreamInput {
    fn read_stream(
        &mut self,
        buffer: &mut [u8],
        runtime: Option<&mut RepositoryRuntime>,
    ) -> Result<usize>;

    fn read_exact_stream(
        &mut self,
        mut buffer: &mut [u8],
        mut runtime: Option<&mut RepositoryRuntime>,
    ) -> Result<()> {
        while !buffer.is_empty() {
            let read = loop {
                match self.read_stream(buffer, runtime.as_deref_mut()) {
                    Err(error)
                        if error.downcast_ref::<std::io::Error>().is_some_and(|error| {
                            error.kind() == std::io::ErrorKind::Interrupted
                        }) =>
                    {
                        continue;
                    }
                    result => break result?,
                }
            };
            if read == 0 {
                bail!("stream ended before the frame was complete");
            }
            buffer = &mut buffer[read..];
        }
        Ok(())
    }
}

struct BorrowedStreamInput<'a>(&'a mut dyn Read);

impl StreamInput for BorrowedStreamInput<'_> {
    fn read_stream(
        &mut self,
        buffer: &mut [u8],
        _runtime: Option<&mut RepositoryRuntime>,
    ) -> Result<usize> {
        Ok(self.0.read(buffer)?)
    }
}

struct BlockingStreamInput<R> {
    reader: Option<BufReader<R>>,
    buffer: Vec<u8>,
}

impl<R: Read> BlockingStreamInput<R> {
    fn new(reader: R) -> Self {
        Self {
            reader: Some(BufReader::new(reader)),
            buffer: Vec::with_capacity(1 << 20),
        }
    }
}

impl<R: Read + Send + 'static> StreamInput for BlockingStreamInput<R> {
    fn read_stream(
        &mut self,
        output: &mut [u8],
        runtime: Option<&mut RepositoryRuntime>,
    ) -> Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        let runtime = runtime.ok_or_else(|| anyhow!("runtime stream input has no runtime"))?;
        let mut reader = self
            .reader
            .take()
            .ok_or_else(|| anyhow!("runtime stream reader is already in use"))?;
        let mut buffer = std::mem::take(&mut self.buffer);
        buffer.resize(output.len(), 0);
        let (next_reader, next_buffer, result) = runtime.run_blocking_io(move || {
            let result = loop {
                match reader.read(&mut buffer) {
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    result => break result,
                }
            };
            (reader, buffer, result)
        })?;
        self.reader = Some(next_reader);
        self.buffer = next_buffer;
        let read = result?;
        output[..read].copy_from_slice(&self.buffer[..read]);
        Ok(read)
    }
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
    let mut stream = BorrowedStreamInput(&mut input);
    receive_internal(root, &mut stream, trust, None, None, || Ok(()))
}

/// Like [`receive`], ingesting verified object files through the command proactor.
/// The owned input is read in bounded chunks on the runtime's blocking-I/O lane,
/// leaving the command thread available to dispatch proactor completions.
pub fn receive_with_runtime<R: Read + Send + 'static>(
    root: &Path,
    input: R,
    trust: &Trust,
    runtime: &mut RepositoryRuntime,
) -> Result<ReceiveReport> {
    let mut input = BlockingStreamInput::new(input);
    receive_internal(root, &mut input, trust, Some(runtime), None, || Ok(()))
}

/// Fetches a branch by asking a local or SSH remote to emit a stream. A locally
/// complete branch tip is sent as a reuse hint; otherwise the source sends all
/// ancestry. The head moves only after sender success and full closure verification.
pub fn fetch_with_runtime(
    root: &Path,
    repository_id: &str,
    branch: &str,
    source: &FetchSource<'_>,
    trust: &Trust,
    runtime: &mut RepositoryRuntime,
) -> Result<ReceiveReport> {
    let known = if super::is_store(root) {
        let local = Repository::open(root, repository_id, false)?;
        match local.head_with_runtime(branch, runtime)? {
            Some(head)
                if local
                    .check_closure_with_runtime(head.version, runtime)
                    .is_ok() =>
            {
                vec![head.version]
            }
            _ => Vec::new(),
        }
    } else {
        Vec::new()
    };
    let mut args = vec![
        "send".to_string(),
        "--stream-stdout".to_string(),
        "--store".to_string(),
        source.store.to_string(),
        "--repository".to_string(),
        repository_id.to_string(),
        "--branch".to_string(),
        branch.to_string(),
    ];
    for version in &known {
        args.push("--known-version".to_string());
        args.push(version.to_string());
    }
    if let Some(path) = source.trusted_public_key {
        args.push("--trusted-public-key".to_string());
        args.push(path.to_string());
    }
    let mut command = source.remote.command(&args);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let mut child = command
        .spawn()
        .context("failed to start remote fetch sender")?;
    let output = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("remote fetch sender has no stdout pipe"))?;
    let mut input = BlockingStreamInput::new(output);
    let result = receive_internal(
        root,
        &mut input,
        trust,
        Some(runtime),
        Some((repository_id, branch)),
        || {
            let status = child
                .wait()
                .context("failed to wait for remote fetch sender")?;
            if !status.success() {
                bail!("remote fetch sender exited with {status}");
            }
            Ok(())
        },
    );
    match result {
        Ok(report) => Ok(report),
        Err(error) => {
            if child.try_wait().ok().flatten().is_none() {
                let _ = child.kill();
            }
            let _ = child.wait();
            Err(error).context("fetch failed; the local branch head was not advanced")
        }
    }
}

fn receive_internal(
    root: &Path,
    input: &mut impl StreamInput,
    trust: &Trust,
    mut runtime: Option<&mut RepositoryRuntime>,
    expected: Option<(&str, &str)>,
    before_publish: impl FnOnce() -> Result<()>,
) -> Result<ReceiveReport> {
    let mut magic = vec![0_u8; STREAM_MAGIC.len()];
    input
        .read_exact_stream(&mut magic, runtime.as_deref_mut())
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
            .read_exact_stream(&mut tag, runtime.as_deref_mut())
            .context("the stream is cut short")?;
        match tag[0] {
            b'O' => {
                let mut hash = [0_u8; 32];
                input.read_exact_stream(&mut hash, runtime.as_deref_mut())?;
                let hash = CasHash::from_bytes(hash);
                let length = read_length(input, runtime.as_deref_mut())?;
                crate::disk::ensure_room(root, length, crate::disk::floor_from_env())?;
                receive_object(
                    &store,
                    hash,
                    length,
                    input,
                    &mut buffer,
                    runtime.as_deref_mut(),
                )?;
                objects += 1;
            }
            b'H' => {
                let length = read_length(input, runtime.as_deref_mut())?;
                if length > LARGEST_HEAD {
                    bail!("the head is {length} bytes; not a head");
                }
                let mut bytes = vec![0_u8; usize::try_from(length)?];
                input.read_exact_stream(&mut bytes, runtime.as_deref_mut())?;
                head = Some(
                    serde_json::from_slice::<SignedHead>(&bytes)
                        .context("the head is not a head")?,
                );
            }
            b'E' => break,
            other => bail!("unknown frame {other:#04x} in the stream"),
        }
    }
    let mut trailing = [0_u8; 1];
    if input.read_stream(&mut trailing, runtime.as_deref_mut())? != 0 {
        bail!("bytes after the end of the stream");
    }
    let head = head.ok_or_else(|| anyhow!("the stream holds no head"))?;
    if let Some((repository_id, branch)) = expected {
        if head.repository_id != repository_id || head.branch != branch {
            bail!(
                "fetch requested {repository_id}/{branch}, but the source sent {}/{}",
                head.repository_id,
                head.branch
            );
        }
    }
    before_publish()?;
    let version = if let Some(runtime) = runtime.as_deref_mut() {
        runtime.verify_head(&head, trust)?
    } else {
        head.verify(trust)?
    };
    let repository = Repository::open(root, &head.repository_id, false)?;
    let current_head = if let Some(runtime) = runtime.as_deref_mut() {
        repository.head_with_runtime(&head.branch, runtime)?
    } else {
        repository.head(&head.branch)?
    };
    let current = current_head.map(|current| current.version);
    if current == Some(version) {
        // Nothing to move, but say so only if what the branch names is intact here.
        let closure = if let Some(runtime) = runtime.as_deref_mut() {
            repository.check_closure_with_runtime(version, runtime)
        } else {
            repository.check_closure(version)
        };
        closure.with_context(|| {
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
    if let Some(runtime) = runtime.as_deref_mut() {
        repository.check_closure_with_runtime(version, runtime)?;
    } else {
        repository.check_closure(version)?;
    }
    if let Some(current) = current {
        let is_ancestor = if let Some(runtime) = runtime.as_deref_mut() {
            repository.is_ancestor_with_runtime(current, version, runtime)?
        } else {
            repository.is_ancestor(current, version)?
        };
        if !is_ancestor {
            bail!(
                "branch {} is at {current}, which is not in the history of {version}; refusing to move it back or sideways",
                head.branch
            );
        }
    }
    if let Some(runtime) = runtime {
        repository.set_head_with_runtime(&head, current, runtime)?;
    } else {
        repository.set_head(&head, current)?;
    }
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
    input: &mut impl StreamInput,
    buffer: &mut Vec<u8>,
    mut runtime: Option<&mut RepositoryRuntime>,
) -> Result<()> {
    let partial = store.scratch_path("receive");
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&partial)
            .with_context(|| format!("failed to create {}", partial.display()))?;
        let mut hasher = blake3::Hasher::new();
        let mut copied = 0_u64;
        while copied < length {
            let limit = usize::try_from((length - copied).min(buffer.len() as u64))?;
            let read = input.read_stream(&mut buffer[..limit], runtime.as_deref_mut())?;
            if read == 0 {
                break;
            }
            if let Some(runtime) = runtime.as_deref_mut() {
                runtime.admit_optional_work()?;
            }
            if let Some(runtime) = runtime.as_deref_mut() {
                let block = std::mem::take(buffer);
                let (next_file, next_buffer, write_result) =
                    runtime.run_blocking_io(move || {
                        let result = file.write_all(&block[..read]);
                        (file, block, result)
                    })?;
                file = next_file;
                *buffer = next_buffer;
                write_result?;
            } else {
                file.write_all(&buffer[..read])?;
            }
            if let Some(runtime) = runtime.as_deref_mut() {
                let block = std::mem::take(buffer);
                let task = runtime.try_submit_cpu(move || {
                    let mut hasher = hasher;
                    hasher.update(&block[..read]);
                    (hasher, block)
                })?;
                let (next_hasher, next_buffer) = runtime.wait_cpu(task)?;
                hasher = next_hasher;
                *buffer = next_buffer;
            } else {
                hasher.update(&buffer[..read]);
            }
            copied += read as u64;
        }
        if copied != length {
            bail!("object {hash} is cut short");
        }
        let actual = CasHash::from_bytes(*hasher.finalize().as_bytes());
        if actual != hash {
            bail!("object {hash} arrived as bytes hashing to {actual}");
        }
        if let Some(runtime) = runtime.as_deref_mut() {
            runtime.admit_optional_work()?;
            let (next_file, sync_result) = runtime.run_blocking_io(move || {
                let result = file.sync_all();
                (file, result)
            })?;
            file = next_file;
            sync_result?;
        } else {
            file.sync_all()?;
        }
        drop(file);
        let stored_copy_is_intact = if store.has_object(hash) {
            if let Some(runtime) = runtime.as_deref_mut() {
                stored_copy_is_intact_with_runtime(store, hash, length, runtime)?
            } else {
                stored_copy_is_intact(store, hash, buffer)
            }
        } else {
            false
        };
        if stored_copy_is_intact {
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
        let stored = if let Some(runtime) = runtime {
            let (proactor, handle, thermal, _) = runtime.parts_mut();
            store
                .ingest_file_with_proactor_admitted(
                    proactor,
                    handle,
                    &partial,
                    |proactor, handle| thermal.admit(proactor, handle).map_err(anyhow::Error::from),
                )
                .map(|result| result.object)
                .with_context(|| format!("failed to store {hash}"))
        } else {
            store
                .add_stream(&mut File::open(&partial)?)
                .map(|result| result.object)
                .with_context(|| format!("failed to store {hash}"))
        };
        match stored {
            Ok(stored) if stored.hash == hash => {
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

/// Verifies an existing raw or compressed CAS copy using admitted proactor range
/// reads and bounded CPU-worker hash updates. A failed CAS read marks the copy as
/// damaged; a thermal-admission or worker failure aborts receive without replacing it.
pub(super) fn stored_copy_is_intact_with_runtime(
    store: &crate::archive_cas::ArchiveCasStorage,
    hash: CasHash,
    size: u64,
    runtime: &mut RepositoryRuntime,
) -> Result<bool> {
    let mut buffer = vec![0_u8; STORED_COPY_VERIFY_BUFFER_BYTES];
    let mut hasher = blake3::Hasher::new();
    let mut offset = 0_u64;
    while offset < size {
        let wanted = usize::try_from((size - offset).min(buffer.len() as u64))?;
        let Some(read) =
            read_stored_copy_range(store, hash, offset, &mut buffer[..wanted], runtime)?
        else {
            return Ok(false);
        };
        if read == 0 || read > wanted {
            return Ok(false);
        }
        let block = std::mem::take(&mut buffer);
        let task = runtime.try_submit_cpu(move || {
            hasher.update(&block[..read]);
            (hasher, block)
        })?;
        (hasher, buffer) = runtime.wait_cpu(task)?;
        offset = offset
            .checked_add(read as u64)
            .ok_or_else(|| anyhow!("stored object verification offset exceeds u64"))?;
    }

    // Confirm the stored representation has no bytes beyond its declared size.
    let Some(extra) = read_stored_copy_range(store, hash, offset, &mut buffer[..1], runtime)?
    else {
        return Ok(false);
    };
    if extra != 0 {
        return Ok(false);
    }
    Ok(CasHash::from_bytes(*hasher.finalize().as_bytes()) == hash)
}

/// Reads one existing-object range. Thermal failures propagate; object read errors
/// mean that this stored copy is unusable and can be repaired from the proven input.
fn read_stored_copy_range(
    store: &crate::archive_cas::ArchiveCasStorage,
    hash: CasHash,
    offset: u64,
    output: &mut [u8],
    runtime: &mut RepositoryRuntime,
) -> Result<Option<usize>> {
    let mut admission_error = None;
    let read = {
        let (proactor, handle, thermal, _) = runtime.parts_mut();
        store.read_range_into_with_proactor_admitted(
            proactor,
            handle,
            hash,
            offset,
            output,
            |proactor, handle| match thermal.admit(proactor, handle) {
                Ok(()) => Ok(()),
                Err(error) => {
                    admission_error = Some(anyhow::Error::from(error));
                    Err(anyhow!("thermal admission failed during stored-copy read"))
                }
            },
        )
    };
    if let Some(error) = admission_error {
        return Err(error).context("failed to admit stored-copy verification");
    }
    Ok(read.ok())
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

fn read_length(
    input: &mut impl StreamInput,
    runtime: Option<&mut RepositoryRuntime>,
) -> Result<u64> {
    let mut length = [0_u8; 8];
    input.read_exact_stream(&mut length, runtime)?;
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
        let mut objects = std::collections::BTreeMap::new();
        let mut files = std::collections::BTreeMap::new();
        for (_, version) in self.ancestry(version)? {
            self.check_tree(version.tree, &mut trees, &mut objects, &mut files)?;
        }
        for (&hash, &size) in &objects {
            self.store()
                .verify_object(crate::archive_cas::ArchiveObject { hash, size })
                .with_context(|| format!("repository object {hash} is missing or damaged"))?;
        }
        Ok(())
    }

    /// Like [`Self::check_closure`], checking metadata and physical CAS objects through
    /// bounded proactor reads with thermal admission before a received head is published.
    pub fn check_closure_with_runtime(
        &self,
        version: CasHash,
        runtime: &mut RepositoryRuntime,
    ) -> Result<()> {
        let mut trees = std::collections::HashMap::new();
        let mut objects = std::collections::BTreeMap::new();
        let mut files = std::collections::BTreeMap::new();
        for (_, version) in self.ancestry_with_runtime(version, runtime)? {
            self.check_tree_with_runtime(
                version.tree,
                &mut trees,
                &mut objects,
                &mut files,
                runtime,
            )?;
        }
        for (&hash, &size) in &objects {
            if !stored_copy_is_intact_with_runtime(self.store(), hash, size, runtime)? {
                bail!("repository object {hash} is missing or damaged");
            }
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

/// Runtime form of [`list_heads`], enumerating and reading heads on one
/// thermally admitted bounded blocking-I/O operation.
pub fn list_heads_with_runtime(
    root: &Path,
    id: &str,
    runtime: &mut RepositoryRuntime,
) -> Result<String> {
    super::validate_id(id, "repository id")?;
    if !super::is_store(root) {
        return Ok(String::new());
    }
    let repository = Repository::open(root, id, false)?;
    let mut text = String::new();
    for (branch, head) in repository.heads_with_runtime(runtime)? {
        text.push_str(&format!("{branch}\t{}\n", head.version));
    }
    Ok(text)
}
