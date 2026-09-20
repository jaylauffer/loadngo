use anyhow::{anyhow, bail, Context, Result};
use data::archive_cas::{ArchiveCasStorage, ArchiveEntry, ArchiveObject};
use data::cli::{ArgDoc, Usage};
use std::fs;
use std::path::PathBuf;

fn main() {
    if let Err(error) = run() {
        eprintln!("archive_cas_restore: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args = Args::parse()?;
    if args.destination.exists() {
        bail!(
            "--destination must not exist; archive restore will never overwrite it: {}",
            args.destination.display()
        );
    }
    let destination_parent = args.destination.parent().ok_or_else(|| {
        anyhow!(
            "--destination needs an existing parent directory: {}",
            args.destination.display()
        )
    })?;
    if !destination_parent.is_dir() {
        bail!(
            "--destination parent is not a directory: {}",
            destination_parent.display()
        );
    }

    let store = ArchiveCasStorage::new(&args.cas_root)?;
    let manifest = store.read_manifest(&args.manifest)?;
    let manifest_bytes = manifest.canonical_bytes()?;
    let manifest_object = ArchiveObject {
        hash: manifest.digest()?,
        size: u64::try_from(manifest_bytes.len()).context("manifest length exceeds u64")?,
    };
    store
        .verify_object(manifest_object)
        .context("archive manifest is not present as a verified CAS object")?;
    let selected = manifest
        .entries
        .iter()
        .filter_map(|entry| match entry {
            ArchiveEntry::File { path, object, .. }
                if args.paths.iter().any(|wanted| wanted == path) =>
            {
                Some((path, *object))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    if selected.len() != args.paths.len() {
        let missing = args
            .paths
            .iter()
            .filter(|wanted| !selected.iter().any(|(path, _)| path == wanted))
            .cloned()
            .collect::<Vec<_>>();
        bail!(
            "selected archive file paths were not found: {}",
            missing.join(", ")
        );
    }

    fs::create_dir(&args.destination).with_context(|| {
        format!(
            "failed to create restore root {}",
            args.destination.display()
        )
    })?;
    let mut restored_bytes = 0_u64;
    for (relative_path, object) in selected {
        let destination = args.destination.join(relative_path);
        let parent = destination
            .parent()
            .ok_or_else(|| anyhow!("restored path has no parent: {}", destination.display()))?;
        fs::create_dir_all(parent)?;
        store
            .restore_object_to_path(object, &destination)
            .with_context(|| format!("failed to restore archive path {relative_path:?}"))?;
        restored_bytes = restored_bytes
            .checked_add(object.size)
            .ok_or_else(|| anyhow!("restored byte count overflow"))?;
    }

    println!("Restored archive manifest: {}", args.manifest.display());
    println!("Restore destination: {}", args.destination.display());
    println!("Files restored: {}", args.paths.len());
    println!("Bytes restored and verified: {restored_bytes}");
    Ok(())
}

#[derive(Debug)]
struct Args {
    cas_root: PathBuf,
    manifest: PathBuf,
    destination: PathBuf,
    paths: Vec<String>,
}

impl Args {
    fn parse() -> Result<Self> {
        let mut cas_root = None;
        let mut manifest = None;
        let mut destination = None;
        let mut paths = Vec::new();
        let mut args = data::cli::read_args(&usage(), true).into_iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--cas-root" => cas_root = args.next().map(PathBuf::from),
                "--manifest" => manifest = args.next().map(PathBuf::from),
                "--destination" => destination = args.next().map(PathBuf::from),
                "--path" => {
                    let path = args
                        .next()
                        .ok_or_else(|| anyhow!("missing value after --path"))?;
                    paths.push(path);
                }
                other => return Err(anyhow!("unknown argument: {other}\n{}", usage().hint())),
            }
        }
        if paths.is_empty() {
            bail!(
                "supply at least one --path <manifest-file-path>\n{}",
                usage().hint()
            );
        }
        paths.sort();
        paths.dedup();
        Ok(Self {
            cas_root: cas_root
                .ok_or_else(|| anyhow!("missing --cas-root <directory>\n{}", usage().hint()))?,
            manifest: manifest
                .ok_or_else(|| anyhow!("missing --manifest <file>\n{}", usage().hint()))?,
            destination: destination.ok_or_else(|| {
                anyhow!("missing --destination <new-directory>\n{}", usage().hint())
            })?,
            paths,
        })
    }
}

fn usage() -> Usage {
    const ARGS: &[ArgDoc] = &[
        ArgDoc::required(
            "--cas-root",
            "<archive-directory>",
            "Archive CAS root that holds the manifest and its objects",
        ),
        ArgDoc::required(
            "--manifest",
            "<archive-manifest.json>",
            "manifest to restore files from",
        ),
        ArgDoc::required(
            "--destination",
            "<new-directory>",
            "output directory; must not already exist",
        ),
        ArgDoc::repeated(
            "--path",
            "<relative-file-path>",
            "manifest-relative regular-file path to restore",
        ),
    ];
    const EXAMPLES: &[&str] = &[
        "cargo run -p data --bin archive_cas_restore -- --cas-root /Volumes/Backup/loadngo-archive-cas --manifest /Volumes/Backup/loadngo-archive-cas/manifests/<archive>.json --destination /tmp/restore-drill --path relative/path/to/one-file",
    ];
    const NOTES: &[&str] = &[
        "Refuses an existing destination root and any existing output file.",
        "Restores selected regular-file paths only; it does not recreate symlinks or reapply timestamps/ownership.",
        "Each output is copied through a temporary sibling, then BLAKE3-checked against its archive object before being published.",
    ];
    Usage {
        bin: "archive_cas_restore",
        invocation: "cargo run -p data --bin archive_cas_restore --",
        about: "restore a deliberately selected set of regular files from an archive manifest into a new directory",
        args: ARGS,
        examples: EXAMPLES,
        notes: NOTES,
    }
}
