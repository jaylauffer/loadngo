//! Retires superseded manifest files (and their signatures) for one archive
//! id, once you're satisfied a newer manifest is the one you want to keep.
//!
//! This destroys append-only history on purpose -- it is a separate,
//! explicit step from `archive_cas_remove`, never bundled into it. It
//! refuses to prune anything that isn't actually reachable as an ancestor of
//! the manifest you're keeping, via `supersedes_archive_root`, so it can't be
//! used to discard an unrelated or divergent manifest by mistake.
//!
//! Pruning history does not free any disk space by itself -- blobs stay on
//! disk until `archive_cas_gc` confirms nothing references them anymore.
//! Dry-run by default; pass `--execute` to actually delete.

use anyhow::{anyhow, bail, Context, Result};
use data::archive_cas::{ArchiveCasStorage, ArchiveManifest};
use data::cas::CasHash;
use std::collections::BTreeMap;
use std::path::PathBuf;

fn main() {
    if let Err(error) = run() {
        eprintln!("archive_cas_prune_manifests: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args = Args::parse()?;
    let store = ArchiveCasStorage::new(&args.cas_root)?;
    let keep = store.read_manifest(&args.keep)?;
    if keep.archive_id != args.archive_id {
        bail!(
            "--keep manifest has archive_id {:?}, expected {:?}",
            keep.archive_id,
            args.archive_id
        );
    }
    let keep_root = keep.digest()?;

    // Every manifest file for this archive_id, and the ancestor chain of the
    // one being kept (by root hash), so we can tell "superseded ancestor" from
    // "unrelated or divergent manifest that happens to share an archive_id."
    let mut by_root: BTreeMap<CasHash, PathBuf> = BTreeMap::new();
    let mut manifests_by_root: BTreeMap<CasHash, ArchiveManifest> = BTreeMap::new();
    for path in store.list_manifests()? {
        let manifest = store
            .read_manifest(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        if manifest.archive_id != args.archive_id {
            continue;
        }
        let root = manifest.digest()?;
        by_root.insert(root, path);
        manifests_by_root.insert(root, manifest);
    }

    let mut ancestors = std::collections::BTreeSet::new();
    let mut cursor = manifests_by_root
        .get(&keep_root)
        .context("--keep manifest was not found among this archive_id's manifests")?
        .supersedes_archive_root;
    while let Some(root) = cursor {
        ancestors.insert(root);
        cursor = manifests_by_root
            .get(&root)
            .and_then(|manifest| manifest.supersedes_archive_root);
    }

    let mut to_prune: Vec<(CasHash, PathBuf)> = by_root
        .into_iter()
        .filter(|(root, _)| *root != keep_root && ancestors.contains(root))
        .collect();
    to_prune.sort_by_key(|(root, _)| *root);

    if to_prune.is_empty() {
        println!("Nothing to prune: no superseded ancestor manifests found for archive_id {:?} behind {}.", args.archive_id, keep_root);
        return Ok(());
    }

    println!(
        "{} superseded manifest(s) for archive_id {:?}, ancestors of kept root {}:",
        to_prune.len(),
        args.archive_id,
        keep_root
    );
    for (root, path) in &to_prune {
        let signature_path = args.cas_root.join("manifests").join(format!(
            "{}-{}.signature.json",
            args.archive_id,
            root.to_hex()
        ));
        println!("  manifest {} ({})", root, path.display());
        if signature_path.exists() {
            println!("    + signature {}", signature_path.display());
        }
    }

    if !args.execute {
        println!();
        println!("Dry run only -- nothing removed. Pass --execute to actually delete these files.");
        return Ok(());
    }

    for (root, path) in &to_prune {
        store
            .remove_manifest_file(path)
            .with_context(|| format!("failed to remove {}", path.display()))?;
        let signature_path = args.cas_root.join("manifests").join(format!(
            "{}-{}.signature.json",
            args.archive_id,
            root.to_hex()
        ));
        if signature_path.exists() {
            std::fs::remove_file(&signature_path).with_context(|| {
                format!("failed to remove signature {}", signature_path.display())
            })?;
        }
    }
    println!("Pruned {} manifest(s).", to_prune.len());
    println!("Run archive_cas_gc --dry-run to see what disk space is now reclaimable.");
    Ok(())
}

#[derive(Debug)]
struct Args {
    cas_root: PathBuf,
    archive_id: String,
    keep: PathBuf,
    execute: bool,
}

impl Args {
    fn parse() -> Result<Self> {
        let mut cas_root = None;
        let mut archive_id = None;
        let mut keep = None;
        let mut execute = false;
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--cas-root" => cas_root = args.next().map(PathBuf::from),
                "--archive-id" => archive_id = args.next(),
                "--keep" => keep = args.next().map(PathBuf::from),
                "--execute" => execute = true,
                "--help" | "-h" => {
                    print_usage();
                    std::process::exit(0);
                }
                other => return Err(anyhow!("unknown argument: {other}")),
            }
        }
        Ok(Self {
            cas_root: cas_root.ok_or_else(|| anyhow!("missing --cas-root <archive-directory>"))?,
            archive_id: archive_id.ok_or_else(|| anyhow!("missing --archive-id <id>"))?,
            keep: keep.ok_or_else(|| anyhow!("missing --keep <manifest-to-retain>"))?,
            execute,
        })
    }
}

fn print_usage() {
    eprintln!(
        "Usage: cargo run -p data --bin archive_cas_prune_manifests -- \\\n  --cas-root <archive-directory> --archive-id <id> --keep <manifest-to-retain.json> [--execute]\n\nDry-run by default; reports superseded ancestor manifests (and their\nsignatures) for the given archive_id. Pass --execute to actually delete them."
    );
}
