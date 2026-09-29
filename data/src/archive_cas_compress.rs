//! Compresses the objects an Archive CAS root already stores (see
//! [`crate::archive_cas::ArchiveCompression`]). Each `.blob` is read once, checked
//! against its hash while it is compressed, and replaced by a `.zst` only after that
//! copy decompresses back to the same hash; objects that do not compress stay as they
//! are and are remembered, so a later pass does not try them again. Nothing a manifest,
//! root or signature records changes.

use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use anyhow::{Context, Result};
use serde::Serialize;

use crate::archive_cas::{ArchiveCasStorage, ArchiveObject, KeptRaw};
use crate::cas::CasHash;

/// Hashes of objects a pass found not worth compressing, one hex hash per line.
pub const KEPT_RAW_FILE: &str = "compression-kept-raw.txt";

#[derive(Debug, Clone)]
pub struct CompressOptions {
    pub level: i32,
    /// Compress into scratch files and report, changing nothing.
    pub dry_run: bool,
    /// Stop once this many bytes of stored objects have been examined.
    pub max_bytes: Option<u64>,
    /// Objects compressed at once (each on its own thread).
    pub jobs: usize,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct CompressReport {
    /// Objects stored as they are that this pass looked at.
    pub examined: usize,
    pub examined_bytes: u64,
    /// Objects now (or, in a dry run, that would be) stored compressed ...
    pub compressed: usize,
    /// ... their size before ...
    pub compressed_from_bytes: u64,
    /// ... and after.
    pub compressed_to_bytes: u64,
    /// Kept as they are: too small to gain anything.
    pub kept_small: usize,
    /// Kept as they are: compressing saved too little (this pass, or remembered from one).
    pub kept_incompressible: usize,
    /// Already stored compressed.
    pub already_compressed: usize,
    /// Objects not handled, with the reason (a damaged `.blob` is reported, never
    /// replaced).
    pub failed: Vec<(String, String)>,
    pub seconds: f64,
}

impl CompressReport {
    /// Bytes compression saves (or would) on the objects it compressed.
    #[must_use]
    pub fn saved_bytes(&self) -> u64 {
        self.compressed_from_bytes
            .saturating_sub(self.compressed_to_bytes)
    }
}

/// Compresses every object in `store` still stored as a `.blob`, when that pays.
/// `progress` is called after each object with the report so far.
///
/// # Errors
/// When the object directory cannot be listed or the kept-raw record cannot be read or
/// written. A failure on one object is recorded in the report and the pass goes on.
pub fn compress_objects(
    store: &ArchiveCasStorage,
    options: &CompressOptions,
    progress: impl Fn(&CompressReport) + Sync,
) -> Result<CompressReport> {
    let started = Instant::now();
    let kept_path = store.root().join(KEPT_RAW_FILE);
    let kept: HashSet<CasHash> = match fs::read_to_string(&kept_path) {
        Ok(text) => text
            .lines()
            .filter_map(|line| hex::decode(line.trim()).ok())
            .filter_map(|bytes| CasHash::from_slice(&bytes).ok())
            .collect(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => HashSet::new(),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", kept_path.display()))
        }
    };
    let hashes = store.object_hashes_with_progress(|_| {})?;
    let report = Mutex::new(CompressReport::default());
    let kept_record = Mutex::new(None::<fs::File>);
    let next = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    let record_kept = |hash: CasHash| -> Result<()> {
        let mut file = kept_record.lock().unwrap_or_else(|e| e.into_inner());
        if file.is_none() {
            *file = Some(
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&kept_path)
                    .with_context(|| format!("failed to open {}", kept_path.display()))?,
            );
        }
        if let Some(file) = file.as_mut() {
            writeln!(file, "{}", hash.to_hex())?;
        }
        Ok(())
    };
    let work = || -> Result<()> {
        loop {
            if stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            let Some(&hash) = hashes.get(next.fetch_add(1, Ordering::Relaxed)) else {
                return Ok(());
            };
            let raw = store.object_path(hash);
            let Ok(metadata) = fs::metadata(&raw) else {
                let mut report = report.lock().unwrap_or_else(|e| e.into_inner());
                report.already_compressed += 1;
                continue;
            };
            let object = ArchiveObject {
                hash,
                size: metadata.len(),
            };
            {
                let mut report = report.lock().unwrap_or_else(|e| e.into_inner());
                if options
                    .max_bytes
                    .is_some_and(|max| report.examined_bytes >= max)
                {
                    stop.store(true, Ordering::Relaxed);
                    return Ok(());
                }
                report.examined += 1;
                report.examined_bytes += object.size;
                if kept.contains(&hash) {
                    report.kept_incompressible += 1;
                    progress(&report);
                    continue;
                }
            }
            let outcome = if options.dry_run {
                let scratch = store.scratch_path("compress-trial");
                let outcome = store
                    .compress_to(&raw, object, options.level, &scratch)
                    .map(|result| result.map(|after| (object.size, after)));
                let _ = fs::remove_file(&scratch);
                outcome
            } else {
                store.compress_stored_object(object, options.level)
            };
            let mut report = report.lock().unwrap_or_else(|e| e.into_inner());
            match outcome {
                Ok(Ok((before, after))) => {
                    report.compressed += 1;
                    report.compressed_from_bytes += before;
                    report.compressed_to_bytes += after;
                }
                Ok(Err(KeptRaw::Small)) => report.kept_small += 1,
                Ok(Err(KeptRaw::Incompressible)) => {
                    report.kept_incompressible += 1;
                    if !options.dry_run {
                        record_kept(hash)?;
                    }
                }
                Err(error) => report.failed.push((hash.to_hex(), format!("{error:#}"))),
            }
            progress(&report);
        }
    };
    let jobs = options.jobs.max(1);
    let results: Vec<Result<()>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..jobs).map(|_| scope.spawn(work)).collect();
        handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .unwrap_or_else(|_| Err(anyhow::anyhow!("a compression worker panicked")))
            })
            .collect()
    });
    if let Some(file) = kept_record.into_inner().unwrap_or_else(|e| e.into_inner()) {
        file.sync_all()?;
    }
    results.into_iter().collect::<Result<()>>()?;
    let mut report = report.into_inner().unwrap_or_else(|e| e.into_inner());
    report.seconds = started.elapsed().as_secs_f64();
    Ok(report)
}

/// Where a root keeps its kept-raw record.
#[must_use]
pub fn kept_raw_path(store: &ArchiveCasStorage) -> PathBuf {
    store.root().join(KEPT_RAW_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive_cas::{ArchiveEntry, ArchiveManifest};

    fn text(lines: usize) -> Vec<u8> {
        (0..lines)
            .flat_map(|n| {
                format!("line {n}: the quick brown fox jumps over the lazy dog\n").into_bytes()
            })
            .collect()
    }

    fn noise(len: usize) -> Vec<u8> {
        // xorshift: incompressible, deterministic.
        let mut state = 0x9E37_79B9_7F4A_7C15_u64;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state.to_le_bytes()[0]
            })
            .collect()
    }

    #[test]
    fn a_pass_compresses_what_pays_and_every_reader_still_gets_the_same_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let store = ArchiveCasStorage::with_buffer_size(dir.path().join("cas"), 4_096).unwrap();
        let big_text = text(60_000); // ~3.4 MB, crosses the sample size
        let small_text = text(400);
        let random = noise(2 * 1024 * 1024);
        let tiny = b"tiny".to_vec();
        let objects: Vec<_> = [&big_text, &small_text, &random, &tiny]
            .iter()
            .map(|bytes| store.add_content(bytes).unwrap().object)
            .collect();

        let options = CompressOptions {
            level: 3,
            dry_run: true,
            max_bytes: None,
            jobs: 2,
        };
        let trial = compress_objects(&store, &options, |_| {}).unwrap();
        assert_eq!(trial.compressed, 2, "{trial:?}");
        assert!(
            store.object_path(objects[0].hash).exists(),
            "a dry run changes nothing"
        );
        assert!(!kept_raw_path(&store).exists());

        let report = compress_objects(
            &store,
            &CompressOptions {
                dry_run: false,
                ..options.clone()
            },
            |_| {},
        )
        .unwrap();
        assert_eq!(
            (
                report.compressed,
                report.kept_incompressible,
                report.kept_small
            ),
            (2, 1, 1),
            "{report:?}"
        );
        assert!(report.failed.is_empty());
        assert!(report.saved_bytes() > 3_000_000);
        for (object, bytes) in objects.iter().zip([&big_text, &small_text, &random, &tiny]) {
            store.verify_object(*object).unwrap();
            assert_eq!(
                store.read_range(object.hash, 0, bytes.len()).unwrap(),
                **bytes
            );
            if bytes.len() > 1_050 {
                assert_eq!(
                    store.read_range(object.hash, 1_000, 50).unwrap(),
                    bytes[1_000..1_050]
                );
            } else {
                assert!(store.read_range(object.hash, 1_000, 50).is_err());
            }
            let restored = dir
                .path()
                .join(format!("restored-{}", &object.hash.to_hex()[..8]));
            store.restore_object_to_path(*object, &restored).unwrap();
            assert_eq!(fs::read(&restored).unwrap(), **bytes);
        }
        assert!(!store.object_path(objects[0].hash).exists());
        assert!(store.compressed_object_path(objects[0].hash).exists());
        assert!(
            store.object_path(objects[2].hash).exists(),
            "noise stays as it is"
        );
        let listed = store.list_objects().unwrap();
        assert_eq!(listed.len(), 4);

        // A second pass has nothing left to do and does not retry the noise.
        let again = compress_objects(
            &store,
            &CompressOptions {
                dry_run: false,
                ..options
            },
            |_| {},
        )
        .unwrap();
        assert_eq!(
            (
                again.compressed,
                again.already_compressed,
                again.kept_incompressible
            ),
            (0, 2, 1)
        );

        // Removal frees the compressed file.
        assert!(store.remove_object(objects[0].hash).unwrap() < big_text.len() as u64);
        assert!(!store.has_object(objects[0].hash));
    }

    #[test]
    fn a_damaged_blob_is_reported_and_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let store = ArchiveCasStorage::new(dir.path().join("cas")).unwrap();
        let object = store.add_content(&text(2_000)).unwrap().object;
        let path = store.object_path(object.hash);
        let mut damaged = fs::read(&path).unwrap();
        damaged[10] ^= 1;
        fs::write(&path, &damaged).unwrap();
        let options = CompressOptions {
            level: 3,
            dry_run: false,
            max_bytes: None,
            jobs: 1,
        };
        let report = compress_objects(&store, &options, |_| {}).unwrap();
        assert_eq!(report.failed.len(), 1, "{report:?}");
        assert_eq!(fs::read(&path).unwrap(), damaged);
        assert!(!store.compressed_object_path(object.hash).exists());
    }

    #[test]
    fn a_compressing_root_stores_new_objects_compressed_and_dedups_across_forms() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("cas");
        let plain = ArchiveCasStorage::new(&root).unwrap();
        let before = plain.add_content(&text(1_000)).unwrap().object;
        let mut store = ArchiveCasStorage::new(&root).unwrap();
        store.set_compression(Some(3)).unwrap();
        assert_eq!(
            ArchiveCasStorage::new(&root).unwrap().compression_level(),
            Some(3)
        );

        // Content, streams and ingested files all publish compressed ...
        let content = store.add_content(&text(3_000)).unwrap();
        assert!(content.inserted);
        assert!(store.compressed_object_path(content.object.hash).exists());
        let streamed = store.add_stream(&mut &text(4_000)[..]).unwrap();
        assert!(store.compressed_object_path(streamed.object.hash).exists());
        let source = dir.path().join("source.txt");
        fs::write(&source, text(5_000)).unwrap();
        let ingested = store.ingest_file(&source, "source.txt").unwrap();
        assert!(ingested.inserted);
        assert!(store.compressed_object_path(ingested.object.hash).exists());
        assert!(!store.object_path(ingested.object.hash).exists());
        // ... incompressible and small ones stay raw ...
        let random = store.add_content(&noise(64 * 1024)).unwrap().object;
        assert!(store.object_path(random.hash).exists());
        // ... and the same content again is recognised in either form.
        assert!(!store.add_content(&text(3_000)).unwrap().inserted);
        assert!(!store.add_stream(&mut &text(4_000)[..]).unwrap().inserted);
        assert!(!store.add_stream(&mut &text(1_000)[..]).unwrap().inserted);
        let copy = dir.path().join("copy.txt");
        fs::write(&copy, text(5_000)).unwrap();
        assert!(!store.ingest_file(&copy, "copy.txt").unwrap().inserted);
        store.verify_object(before).unwrap();

        // Manifests (control objects) are written and read through the same paths.
        let manifest = ArchiveManifest::new(
            "compressed",
            "test",
            1_700_000_000,
            vec![ArchiveEntry::File {
                path: "source.txt".into(),
                object: ingested.object,
                modified_at_unix_secs: None,
            }],
        )
        .unwrap();
        let (path, _) = store.write_manifest(&manifest).unwrap();
        assert_eq!(store.read_manifest(&path).unwrap(), manifest);

        store.set_compression(None).unwrap();
        assert_eq!(
            ArchiveCasStorage::new(&root).unwrap().compression_level(),
            None
        );
    }
}
