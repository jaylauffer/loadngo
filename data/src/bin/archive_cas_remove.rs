//! Removes named paths from an archive manifest, producing a new manifest
//! that supersedes the old one and a delete-log sidecar explaining the
//! change. Never touches blob objects, other manifest files, or signatures --
//! reclaiming disk space is [`archive_cas_gc`], and it must be run after
//! re-signing the superseding manifest this tool writes.

use anyhow::{anyhow, bail, Context, Result};
use data::archive_cas::{ArchiveCasStorage, ArchiveObject};
use data::cli::{ArgDoc, Usage};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    if let Err(error) = run() {
        eprintln!("archive_cas_remove: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args = Args::parse()?;
    let store = ArchiveCasStorage::new(&args.cas_root)?;
    let manifest = store.read_manifest(&args.manifest)?;
    let manifest_bytes = manifest.canonical_bytes()?;
    let previous_root = ArchiveObject {
        hash: manifest.digest()?,
        size: u64::try_from(manifest_bytes.len()).context("manifest length exceeds u64")?,
    };
    store
        .verify_object(previous_root)
        .context("source manifest is not present as a verified CAS object")?;

    let (amended, log) =
        manifest.with_entries_removed(&args.paths, args.reason, args.actor, unix_now()?)?;
    let (manifest_path, archive_root) = store.write_manifest(&amended)?;
    let log_path = store.write_delete_log(&manifest_path, &log)?;

    println!("Superseded archive root: {}", previous_root.hash);
    println!("New archive manifest: {}", manifest_path.display());
    println!("New archive root object: {}", archive_root.hash);
    println!("Delete log: {}", log_path.display());
    println!("Entries removed: {}", log.removed_paths.len());
    for path in &log.removed_paths {
        println!("  - {path}");
    }
    println!();
    println!("This manifest is not yet signed. Next steps:");
    println!(
        "  1. archive_cas_sign sign --cas-root {} --manifest {} --signer-identity <you> --public-key <pub> --private-key <priv>",
        args.cas_root.display(),
        manifest_path.display()
    );
    println!("  2. Once you're satisfied, archive_cas_prune_manifests to retire the old manifest/signature.");
    println!("  3. archive_cas_gc --dry-run to see what disk space the removal frees.");
    Ok(())
}

#[derive(Debug)]
struct Args {
    cas_root: PathBuf,
    manifest: PathBuf,
    paths: Vec<String>,
    reason: String,
    actor: String,
}

impl Args {
    fn parse() -> Result<Self> {
        let mut cas_root = None;
        let mut manifest = None;
        let mut paths = Vec::new();
        let mut reason = None;
        let mut actor = None;
        let mut args = data::cli::read_args(&usage(), true).into_iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--cas-root" => cas_root = args.next().map(PathBuf::from),
                "--manifest" => manifest = args.next().map(PathBuf::from),
                "--path" => paths.push(
                    args.next()
                        .ok_or_else(|| anyhow!("--path requires a value"))?,
                ),
                "--reason" => reason = args.next(),
                "--actor" => actor = args.next(),
                other => return Err(anyhow!("unknown argument: {other}\n{}", usage().hint())),
            }
        }
        let reason =
            reason.ok_or_else(|| anyhow!("missing --reason <text>\n{}", usage().hint()))?;
        if reason.trim().is_empty() {
            bail!("--reason must not be empty");
        }
        let actor = actor.ok_or_else(|| anyhow!("missing --actor <name>\n{}", usage().hint()))?;
        if actor.trim().is_empty() {
            bail!("--actor must not be empty");
        }
        if paths.is_empty() {
            bail!("at least one --path is required\n{}", usage().hint());
        }
        Ok(Self {
            cas_root: cas_root.ok_or_else(|| {
                anyhow!("missing --cas-root <archive-directory>\n{}", usage().hint())
            })?,
            manifest: manifest.ok_or_else(|| {
                anyhow!(
                    "missing --manifest <archive-manifest.json>\n{}",
                    usage().hint()
                )
            })?,
            paths,
            reason,
            actor,
        })
    }
}

fn usage() -> Usage {
    const ARGS: &[ArgDoc] = &[
        ArgDoc::required(
            "--cas-root",
            "<archive-directory>",
            "Archive CAS root that holds the manifest",
        ),
        ArgDoc::required(
            "--manifest",
            "<archive-manifest.json>",
            "manifest to remove entries from",
        ),
        ArgDoc::repeated(
            "--path",
            "<manifest-path>",
            "manifest-relative path to remove; a directory path also removes everything nested under it",
        ),
        ArgDoc::required("--reason", "<why>", "non-empty justification recorded in the delete log"),
        ArgDoc::required("--actor", "<who>", "non-empty name of the person or agent removing these paths"),
    ];
    const EXAMPLES: &[&str] = &[
        "cargo run -p data --bin archive_cas_remove -- --cas-root /Volumes/Backup/loadngo-archive-cas --manifest /Volumes/Backup/loadngo-archive-cas/manifests/<archive>.json --path some/private/file.txt --reason \"owner-requested removal\" --actor jay",
    ];
    const NOTES: &[&str] = &[
        "Writes a new, superseding manifest plus a delete-log sidecar; never touches blob objects, other manifests, or signatures.",
        "Reclaiming the freed disk space is a separate step: archive_cas_gc.",
        "The superseding manifest is unsigned; sign it with archive_cas_sign before treating it as the archive of record.",
    ];
    Usage {
        bin: "archive_cas_remove",
        invocation: "cargo run -p data --bin archive_cas_remove --",
        about: "remove named paths from an archive manifest, producing a new manifest that supersedes the old one",
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
