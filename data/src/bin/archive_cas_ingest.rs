use anyhow::{anyhow, bail, Context, Result};
use data::archive_cas::{ArchiveCasStorage, ArchiveEntry, ArchiveManifest};
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

    let manifest = ArchiveManifest::new(
        args.archive_id,
        args.source_label,
        unix_now()?,
        entries.into_values().collect(),
    )?;
    let (manifest_path, manifest_object) = store.write_manifest(&manifest)?;

    println!("Archive CAS root: {}", store.root().display());
    println!("Ingest journal: {}", journal.path.display());
    println!("Archive manifest: {}", manifest_path.display());
    println!("Archive root object: {}", manifest_object.hash);
    println!("Archive root size: {}", manifest_object.size);
    println!("Files declared: {}", stats.files);
    println!("Directories declared: {}", stats.directories);
    println!("Symlinks declared: {}", stats.symlinks);
    println!("Unreadable source entries: {}", stats.unreadable_entries);
    println!("Capture complete: {}", manifest.is_complete());
    println!("Logical file bytes: {}", stats.logical_bytes);
    println!("New archive objects: {}", stats.inserted_objects);
    println!("Deduplicated files: {}", stats.deduplicated_files);
    println!("Journal-reused files: {}", stats.journal_reused_files);
    println!("Resumed source bytes: {}", stats.resumed_bytes);
    Ok(())
}

#[derive(Debug)]
struct Args {
    source: PathBuf,
    cas_root: PathBuf,
    archive_id: String,
    source_label: String,
}

impl Args {
    fn parse() -> Result<Self> {
        let mut source = None;
        let mut cas_root = None;
        let mut archive_id = None;
        let mut source_label = None;
        let mut args = std::env::args().skip(1);

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--source" => source = args.next().map(PathBuf::from),
                "--cas-root" => cas_root = args.next().map(PathBuf::from),
                "--archive-id" => archive_id = args.next(),
                "--source-label" => source_label = args.next(),
                "--help" | "-h" => {
                    print_usage();
                    std::process::exit(0);
                }
                other => return Err(anyhow!("unknown argument: {other}")),
            }
        }

        let archive_id = archive_id.ok_or_else(|| anyhow!("missing --archive-id <id>"))?;
        Ok(Self {
            source: source.ok_or_else(|| anyhow!("missing --source <directory>"))?,
            cas_root: cas_root.ok_or_else(|| anyhow!("missing --cas-root <directory>"))?,
            source_label: source_label.unwrap_or_else(|| archive_id.clone()),
            archive_id,
        })
    }
}

fn print_usage() {
    eprintln!(
        "Usage: cargo run -p data --bin archive_cas_ingest -- --source <read-only-directory> --cas-root <archive-directory> --archive-id <lowercase-id> [--source-label <label>]"
    );
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

fn capture_directory(
    store: &ArchiveCasStorage,
    journal: &mut IngestJournal,
    absolute_directory: &Path,
    relative_directory: &Path,
    entries: &mut BTreeMap<String, ArchiveEntry>,
    stats: &mut IngestStats,
) -> Result<()> {
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
                continue;
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to stat {}", absolute_path.display()))
            }
        };
        let file_type = metadata.file_type();

        if file_type.is_dir() {
            entries.insert(
                portable_path.clone(),
                ArchiveEntry::Directory {
                    path: portable_path,
                    modified_at_unix_secs: modified_at_unix_secs(&metadata),
                },
            );
            stats.directories += 1;
            capture_directory(
                store,
                journal,
                &absolute_path,
                &relative_path,
                entries,
                stats,
            )?;
        } else if file_type.is_file() {
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
        } else {
            bail!(
                "unsupported special file in source: {}",
                absolute_path.display()
            );
        }
    }
    Ok(())
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
}
