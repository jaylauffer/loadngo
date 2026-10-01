use anyhow::{anyhow, bail, Context, Result};
use data::archive_cas::{ArchiveCasStorage, ArchiveEntry, ArchiveObject};
use data::archive_cas_sign::{default_trusted_key, read_public_key};
use data::archive_view::{list_archives, signed_successor};
use data::cli::{ArgDoc, Usage};
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

    // A superseded version retired by a purge has lost the objects no live version
    // lists; under a signed later version those are dropped on purpose, not missing.
    let trusted = match &args.trusted_key {
        Some(path) => Some(read_public_key(path)?),
        None => default_trusted_key()?,
    };
    let listings = list_archives(&args.cas_root, trusted.as_ref())?;
    let root = manifest_object.hash;
    let superseded_by = listings
        .iter()
        .find(|l| l.parents.contains(&root))
        .map(|l| l.root);
    let retired_under = signed_successor(&listings, root);

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
    let (mut dropped, mut dropped_bytes) = (0_u64, 0_u64);
    for (index, object) in objects.iter().enumerate() {
        if retired_under.is_some() && !store.has_object(object.hash) {
            dropped += 1;
            dropped_bytes += object.size;
            continue;
        }
        store.verify_object(*object).with_context(|| {
            let mut context = format!("failed to verify archive object {}", object.hash);
            if let Some(next) = superseded_by {
                context.push_str(&format!(
                    " (this version is superseded by {next}, but no later version is signed by the trusted key)"
                ));
            }
            context
        })?;
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
    // Attachments are part of the version's record: a purge never drops them.
    for attached in &manifest.unverified_history {
        store.verify_object(attached.object).with_context(|| {
            format!(
                "failed to verify attachment {} ({})",
                attached.name, attached.object.hash
            )
        })?;
    }
    println!("Verified archive manifest: {}", args.manifest.display());
    println!("Archive id: {}", manifest.archive_id);
    println!("Archive root object: {}", manifest_object.hash);
    for parent in manifest.parents() {
        println!("Made from: {parent}");
    }
    for record in &manifest.records {
        println!(
            "Change: {} (by {}, at {}: {})",
            record.describe(),
            record.actor,
            record.at_unix_secs,
            record.reason
        );
    }
    if !manifest.unverified_history.is_empty() {
        println!(
            "Attachments verified (unverified history, kept as found): {}",
            manifest.unverified_history.len()
        );
    }
    println!("Files verified: {file_count}");
    println!("Logical file bytes verified: {logical_bytes}");
    println!(
        "Unique objects verified: {}",
        objects.len() as u64 - dropped
    );
    println!("Unique object bytes verified: {unique_object_bytes}");
    if let Some(signed) = retired_under {
        println!(
            "Retired version: superseded; later version {} is {}",
            signed.root,
            signed.signature.describe()
        );
        println!("Objects dropped on retirement: {dropped} ({dropped_bytes} bytes)");
        println!("Blob verification: complete for every object still stored");
    } else {
        println!("Blob verification: complete");
    }
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
    trusted_key: Option<PathBuf>,
}

impl Args {
    fn parse() -> Result<Self> {
        let mut cas_root = None;
        let mut manifest = None;
        let mut trusted_key = None;
        let mut args = data::cli::read_args(&usage(), true).into_iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--cas-root" => cas_root = args.next().map(PathBuf::from),
                "--manifest" => manifest = args.next().map(PathBuf::from),
                "--trusted-public-key" => trusted_key = args.next().map(PathBuf::from),
                other => return Err(anyhow!("unknown argument: {other}\n{}", usage().hint())),
            }
        }
        Ok(Self {
            cas_root: cas_root
                .ok_or_else(|| anyhow!("missing --cas-root <directory>\n{}", usage().hint()))?,
            manifest: manifest
                .ok_or_else(|| anyhow!("missing --manifest <file>\n{}", usage().hint()))?,
            trusted_key,
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
            "manifest to verify, from <cas-root>/manifests/",
        ),
        ArgDoc::optional(
            "--trusted-public-key",
            "<hex-file>",
            "key that decides whether a superseded version was retired under a signed later version (default: the one *.dilithium2.pub in ~/.loadngo/keys)",
        ),
    ];
    const EXAMPLES: &[&str] = &[
        "cargo run -p data --bin archive_cas_verify -- --cas-root /Volumes/Backup/loadngo-archive-cas --manifest /Volumes/Backup/loadngo-archive-cas/manifests/photos-20260920-<hash>.json",
    ];
    const NOTES: &[&str] = &[
        "Re-hashes every distinct object once, so this reads the entire unique archive content from the volume.",
        "Exits non-zero if the manifest records any unreadable source entry, even though every present blob still verifies.",
        "A superseded version that archive_cas_purge retired under a signed later version has lost the objects only it listed: those are counted as dropped on retirement, and every object still stored is verified.",
    ];
    Usage {
        bin: "archive_cas_verify",
        invocation: "cargo run -p data --bin archive_cas_verify --",
        about: "re-hash and confirm every object an archive manifest references, and report whether the capture is complete",
        args: ARGS,
        examples: EXAMPLES,
        notes: NOTES,
    }
}
