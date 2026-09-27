//! Combines several archives in one Archive CAS root into a single archive, each under
//! its own folder. Only a new manifest and a merge log are written: every object is
//! already in the root and is referenced as it is. The source archives are left
//! exactly as they were; retire them afterwards (the Archive CAS browser's Delete
//! archive keeps every object the merged archive still lists).

use anyhow::{anyhow, bail, Context, Result};
use data::archive_cas::{
    ArchiveCasStorage, ArchiveManifest, ArchiveMergeLog, ArchiveMergeSource,
    ARCHIVE_MERGE_LOG_FORMAT_V1,
};
use data::archive_view::list_archives;
use data::cli::{ArgDoc, Usage};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    if let Err(error) = run() {
        eprintln!("archive_cas_merge: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args = Args::parse()?;
    let store = ArchiveCasStorage::new(&args.cas_root)?;
    let listings = list_archives(&args.cas_root, None)?;
    if listings.iter().any(|l| l.archive_id == args.archive_id) {
        bail!(
            "archive {:?} already exists in {}",
            args.archive_id,
            args.cas_root.display()
        );
    }

    let mut manifests = Vec::new();
    for (id, under) in &args.sources {
        let current: Vec<_> = listings
            .iter()
            .filter(|l| &l.archive_id == id && !l.superseded)
            .collect();
        let listing = match current.as_slice() {
            [one] => *one,
            [] => bail!("no archive {id:?} in {}", args.cas_root.display()),
            _ => bail!("archive {id:?} has more than one current manifest; resolve that first"),
        };
        eprintln!("reading {id} ({})", listing.manifest_path.display());
        let (manifest, root) = store.read_manifest_and_root(&listing.manifest_path)?;
        manifests.push((under.clone(), manifest, root));
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_secs();
    let sources: Vec<(&str, &ArchiveManifest)> = manifests
        .iter()
        .map(|(under, manifest, _)| (under.as_str(), manifest))
        .collect();
    let merged = ArchiveManifest::merged(&args.archive_id, &args.label, now, &sources)?;
    let (path, root) = store.write_manifest(&merged)?;
    let log = ArchiveMergeLog {
        format: ARCHIVE_MERGE_LOG_FORMAT_V1.to_string(),
        archive_id: args.archive_id.clone(),
        merged_manifest_root: root.hash,
        merged_at_unix_secs: now,
        actor: args.actor,
        reason: args.reason,
        sources: manifests
            .iter()
            .map(|(under, manifest, root)| ArchiveMergeSource {
                archive_id: manifest.archive_id.clone(),
                source_label: manifest.source_label.clone(),
                manifest_root: *root,
                under: under.clone(),
            })
            .collect(),
    };
    let log_path = store.write_merge_log(&path, &log)?;

    println!("Merged archive {} ({})", args.archive_id, args.label);
    println!("Manifest: {}", path.display());
    println!("Root: {}", root.hash);
    println!("Merge log: {}", log_path.display());
    for (under, manifest, root) in &manifests {
        println!(
            "  {under}/  <- {} ({} files, root {})",
            manifest.archive_id,
            manifest.file_count(),
            &root.to_hex()[..12]
        );
    }
    println!("Files in the merged archive: {}", merged.file_count());
    println!();
    println!("The source archives are unchanged. Next:");
    println!("  1. sign the merged manifest with archive_cas_sign (unsigned until then);");
    println!("  2. retire each source archive with the browser's Delete archive: it keeps every object the merged archive lists.");
    Ok(())
}

#[derive(Debug)]
struct Args {
    cas_root: PathBuf,
    archive_id: String,
    label: String,
    sources: Vec<(String, String)>,
    reason: String,
    actor: String,
}

impl Args {
    fn parse() -> Result<Self> {
        let (mut cas_root, mut archive_id, mut label, mut reason, mut actor) =
            (None, None, None, None, None);
        let mut sources = Vec::new();
        let mut args = data::cli::read_args(&usage(), true).into_iter();
        while let Some(arg) = args.next() {
            let mut value = || args.next().ok_or_else(|| anyhow!("{arg} requires a value"));
            match arg.as_str() {
                "--cas-root" => cas_root = Some(PathBuf::from(value()?)),
                "--archive-id" => archive_id = Some(value()?),
                "--label" => label = Some(value()?),
                "--from" => {
                    let pair = value()?;
                    let (id, under) = pair.split_once('=').ok_or_else(|| {
                        anyhow!("--from takes <archive-id>=<folder>, got {pair:?}")
                    })?;
                    sources.push((id.to_string(), under.to_string()));
                }
                "--reason" => reason = Some(value()?),
                "--actor" => actor = Some(value()?),
                other => bail!("unknown argument: {other}\n{}", usage().hint()),
            }
        }
        if sources.len() < 2 {
            bail!(
                "give at least two --from <archive-id>=<folder>\n{}",
                usage().hint()
            );
        }
        let missing = |name: &str| anyhow!("missing {name}\n{}", usage().hint());
        Ok(Self {
            cas_root: cas_root.ok_or_else(|| missing("--cas-root"))?,
            archive_id: archive_id.ok_or_else(|| missing("--archive-id"))?,
            label: label.ok_or_else(|| missing("--label"))?,
            sources,
            reason: reason.ok_or_else(|| missing("--reason"))?,
            actor: actor.ok_or_else(|| missing("--actor"))?,
        })
    }
}

fn usage() -> Usage {
    const ARGS: &[ArgDoc] = &[
        ArgDoc::required(
            "--cas-root",
            "<archive-directory>",
            "Archive CAS root holding the archives",
        ),
        ArgDoc::required(
            "--archive-id",
            "<new-id>",
            "id of the merged archive (lowercase, digits, hyphens); must not exist yet",
        ),
        ArgDoc::required(
            "--label",
            "<label>",
            "human-readable label of the merged archive",
        ),
        ArgDoc::repeated(
            "--from",
            "<archive-id>=<folder>",
            "an archive to include, and the folder its contents go under",
        ),
        ArgDoc::required("--reason", "<why>", "recorded in the merge log"),
        ArgDoc::required("--actor", "<who>", "recorded in the merge log"),
    ];
    const EXAMPLES: &[&str] = &[
        "cargo run -p data --bin archive_cas_merge -- --cas-root \"/Volumes/Loadngo Archive Staging/loadngo-archive-cas\" --archive-id loadngo-archive --label \"Loadngo Archive\" --from untitled-documents-20260917=Untitled/Documents --from zhoenus-ii-20260915=\"Zhoenus II\" --reason \"one archive per drive\" --actor jay",
    ];
    const NOTES: &[&str] = &[
        "Writes one new manifest and a merge log; copies no data and changes no source archive.",
        "Each source's current (not superseded) manifest is used.",
        "The merged manifest is unsigned; sign it with archive_cas_sign.",
    ];
    Usage {
        bin: "archive_cas_merge",
        invocation: "cargo run -p data --bin archive_cas_merge --",
        about: "combine archives in one root into a single archive, each under its own folder",
        args: ARGS,
        examples: EXAMPLES,
        notes: NOTES,
    }
}
