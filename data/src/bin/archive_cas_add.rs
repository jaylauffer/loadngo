//! Adds files or folders to an existing archive, producing a new manifest that
//! supersedes the old one and an add-log sidecar explaining the change. The new files
//! are stored (and hashed) first; the old manifest, its signature and every other file
//! are left untouched. The counterpart of [`archive_cas_remove`].

use anyhow::{anyhow, bail, Context, Result};
use data::archive_cas::{ArchiveCasStorage, ArchiveEntry, ArchiveManifest, ArchiveObject};
use data::cli::{ArgDoc, Usage};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    if let Err(error) = run() {
        eprintln!("archive_cas_add: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args = Args::parse()?;
    let store = ArchiveCasStorage::new(&args.cas_root)?;
    let manifest_path = match (&args.manifest, &args.archive) {
        (Some(path), None) => path.clone(),
        (None, Some(id)) => newest_manifest(&store, id)?,
        _ => bail!(
            "give exactly one of --manifest or --archive\n{}",
            usage().hint()
        ),
    };
    let manifest = store.read_manifest(&manifest_path)?;
    let manifest_bytes = manifest.canonical_bytes()?;
    let previous_root = ArchiveObject {
        hash: manifest.digest()?,
        size: u64::try_from(manifest_bytes.len()).context("manifest length exceeds u64")?,
    };
    store
        .verify_object(previous_root)
        .context("source manifest is not present as a verified CAS object")?;
    println!(
        "Adding to {} ({})",
        manifest.archive_id,
        manifest_path.display()
    );

    let mut added = Vec::new();
    let mut bytes = 0u64;
    for source in &args.sources {
        let name = source
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| anyhow!("{} has no usable name", source.display()))?;
        let at = match args.under.as_deref() {
            Some(under) => format!("{}/{name}", under.trim_matches('/')),
            None => name.to_string(),
        };
        capture(&store, source, &at, &mut added, &mut bytes)?;
    }

    let (amended, log) =
        manifest.with_entries_added(added, args.reason, args.actor, unix_now()?)?;
    let (new_manifest, archive_root) = store.write_manifest(&amended)?;
    let log_path = store.write_add_log(&new_manifest, &log)?;

    println!("Superseded archive root: {}", previous_root.hash);
    println!("New archive manifest: {}", new_manifest.display());
    println!("New archive root object: {}", archive_root.hash);
    println!("Add log: {}", log_path.display());
    println!(
        "Entries added: {} ({bytes} bytes of files)",
        log.added_paths.len()
    );
    for path in &log.added_paths {
        println!("  + {path}");
    }
    println!();
    println!("This manifest is not yet signed. Next steps:");
    println!(
        "  1. archive_cas_verify --cas-root {} --manifest {}",
        args.cas_root.display(),
        new_manifest.display()
    );
    println!(
        "  2. archive_cas_sign sign --cas-root {} --manifest {} --signer-identity <you> --public-key <pub> --private-key <priv>",
        args.cas_root.display(),
        new_manifest.display()
    );
    println!("  3. Once you're satisfied, archive_cas_prune_manifests to retire the old manifest.");
    Ok(())
}

/// The newest manifest (by creation time) of archive `id` under the root.
fn newest_manifest(store: &ArchiveCasStorage, id: &str) -> Result<PathBuf> {
    let prefix = format!("{id}-");
    let mut newest: Option<(u64, PathBuf)> = None;
    for path in store.list_manifests()? {
        let is_candidate = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with(&prefix));
        if !is_candidate {
            continue;
        }
        let manifest: ArchiveManifest = store.read_manifest(&path)?;
        if manifest.archive_id == id
            && newest
                .as_ref()
                .is_none_or(|(at, _)| manifest.created_at_unix_secs > *at)
        {
            newest = Some((manifest.created_at_unix_secs, path));
        }
    }
    newest
        .map(|(_, path)| path)
        .ok_or_else(|| anyhow!("no manifest for archive {id:?} under this root"))
}

/// Stores `source` (a file, folder or symlink) at archive path `at`, recursively.
fn capture(
    store: &ArchiveCasStorage,
    source: &Path,
    at: &str,
    added: &mut Vec<ArchiveEntry>,
    bytes: &mut u64,
) -> Result<()> {
    let metadata = fs::symlink_metadata(source)
        .with_context(|| format!("failed to stat {}", source.display()))?;
    let modified = metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs());
    let kind = metadata.file_type();
    if kind.is_dir() {
        added.push(ArchiveEntry::Directory {
            path: at.to_string(),
            modified_at_unix_secs: modified,
        });
        let mut children = fs::read_dir(source)
            .with_context(|| format!("failed to read {}", source.display()))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        children.sort_by_key(fs::DirEntry::file_name);
        for child in children {
            let name = child.file_name();
            let name = name
                .to_str()
                .ok_or_else(|| anyhow!("{} is not valid UTF-8", child.path().display()))?;
            capture(store, &child.path(), &format!("{at}/{name}"), added, bytes)?;
        }
    } else if kind.is_file() {
        let key = source
            .canonicalize()
            .unwrap_or_else(|_| source.to_path_buf())
            .to_string_lossy()
            .into_owned();
        let ingest = store
            .ingest_file(source, &key)
            .with_context(|| format!("failed to store {}", source.display()))?;
        *bytes += ingest.object.size;
        println!(
            "  stored {at} ({} bytes{})",
            ingest.object.size,
            if ingest.inserted {
                ""
            } else {
                ", already in the archive root"
            }
        );
        added.push(ArchiveEntry::File {
            path: at.to_string(),
            object: ingest.object,
            modified_at_unix_secs: modified,
        });
    } else if kind.is_symlink() {
        let target = fs::read_link(source)
            .with_context(|| format!("failed to read link {}", source.display()))?;
        added.push(ArchiveEntry::Symlink {
            path: at.to_string(),
            target: target.to_string_lossy().into_owned(),
        });
    } else {
        bail!("unsupported special file: {}", source.display());
    }
    Ok(())
}

#[derive(Debug)]
struct Args {
    cas_root: PathBuf,
    manifest: Option<PathBuf>,
    archive: Option<String>,
    sources: Vec<PathBuf>,
    under: Option<String>,
    reason: String,
    actor: String,
}

impl Args {
    fn parse() -> Result<Self> {
        let mut cas_root = None;
        let mut manifest = None;
        let mut archive = None;
        let mut sources = Vec::new();
        let mut under = None;
        let mut reason = None;
        let mut actor = None;
        let mut args = data::cli::read_args(&usage(), true).into_iter();
        while let Some(arg) = args.next() {
            let mut value = || args.next().ok_or_else(|| anyhow!("{arg} requires a value"));
            match arg.as_str() {
                "--cas-root" => cas_root = Some(PathBuf::from(value()?)),
                "--manifest" => manifest = Some(PathBuf::from(value()?)),
                "--archive" => archive = Some(value()?),
                "--add" => sources.push(PathBuf::from(value()?)),
                "--under" => under = Some(value()?),
                "--reason" => reason = Some(value()?),
                "--actor" => actor = Some(value()?),
                other => return Err(anyhow!("unknown argument: {other}\n{}", usage().hint())),
            }
        }
        if sources.is_empty() {
            bail!("at least one --add is required\n{}", usage().hint());
        }
        let reason =
            reason.ok_or_else(|| anyhow!("missing --reason <text>\n{}", usage().hint()))?;
        let actor = actor.ok_or_else(|| anyhow!("missing --actor <name>\n{}", usage().hint()))?;
        if reason.trim().is_empty() || actor.trim().is_empty() {
            bail!("--reason and --actor must not be empty");
        }
        Ok(Self {
            cas_root: cas_root.ok_or_else(|| {
                anyhow!("missing --cas-root <archive-directory>\n{}", usage().hint())
            })?,
            manifest,
            archive,
            sources,
            under,
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
            "Archive CAS root that holds the archive",
        ),
        ArgDoc::optional(
            "--archive",
            "<archive-id>",
            "archive to add to; its newest manifest is used (give this or --manifest)",
        ),
        ArgDoc::optional(
            "--manifest",
            "<archive-manifest.json>",
            "exact manifest to add to (give this or --archive)",
        ),
        ArgDoc::repeated(
            "--add",
            "<file-or-folder>",
            "file or folder to add; a folder is added with everything in it",
        ),
        ArgDoc::optional(
            "--under",
            "<archive-path>",
            "folder inside the archive to put the additions in (default: the top level)",
        ),
        ArgDoc::required(
            "--reason",
            "<why>",
            "non-empty reason recorded in the add log",
        ),
        ArgDoc::required(
            "--actor",
            "<who>",
            "non-empty name of the person or agent adding these files",
        ),
    ];
    const EXAMPLES: &[&str] = &[
        "cargo run -p data --bin archive_cas_add -- --cas-root \"/Volumes/Loadngo Archive Staging/loadngo-archive-cas\" --archive zhoenus-ii-20260915 --add ~/Downloads/export.zip --under added/2026-09-27 --reason \"LinkedIn export\" --actor jay",
    ];
    const NOTES: &[&str] = &[
        "Stores the new files first, then writes a new, superseding manifest plus an add-log sidecar; the old manifest and its signature are untouched.",
        "A path already in the archive is refused; remove it first with archive_cas_remove to replace it.",
        "The new manifest is unsigned; verify and sign it with archive_cas_verify and archive_cas_sign.",
    ];
    Usage {
        bin: "archive_cas_add",
        invocation: "cargo run -p data --bin archive_cas_add --",
        about: "add files or folders to an archive, producing a new manifest that supersedes the old one",
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
