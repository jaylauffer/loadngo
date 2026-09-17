use anyhow::{anyhow, bail, Context, Result};
use data::archive_cas::{ArchiveCasStorage, ArchiveEntry, ArchiveObject};
use std::collections::HashMap;
use std::path::PathBuf;

fn main() {
    if let Err(error) = run() {
        eprintln!("archive_cas_verify: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args = Args::parse()?;
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

    let mut file_count = 0_u64;
    let mut logical_bytes = 0_u64;
    let mut object_sizes = HashMap::new();
    let mut objects = Vec::new();
    for entry in &manifest.entries {
        if let ArchiveEntry::File { object, .. } = entry {
            if let Some(previous_size) = object_sizes.insert(object.hash, object.size) {
                if previous_size != object.size {
                    bail!(
                        "archive manifest assigns inconsistent sizes to object {}",
                        object.hash
                    );
                }
            } else {
                objects.push(*object);
            }
            file_count += 1;
            logical_bytes = logical_bytes
                .checked_add(object.size)
                .ok_or_else(|| anyhow!("logical byte count overflow"))?;
        }
    }

    if manifest.file_count() as u64 != file_count {
        bail!("archive manifest file count changed during verification");
    }
    let mut unique_object_bytes = 0_u64;
    for (index, object) in objects.iter().enumerate() {
        store
            .verify_object(*object)
            .with_context(|| format!("failed to verify archive object {}", object.hash))?;
        unique_object_bytes = unique_object_bytes
            .checked_add(object.size)
            .ok_or_else(|| anyhow!("unique object byte count overflow"))?;
        if (index + 1).is_multiple_of(1_000) {
            eprintln!(
                "archive verify progress: objects={} stored_bytes={}",
                index + 1,
                unique_object_bytes
            );
        }
    }
    println!("Verified archive manifest: {}", args.manifest.display());
    println!("Archive id: {}", manifest.archive_id);
    println!("Archive root object: {}", manifest_object.hash);
    println!("Files verified: {file_count}");
    println!("Logical file bytes verified: {logical_bytes}");
    println!("Unique objects verified: {}", objects.len());
    println!("Unique object bytes verified: {unique_object_bytes}");
    println!("Blob verification: complete");
    let unreadable = manifest.unreadable_entry_count();
    if unreadable > 0 {
        bail!(
            "archive capture is incomplete: {unreadable} unreadable source entr{} recorded in the manifest",
            if unreadable == 1 { "y" } else { "ies" }
        );
    }
    let excluded = manifest.excluded_entry_count();
    if excluded > 0 {
        println!("Declared source exclusions: {excluded}");
        println!("Capture completeness: complete within declared scope");
    } else {
        println!("Capture completeness: complete");
    }
    Ok(())
}

#[derive(Debug)]
struct Args {
    cas_root: PathBuf,
    manifest: PathBuf,
}

impl Args {
    fn parse() -> Result<Self> {
        let mut cas_root = None;
        let mut manifest = None;
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--cas-root" => cas_root = args.next().map(PathBuf::from),
                "--manifest" => manifest = args.next().map(PathBuf::from),
                "--help" | "-h" => {
                    print_usage();
                    std::process::exit(0);
                }
                other => return Err(anyhow!("unknown argument: {other}")),
            }
        }
        Ok(Self {
            cas_root: cas_root.ok_or_else(|| anyhow!("missing --cas-root <directory>"))?,
            manifest: manifest.ok_or_else(|| anyhow!("missing --manifest <file>"))?,
        })
    }
}

fn print_usage() {
    eprintln!(
        "Usage: cargo run -p data --bin archive_cas_verify -- --cas-root <archive-directory> --manifest <archive-manifest.json>"
    );
}
