use anyhow::{anyhow, bail, Context, Result};
use data::archive_cas::{ArchiveCasStorage, ArchiveObject};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    if let Err(error) = run() {
        eprintln!("archive_cas_exclude: {error:#}");
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

    let excluded_path = match (args.path, args.only_unreadable) {
        (Some(path), false) => path,
        (None, true) => {
            let unresolved = manifest
                .entries
                .iter()
                .filter_map(|entry| match entry {
                    data::archive_cas::ArchiveEntry::Unreadable { path, .. } => Some(path),
                    _ => None,
                })
                .collect::<Vec<_>>();
            match unresolved.as_slice() {
                [path] => (*path).to_string(),
                [] => bail!("archive manifest has no unresolved unreadable entries"),
                _ => bail!(
                    "archive manifest has {} unresolved entries; name one with --path",
                    unresolved.len()
                ),
            }
        }
        (Some(_), true) | (None, false) => {
            bail!("supply exactly one of --path or --only-unreadable")
        }
    };
    let amended =
        manifest.with_owner_approved_exclusion(&excluded_path, args.reason, unix_now()?)?;
    let (manifest_path, archive_root) = store.write_manifest(&amended)?;
    println!("Superseded archive root: {}", previous_root.hash);
    println!("Archive manifest: {}", manifest_path.display());
    println!("Archive root object: {}", archive_root.hash);
    println!(
        "Declared source exclusions: {}",
        amended.excluded_entry_count()
    );
    println!(
        "Capture complete within declared scope: {}",
        amended.is_complete()
    );
    Ok(())
}

#[derive(Debug)]
struct Args {
    cas_root: PathBuf,
    manifest: PathBuf,
    path: Option<String>,
    only_unreadable: bool,
    reason: String,
}

impl Args {
    fn parse() -> Result<Self> {
        let mut cas_root = None;
        let mut manifest = None;
        let mut path = None;
        let mut reason = None;
        let mut only_unreadable = false;
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--cas-root" => cas_root = args.next().map(PathBuf::from),
                "--manifest" => manifest = args.next().map(PathBuf::from),
                "--path" => path = args.next(),
                "--only-unreadable" => only_unreadable = true,
                "--reason" => reason = args.next(),
                "--help" | "-h" => {
                    print_usage();
                    std::process::exit(0);
                }
                other => return Err(anyhow!("unknown argument: {other}")),
            }
        }
        let reason = reason.ok_or_else(|| anyhow!("missing --reason <text>"))?;
        if reason.trim().is_empty() {
            bail!("--reason must not be empty");
        }
        Ok(Self {
            cas_root: cas_root.ok_or_else(|| anyhow!("missing --cas-root <archive-directory>"))?,
            manifest: manifest
                .ok_or_else(|| anyhow!("missing --manifest <archive-manifest.json>"))?,
            path,
            only_unreadable,
            reason,
        })
    }
}

fn print_usage() {
    eprintln!(
        "Usage: cargo run -p data --bin archive_cas_exclude -- --cas-root <archive-directory> --manifest <archive-manifest.json> (--path <unreadable-manifest-path> | --only-unreadable) --reason <owner-approved-scope-reason>"
    );
}

fn unix_now() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_secs())
}
