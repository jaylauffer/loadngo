//! Moves the sidecar logs written beside manifests before v3 into the store: the
//! archive's current version is followed by a v3 version with the same entries, made
//! from it, that carries the logs unchanged as unverified history. The root covers the
//! logs' bytes, so they can no longer be lost or edited unnoticed, but nothing vouches
//! for what they say (see `docs/RECONCILIATION.md`, "Decisions").
//!
//! Once the new version is written and every log reads back from the store, the log
//! files under `manifests/` are deleted; `archive_cas_restore --path <log name>` gets
//! one back. Nothing else is changed, and nothing is signed.

use anyhow::{anyhow, bail, Context, Result};
use data::archive_cas::{ArchiveAttachment, ArchiveCasStorage};
use data::archive_view::list_archives;
use data::cli::{ArgDoc, Usage};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    if let Err(error) = run() {
        eprintln!("archive_cas_upgrade: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args = Args::parse()?;
    let store = ArchiveCasStorage::new(&args.cas_root)?;
    let current: Vec<_> = list_archives(&args.cas_root, None)?
        .into_iter()
        .filter(|l| l.archive_id == args.archive && !l.superseded)
        .collect();
    let listing = match current.as_slice() {
        [one] => one,
        [] => bail!(
            "no archive {:?} in {}",
            args.archive,
            args.cas_root.display()
        ),
        _ => bail!(
            "archive {:?} has more than one current version",
            args.archive
        ),
    };

    let mut names = BTreeSet::new();
    let mut logs = Vec::new();
    for path in &args.attach {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| anyhow!("{} has no usable name", path.display()))?
            .to_string();
        if !names.insert(name.clone()) {
            bail!("two files named {name:?} are attached");
        }
        let metadata =
            fs::metadata(path).with_context(|| format!("cannot read {}", path.display()))?;
        if !metadata.is_file() {
            bail!("{} is not a file", path.display());
        }
        logs.push((path.clone(), name, metadata.len()));
    }

    println!(
        "Current version of {}: {}",
        args.archive,
        listing.manifest_path.display()
    );
    println!("Attaching {} files as unverified history:", logs.len());
    for (path, _, size) in &logs {
        println!("  {} ({size} bytes)", path.display());
    }
    if args.dry_run {
        println!("Dry run: nothing written.");
        return Ok(());
    }

    eprintln!("reading {}", listing.manifest_path.display());
    let (manifest, root) = store.read_manifest_and_root(&listing.manifest_path)?;
    let mut attachments = Vec::new();
    for (path, name, _) in &logs {
        let bytes = fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
        let object = store.add_content(&bytes)?.object;
        attachments.push(ArchiveAttachment {
            name: name.clone(),
            object,
        });
    }
    let next = manifest.with_unverified_history(attachments, unix_now()?)?;
    let (path, object) = store.write_manifest(&next)?;
    for attached in &next.unverified_history {
        store
            .verify_object(attached.object)
            .with_context(|| format!("stored copy of {} does not verify", attached.name))?;
    }

    let manifests = fs::canonicalize(store.manifests_root())?;
    let mut removed = 0;
    for (log, _, _) in &logs {
        if in_directory(log, &manifests) {
            fs::remove_file(log).with_context(|| format!("cannot remove {}", log.display()))?;
            removed += 1;
        }
    }

    println!("New version: {}", path.display());
    println!("Root: {} (made from {})", object.hash, &root.to_hex()[..12]);
    println!(
        "Attachments stored and verified: {}; log files removed from manifests/: {removed}",
        next.unverified_history.len()
    );
    println!();
    println!("Unsigned. Sign it with archive_cas_sign; a purge then retires the version it was");
    println!("made from, keeping that version's manifest and signature.");
    Ok(())
}

/// Whether `file` sits directly in `directory` (already canonical).
fn in_directory(file: &Path, directory: &Path) -> bool {
    fs::canonicalize(file)
        .ok()
        .and_then(|file| file.parent().map(|parent| parent == directory))
        .unwrap_or(false)
}

#[derive(Debug)]
struct Args {
    cas_root: PathBuf,
    archive: String,
    attach: Vec<PathBuf>,
    dry_run: bool,
}

impl Args {
    fn parse() -> Result<Self> {
        let mut cas_root = None;
        let mut archive = None;
        let mut attach = Vec::new();
        let mut dry_run = false;
        let mut args = data::cli::read_args(&usage(), true).into_iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--cas-root" => cas_root = args.next().map(PathBuf::from),
                "--archive" => archive = args.next(),
                "--attach" => attach.push(PathBuf::from(
                    args.next()
                        .ok_or_else(|| anyhow!("--attach requires a file"))?,
                )),
                "--dry-run" => dry_run = true,
                other => bail!("unknown argument: {other}\n{}", usage().hint()),
            }
        }
        if attach.is_empty() {
            bail!("at least one --attach is required\n{}", usage().hint());
        }
        Ok(Self {
            cas_root: cas_root.ok_or_else(|| {
                anyhow!("missing --cas-root <archive-directory>\n{}", usage().hint())
            })?,
            archive: archive
                .ok_or_else(|| anyhow!("missing --archive <archive-id>\n{}", usage().hint()))?,
            attach,
            dry_run,
        })
    }
}

fn usage() -> Usage {
    const ARGS: &[ArgDoc] = &[
        ArgDoc::required(
            "--cas-root",
            "<archive-directory>",
            "Archive CAS root that holds the archive",
        ),
        ArgDoc::required(
            "--archive",
            "<archive-id>",
            "the archive whose current version gets the files",
        ),
        ArgDoc::repeated(
            "--attach",
            "<file>",
            "a file to keep, unchanged, as unverified history (an old .delete-log.json, .add-log.json, .merge-log.json or .unpack-log.json); at least one",
        ),
        ArgDoc::switch("--dry-run", "list what would be attached; write nothing"),
    ];
    const EXAMPLES: &[&str] = &[
        "cargo run -p data --bin archive_cas_upgrade -- --cas-root \"/Volumes/Zhoenus II/pudding-cas\" --archive pudding-20260917 --attach \"/Volumes/Zhoenus II/pudding-cas/manifests/pudding-20260917-4d8b...delete-log.json\" --dry-run",
    ];
    const NOTES: &[&str] = &[
        "The new version has the same entries, names the current version as its parent, and records no change of its own.",
        "Attached files that sit in the root's manifests/ directory are deleted once their stored copies verify; others are left where they are.",
        "Restore an attached log with archive_cas_restore --path <its file name>.",
        "The new version is unsigned; sign it with archive_cas_sign.",
    ];
    Usage {
        bin: "archive_cas_upgrade",
        invocation: "cargo run -p data --bin archive_cas_upgrade --",
        about: "keep an archive's old sidecar logs inside the store, as unverified history of a new v3 version",
        args: ARGS,
        examples: EXAMPLES,
        notes: NOTES,
    }
}

fn unix_now() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_secs())
}
