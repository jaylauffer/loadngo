use anyhow::{anyhow, bail, Context, Result};
use data::archive_cas::{is_build_cache, ArchiveCasStorage, ArchiveEntry, ArchiveManifest};
use data::archive_cas_unpack::{has_zips, unpack_zips, UnpackProgress, DEFAULT_MAX_DEPTH};
use data::cli::{ArgDoc, Usage};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const ARCHIVE_INGEST_JOURNAL_FORMAT_V1: &str = "loadngo-archive-ingest-journal-v1";

fn main() {
    if let Err(error) = run() {
        eprintln!("archive_cas_ingest: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args = Args::parse()?;
    let source_metadata = fs::symlink_metadata(&args.source)
        .with_context(|| format!("failed to stat source {}", args.source.display()))?;
    if !source_metadata.is_dir() {
        bail!("--source must name a directory: {}", args.source.display());
    }

    let store = ArchiveCasStorage::new(&args.cas_root)?;
    let mut journal = IngestJournal::open(&args.cas_root, &args.archive_id, &args.source_label)?;
    let mut entries = journal.entries.clone();
    let mut stats = IngestStats::default();
    capture_directory(
        &store,
        &mut journal,
        &args.source,
        Path::new(""),
        &mut entries,
        &mut stats,
    )?;
    // A journal from an earlier run may hold files from a directory now skipped.
    drop_skipped(&mut entries, &stats.skipped_caches);

    let manifest = ArchiveManifest::new(
        args.archive_id,
        args.source_label,
        unix_now()?,
        entries.into_values().collect(),
    )?;
    let (manifest_path, manifest_object) = store.write_manifest(&manifest)?;
    let captured_complete = manifest.is_complete();
    let unpacked = if args.keep_zips || !has_zips(&manifest) {
        None
    } else {
        // Zips are not kept as opaque files: their members are stored and deduplicated
        // like any other file, in a version that supersedes the capture.
        eprintln!("unpacking zips (.zip, .ipa, .jar, .apk)...");
        let (unpacked, log) = unpack_zips(
            &store,
            &manifest,
            manifest_object.hash,
            DEFAULT_MAX_DEPTH,
            unix_now()?,
            "archive_cas_ingest",
            "zips unpacked at ingest",
            |progress| match progress {
                UnpackProgress::Zip { path, members, .. } => {
                    eprintln!("  unpacked {path}: {members} members");
                }
                UnpackProgress::Skipped { path, reason } => {
                    eprintln!("  left whole {path}: {reason}");
                }
            },
        )?;
        let (path, object) = store.write_manifest(&unpacked)?;
        let log_path = store.write_sidecar(&path, "unpack-log", &log)?;
        Some((path, object, log_path, log))
    };

    println!("Archive CAS root: {}", store.root().display());
    println!("Ingest journal: {}", journal.path.display());
    println!("Archive manifest: {}", manifest_path.display());
    println!("Archive root object: {}", manifest_object.hash);
    println!("Archive root size: {}", manifest_object.size);
    println!("Files declared: {}", stats.files);
    println!("Directories declared: {}", stats.directories);
    println!(
        "Empty directories skipped: {} (put a .keep file in one to keep it)",
        stats.skipped_empty_directories
    );
    println!(
        "Build caches skipped (CACHEDIR.TAG): {}",
        stats.skipped_caches.len()
    );
    for cache in &stats.skipped_caches {
        println!("  {cache}");
    }
    println!("Symlinks declared: {}", stats.symlinks);
    println!("Unreadable source entries: {}", stats.unreadable_entries);
    println!("Capture complete: {captured_complete}");
    println!("Logical file bytes: {}", stats.logical_bytes);
    println!("New archive objects: {}", stats.inserted_objects);
    println!("Deduplicated files: {}", stats.deduplicated_files);
    println!("Journal-reused files: {}", stats.journal_reused_files);
    println!("Resumed source bytes: {}", stats.resumed_bytes);
    if let Some((path, object, log_path, log)) = unpacked {
        println!(
            "Zips unpacked: {} ({} left whole)",
            log.unpacked.len(),
            log.skipped.len()
        );
        println!(
            "Zip members newly stored: {} ({} bytes); already stored: {}",
            log.new_objects, log.new_object_bytes, log.reused_objects
        );
        println!("Current manifest (zips unpacked): {}", path.display());
        println!("Current root object: {}", object.hash);
        println!("Unpack log: {}", log_path.display());
        println!(
            "The capture above is superseded; purging it frees the zips' own bytes \
             (archive_cas_purge, or the browser's Purge drive)."
        );
    }
    Ok(())
}

#[derive(Debug)]
struct Args {
    source: PathBuf,
    cas_root: PathBuf,
    archive_id: String,
    source_label: String,
    keep_zips: bool,
}

impl Args {
    fn parse() -> Result<Self> {
        let mut source = None;
        let mut cas_root = None;
        let mut archive_id = None;
        let mut source_label = None;
        let mut keep_zips = false;
        let args = data::cli::read_args(&usage(), true);
        let mut args = args.into_iter();

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--source" => source = args.next().map(PathBuf::from),
                "--cas-root" => cas_root = args.next().map(PathBuf::from),
                "--archive-id" => archive_id = args.next(),
                "--source-label" => source_label = args.next(),
                "--keep-zips" => keep_zips = true,
                other => return Err(anyhow!("unknown argument: {other}\n{}", usage().hint())),
            }
        }

        let archive_id =
            archive_id.ok_or_else(|| anyhow!("missing --archive-id <id>\n{}", usage().hint()))?;
        Ok(Self {
            source: source
                .ok_or_else(|| anyhow!("missing --source <directory>\n{}", usage().hint()))?,
            cas_root: cas_root
                .ok_or_else(|| anyhow!("missing --cas-root <directory>\n{}", usage().hint()))?,
            source_label: source_label.unwrap_or_else(|| archive_id.clone()),
            archive_id,
            keep_zips,
        })
    }
}

fn usage() -> Usage {
    const ARGS: &[ArgDoc] = &[
        ArgDoc::required(
            "--source",
            "<read-only-directory>",
            "directory to capture; never modified",
        ),
        ArgDoc::required(
            "--cas-root",
            "<archive-directory>",
            "Archive CAS root to write into (created if it does not exist)",
        ),
        ArgDoc::required(
            "--archive-id",
            "<lowercase-id>",
            "stable id for this archive; rerun with the same id to resume an interrupted capture",
        ),
        ArgDoc::optional(
            "--source-label",
            "<label>",
            "human-readable label stored in the manifest; defaults to --archive-id",
        ),
        ArgDoc::switch(
            "--keep-zips",
            "store zips as single files instead of unpacking them (the default unpacks)",
        ),
    ];
    const EXAMPLES: &[&str] = &[
        "cargo run -p data --bin archive_cas_ingest -- --source /Volumes/Old/Photos --cas-root /Volumes/Backup/loadngo-archive-cas --archive-id photos-20260920 --source-label \"Old external photo drive\"",
    ];
    const NOTES: &[&str] = &[
        "Prints the manifest path and archive-root hash on success.",
        "If interrupted, rerun the identical command; matching partial files resume after prefix verification.",
        "Run archive_cas_verify afterward to confirm every blob is present and re-hashes correctly.",
        "Zips (.zip, .ipa, .jar, .apk, and zips inside them) are unpacked into folders of their members, as archive_cas_unpack does, in a version that supersedes the capture; purging the capture frees the zips' bytes. Office documents stay whole.",
        "A root with compression on (archive_cas_compress --enable) stores new objects compressed.",
    ];
    Usage {
        bin: "archive_cas_ingest",
        invocation: "cargo run -p data --bin archive_cas_ingest --",
        about: "capture a read-only source directory into an Archive CAS root, resuming a prior interrupted run by archive id",
        args: ARGS,
        examples: EXAMPLES,
        notes: NOTES,
    }
}

#[derive(Debug, Default)]
struct IngestStats {
    files: u64,
    directories: u64,
    symlinks: u64,
    unreadable_entries: u64,
    logical_bytes: u64,
    inserted_objects: u64,
    deduplicated_files: u64,
    journal_reused_files: u64,
    resumed_bytes: u64,
    /// Directories with nothing stored below them, not recorded.
    skipped_empty_directories: u64,
    /// Directories holding a `CACHEDIR.TAG`, not captured.
    skipped_caches: Vec<String>,
}

impl IngestStats {
    fn record_file(&mut self, object_size: u64) -> Result<()> {
        self.files += 1;
        self.logical_bytes = self
            .logical_bytes
            .checked_add(object_size)
            .ok_or_else(|| anyhow!("logical byte count overflow"))?;
        Ok(())
    }

    fn report_progress(&self) {
        if self.files > 0 && self.files.is_multiple_of(1_000) {
            eprintln!(
                "archive progress: files={} logical_bytes={} new_objects={} journal_reused={}",
                self.files, self.logical_bytes, self.inserted_objects, self.journal_reused_files
            );
        }
    }
}

/// Removes entries below any of `skipped` directories.
fn drop_skipped(entries: &mut BTreeMap<String, ArchiveEntry>, skipped: &[String]) {
    for directory in skipped {
        let below = format!("{directory}/");
        entries.retain(|path, _| path != directory && !path.starts_with(&below));
    }
}

/// Captures what `absolute_directory` holds into `entries`. A directory is recorded only
/// when something is stored below it, so empty directories leave no entry (a `.keep`
/// file keeps one); a build cache (a valid `CACHEDIR.TAG`) is skipped whole. Returns whether
/// anything was stored.
fn capture_directory(
    store: &ArchiveCasStorage,
    journal: &mut IngestJournal,
    absolute_directory: &Path,
    relative_directory: &Path,
    entries: &mut BTreeMap<String, ArchiveEntry>,
    stats: &mut IngestStats,
) -> Result<bool> {
    let mut stored = false;
    let mut children = fs::read_dir(absolute_directory)
        .with_context(|| format!("failed to read {}", absolute_directory.display()))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("failed to enumerate {}", absolute_directory.display()))?;
    children.sort_by_key(|entry| entry.file_name());

    for child in children {
        let file_name = child.file_name();
        let absolute_path = child.path();
        let relative_path = relative_directory.join(file_name);
        let portable_path = portable_relative_path(&relative_path)?;
        let metadata = match fs::symlink_metadata(&absolute_path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let entry = ArchiveEntry::Unreadable {
                    path: portable_path.clone(),
                    operation: "stat".to_string(),
                    error: error.to_string(),
                };
                eprintln!(
                    "archive incomplete: unable to stat source entry {}; recorded in manifest",
                    absolute_path.display()
                );
                entries.insert(portable_path, entry);
                stats.unreadable_entries += 1;
                stored = true;
                continue;
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to stat {}", absolute_path.display()))
            }
        };
        let file_type = metadata.file_type();

        if file_type.is_dir() {
            if is_build_cache(&absolute_path) {
                eprintln!("skipped build cache (CACHEDIR.TAG): {portable_path}");
                stats.skipped_caches.push(portable_path);
                continue;
            }
            if capture_directory(
                store,
                journal,
                &absolute_path,
                &relative_path,
                entries,
                stats,
            )? {
                entries.insert(
                    portable_path.clone(),
                    ArchiveEntry::Directory {
                        path: portable_path,
                        modified_at_unix_secs: modified_at_unix_secs(&metadata),
                    },
                );
                stats.directories += 1;
                stored = true;
            } else {
                eprintln!("skipped empty directory: {portable_path}");
                stats.skipped_empty_directories += 1;
            }
        } else if file_type.is_file() {
            stored = true;
            let modified_at_unix_secs = modified_at_unix_secs(&metadata);
            if let Some(object) = journal_reusable_object(
                entries.get(&portable_path),
                metadata.len(),
                modified_at_unix_secs,
                store,
            )? {
                stats.record_file(object.size)?;
                stats.journal_reused_files += 1;
                stats.report_progress();
                continue;
            }
            let ingest = store
                .ingest_file(&absolute_path, &portable_path)
                .with_context(|| format!("failed to ingest {}", absolute_path.display()))?;
            let entry = ArchiveEntry::File {
                path: portable_path,
                object: ingest.object,
                modified_at_unix_secs,
            };
            journal.record(&entry)?;
            entries.insert(entry.path().to_string(), entry);
            stats.record_file(ingest.object.size)?;
            stats.resumed_bytes = stats
                .resumed_bytes
                .checked_add(ingest.resumed_bytes)
                .ok_or_else(|| anyhow!("resumed byte count overflow"))?;
            if ingest.inserted {
                stats.inserted_objects += 1;
            } else {
                stats.deduplicated_files += 1;
            }
            stats.report_progress();
        } else if file_type.is_symlink() {
            let target = fs::read_link(&absolute_path)
                .with_context(|| format!("failed to read link {}", absolute_path.display()))?;
            entries.insert(
                portable_path.clone(),
                ArchiveEntry::Symlink {
                    path: portable_path,
                    target: target.to_string_lossy().into_owned(),
                },
            );
            stats.symlinks += 1;
            stored = true;
        } else {
            bail!(
                "unsupported special file in source: {}",
                absolute_path.display()
            );
        }
    }
    Ok(stored)
}

fn journal_reusable_object(
    entry: Option<&ArchiveEntry>,
    source_size: u64,
    source_modified_at_unix_secs: Option<u64>,
    store: &ArchiveCasStorage,
) -> Result<Option<data::archive_cas::ArchiveObject>> {
    let Some(ArchiveEntry::File {
        object,
        modified_at_unix_secs,
        ..
    }) = entry
    else {
        return Ok(None);
    };
    if object.size != source_size || *modified_at_unix_secs != source_modified_at_unix_secs {
        return Ok(None);
    }
    let object_path = store.object_path(object.hash);
    match fs::metadata(&object_path) {
        Ok(metadata) if metadata.len() == object.size => Ok(Some(*object)),
        Ok(_) => Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| {
            format!(
                "failed to stat journaled archive object {}",
                object_path.display()
            )
        }),
    }
}

fn portable_relative_path(path: &Path) -> Result<String> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        bail!(
            "archive entry path must be non-empty and relative: {}",
            path.display()
        );
    }
    let path = path
        .to_str()
        .ok_or_else(|| anyhow!("archive path is not valid UTF-8: {}", path.display()))?;
    #[cfg(target_os = "windows")]
    {
        Ok(path.replace('\\', "/"))
    }
    #[cfg(not(target_os = "windows"))]
    {
        Ok(path.to_string())
    }
}

fn modified_at_unix_secs(metadata: &fs::Metadata) -> Option<u64> {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs())
}

fn unix_now() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_secs())
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum IngestJournalLine {
    Header {
        format: String,
        archive_id: String,
        source_label: String,
    },
    File {
        entry: ArchiveEntry,
    },
}

struct IngestJournal {
    path: PathBuf,
    file: File,
    entries: BTreeMap<String, ArchiveEntry>,
}

impl IngestJournal {
    fn open(cas_root: &Path, archive_id: &str, source_label: &str) -> Result<Self> {
        let directory = cas_root.join("ingest");
        fs::create_dir_all(&directory).with_context(|| {
            format!(
                "failed to create ingest journal directory {}",
                directory.display()
            )
        })?;
        let path = directory.join(format!("{archive_id}.jsonl"));
        let entries = if path.exists() {
            Self::read_existing(&path, archive_id, source_label)?
        } else {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&path)
                .with_context(|| format!("failed to create ingest journal {}", path.display()))?;
            Self::write_line(
                &mut file,
                &IngestJournalLine::Header {
                    format: ARCHIVE_INGEST_JOURNAL_FORMAT_V1.to_string(),
                    archive_id: archive_id.to_string(),
                    source_label: source_label.to_string(),
                },
            )?;
            BTreeMap::new()
        };
        let file = OpenOptions::new()
            .append(true)
            .open(&path)
            .with_context(|| format!("failed to reopen ingest journal {}", path.display()))?;
        Ok(Self {
            path,
            file,
            entries,
        })
    }

    fn record(&mut self, entry: &ArchiveEntry) -> Result<()> {
        if !matches!(entry, ArchiveEntry::File { .. }) {
            bail!("only regular file entries may be recorded in the ingest journal");
        }
        if self.entries.get(entry.path()) == Some(entry) {
            return Ok(());
        }
        Self::write_line(
            &mut self.file,
            &IngestJournalLine::File {
                entry: entry.clone(),
            },
        )?;
        self.entries.insert(entry.path().to_string(), entry.clone());
        Ok(())
    }

    fn read_existing(
        path: &Path,
        expected_archive_id: &str,
        expected_source_label: &str,
    ) -> Result<BTreeMap<String, ArchiveEntry>> {
        let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
        let mut lines = bytes
            .split_inclusive(|byte| *byte == b'\n')
            .filter_map(|line| line.strip_suffix(b"\n"));
        let Some(header_line) = lines.next() else {
            bail!("ingest journal has no complete header: {}", path.display());
        };
        let header: IngestJournalLine = serde_json::from_slice(header_line)
            .with_context(|| format!("failed to parse ingest journal header {}", path.display()))?;
        match header {
            IngestJournalLine::Header {
                format,
                archive_id,
                source_label,
            } if format == ARCHIVE_INGEST_JOURNAL_FORMAT_V1
                && archive_id == expected_archive_id
                && source_label == expected_source_label => {}
            IngestJournalLine::Header { .. } => bail!(
                "ingest journal belongs to a different archive or source label: {}",
                path.display()
            ),
            IngestJournalLine::File { .. } => {
                bail!("ingest journal is missing its header: {}", path.display())
            }
        }

        let mut entries = BTreeMap::new();
        for line in lines {
            let journal_line: IngestJournalLine = serde_json::from_slice(line)
                .with_context(|| format!("failed to parse ingest journal {}", path.display()))?;
            let IngestJournalLine::File { entry } = journal_line else {
                bail!(
                    "ingest journal contains a second header: {}",
                    path.display()
                );
            };
            if !matches!(entry, ArchiveEntry::File { .. }) {
                bail!(
                    "ingest journal contains a non-file entry: {}",
                    path.display()
                );
            }
            ArchiveManifest::new(
                expected_archive_id.to_string(),
                expected_source_label.to_string(),
                0,
                vec![entry.clone()],
            )
            .with_context(|| {
                format!(
                    "ingest journal contains an invalid file entry: {}",
                    path.display()
                )
            })?;
            entries.insert(entry.path().to_string(), entry);
        }
        Ok(entries)
    }

    fn write_line(file: &mut File, line: &IngestJournalLine) -> Result<()> {
        let bytes =
            serde_json::to_vec(line).context("failed to serialize ingest journal record")?;
        file.write_all(&bytes)?;
        file.write_all(b"\n")?;
        file.sync_data()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use data::archive_cas::ArchiveObject;
    use data::cas::CasHash;
    use tempfile::tempdir;

    #[test]
    fn ingest_journal_reopens_a_completed_file_record() {
        let directory = tempdir().unwrap();
        let entry = ArchiveEntry::File {
            path: "captured/file.bin".to_string(),
            object: ArchiveObject {
                hash: CasHash::digest(b"journal"),
                size: 7,
            },
            modified_at_unix_secs: Some(1),
        };
        let mut journal = IngestJournal::open(directory.path(), "archive-1", "source").unwrap();
        journal.record(&entry).unwrap();
        drop(journal);

        let reopened = IngestJournal::open(directory.path(), "archive-1", "source").unwrap();
        assert_eq!(reopened.entries.get(entry.path()), Some(&entry));
    }

    #[test]
    fn empty_directories_and_build_caches_are_not_captured() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("source");
        let tag = "Signature: 8a477f597d28d172789f06886806bc55\n# a cache\n";
        for (path, bytes) in [
            ("src/main.rs", "fn main() {}"),
            ("keep/.keep", ""),
            ("target/CACHEDIR.TAG", tag),
            ("target/debug/app", "binary"),
            ("app/target-android-build-std/CACHEDIR.TAG", tag),
            ("app/target-android-build-std/out.o", "object"),
            ("app/notes.md", "notes"),
            ("lookalike/CACHEDIR.TAG", "not a tag"),
            ("lookalike/data.txt", "data"),
        ] {
            let path = source.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }
        for empty in ["empty", "nested/empty/deeper", "app/build/out"] {
            fs::create_dir_all(source.join(empty)).unwrap();
        }
        let cas = directory.path().join("cas");
        let store = ArchiveCasStorage::new(&cas).unwrap();
        let mut journal = IngestJournal::open(&cas, "test", "test").unwrap();
        // A file an earlier run journaled inside what is now a skipped cache.
        let stale = ArchiveEntry::File {
            path: "target/debug/old".to_string(),
            object: ArchiveObject {
                hash: CasHash::digest(b"old"),
                size: 3,
            },
            modified_at_unix_secs: None,
        };
        let mut entries = BTreeMap::from([(stale.path().to_string(), stale)]);
        let mut stats = IngestStats::default();
        let stored = capture_directory(
            &store,
            &mut journal,
            &source,
            Path::new(""),
            &mut entries,
            &mut stats,
        )
        .unwrap();
        drop_skipped(&mut entries, &stats.skipped_caches);

        assert!(stored);
        let paths: Vec<&str> = entries.keys().map(String::as_str).collect();
        assert_eq!(
            paths,
            [
                "app",
                "app/notes.md",
                "keep",
                "keep/.keep",
                "lookalike",
                "lookalike/CACHEDIR.TAG",
                "lookalike/data.txt",
                "src",
                "src/main.rs",
            ]
        );
        assert_eq!(
            stats.skipped_caches,
            ["app/target-android-build-std", "target"]
        );
        // empty, nested, nested/empty, nested/empty/deeper, app/build, app/build/out
        assert_eq!(stats.skipped_empty_directories, 6);
        assert_eq!(stats.directories, 4);
    }
}
