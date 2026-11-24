//! Measure the repository's versioned Xet-inspired content-defined chunk profile
//! in Archive CAS and reconstruct the source byte-for-byte. Reads use the platform
//! proactor and optional chunk writes heed thermal pressure. The probe compares
//! chunked storage with whole-file CAS; it is not Xet wire-compatible.

use anyhow::{anyhow, bail, Context, Result};
use data::archive_cas::chunking::{FileManifest, MAX_CHUNK_BYTES, MIN_CHUNK_BYTES, PROFILE};
use data::archive_cas::worker::{ArchiveCasWorkerPool, WorkerTask};
use data::archive_cas::{ArchiveCasStorage, ArchiveIngestResult, ArchiveObject};
use data::cas::CasHash;
use loadngo_proactor::{
    new_platform_proactor, CompletionKind, PlatformPort, Proactor, ProactorHandle,
};
use loadngo_thermal::{platform_provider, ThermalGovernor, ThermalProvider, ThermalRecommendation};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MAX_CHUNKS: usize = 1_000_000;
const MAX_MANIFEST_BYTES: usize = 256 * 1024 * 1024;
const HASH_BUFFER_BYTES: usize = 64 * 1024;
const FAIR_WORK_DELAY: Duration = Duration::from_millis(10);
const SERIOUS_WORK_DELAY: Duration = Duration::from_millis(100);

/// Owns the proactor and thermal/CPU workers used by the probe. Chunk ingest
/// applies optional-work pacing at each bounded source read and CAS write.
struct ProbeRuntime {
    proactor: Proactor<PlatformPort>,
    handle: ProactorHandle<PlatformPort>,
    thermal: ThermalAdmission,
    workers: ArchiveCasWorkerPool,
}

struct ThermalAdmission {
    provider: Box<dyn ThermalProvider>,
    governor: ThermalGovernor,
    samples: u64,
    next_sample: Instant,
}

impl ThermalAdmission {
    fn new() -> Self {
        let mut provider = platform_provider();
        let mut governor = ThermalGovernor::new(Default::default());
        let now = Instant::now();
        governor.observe(&provider.observe(), now);
        let next_sample = now + governor.snapshot().pressure.sample_interval();
        Self {
            provider,
            governor,
            samples: 1,
            next_sample,
        }
    }

    fn observe_if_due(&mut self, force: bool) {
        let now = Instant::now();
        if force || now >= self.next_sample {
            self.governor.observe(&self.provider.observe(), now);
            self.samples += 1;
            self.next_sample = now + self.governor.snapshot().pressure.sample_interval();
        }
    }

    fn admit(
        &mut self,
        proactor: &Proactor<PlatformPort>,
        handle: &ProactorHandle<PlatformPort>,
    ) -> io::Result<()> {
        self.observe_if_due(false);
        loop {
            match self.governor.snapshot().recommendation {
                ThermalRecommendation::Baseline => return Ok(()),
                ThermalRecommendation::ReduceOptional => {
                    wait_for_deadline(proactor, handle, FAIR_WORK_DELAY)?;
                    self.observe_if_due(false);
                    match self.governor.snapshot().recommendation {
                        ThermalRecommendation::Baseline | ThermalRecommendation::ReduceOptional => {
                            return Ok(())
                        }
                        ThermalRecommendation::MinimizeOptional
                        | ThermalRecommendation::PauseOptional => continue,
                    }
                }
                ThermalRecommendation::MinimizeOptional => {
                    wait_for_deadline(proactor, handle, SERIOUS_WORK_DELAY)?;
                    self.observe_if_due(false);
                    match self.governor.snapshot().recommendation {
                        ThermalRecommendation::Baseline
                        | ThermalRecommendation::ReduceOptional
                        | ThermalRecommendation::MinimizeOptional => return Ok(()),
                        ThermalRecommendation::PauseOptional => continue,
                    }
                }
                ThermalRecommendation::PauseOptional => {
                    let delay = self.next_sample.saturating_duration_since(Instant::now());
                    wait_for_deadline(proactor, handle, delay.max(Duration::from_millis(1)))?;
                    self.observe_if_due(true);
                }
            }
        }
    }
}

fn wait_for_deadline(
    proactor: &Proactor<PlatformPort>,
    handle: &ProactorHandle<PlatformPort>,
    delay: Duration,
) -> io::Result<()> {
    let (sender, receiver) = mpsc::sync_channel(1);
    handle.defer_for(delay, CompletionKind::Timer, 0, move |_| {
        let _ = sender.send(());
    })?;
    loop {
        match receiver.try_recv() {
            Ok(()) => return Ok(()),
            Err(TryRecvError::Disconnected) => {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "thermal deadline completion channel closed",
                ));
            }
            Err(TryRecvError::Empty) => {
                proactor.run_once()?;
            }
        }
    }
}

impl ProbeRuntime {
    fn open() -> io::Result<Self> {
        let proactor = new_platform_proactor()?;
        let handle = proactor.handle();
        Ok(Self {
            proactor,
            handle,
            thermal: ThermalAdmission::new(),
            workers: ArchiveCasWorkerPool::new(1, 1)?,
        })
    }

    fn service_ready(&mut self) -> io::Result<()> {
        self.proactor.run_once_until(Instant::now())?;
        self.thermal.observe_if_due(false);
        Ok(())
    }

    fn wait_until_optional_work_allowed(&mut self) -> io::Result<()> {
        self.service_ready()?;
        self.thermal.admit(&self.proactor, &self.handle)
    }

    fn admit_optional_work(&mut self) -> io::Result<()> {
        self.wait_until_optional_work_allowed()
    }

    fn store_content(
        &mut self,
        store: &ArchiveCasStorage,
        bytes: Vec<u8>,
    ) -> Result<ArchiveIngestResult> {
        let proactor = &self.proactor;
        let handle = &self.handle;
        let thermal = &mut self.thermal;
        store
            .add_content_with_proactor_admitted(proactor, handle, bytes, |proactor, handle| {
                thermal.admit(proactor, handle).map_err(anyhow::Error::from)
            })
            .context("failed to store object through the proactor")
    }

    fn read_content_range_into(
        &mut self,
        store: &ArchiveCasStorage,
        hash: CasHash,
        offset: u64,
        output: &mut [u8],
    ) -> Result<usize> {
        self.service_ready()
            .context("failed to service proactor events before CAS range read")?;
        let result = {
            let proactor = &self.proactor;
            let handle = &self.handle;
            let thermal = &mut self.thermal;
            store.read_range_into_with_proactor_admitted(
                proactor,
                handle,
                hash,
                offset,
                output,
                |proactor, handle| thermal.admit(proactor, handle).map_err(anyhow::Error::from),
            )
        }
        .context("failed to read CAS range through the proactor");
        self.service_ready()
            .context("failed to service proactor events after CAS range read")?;
        result
    }

    fn ingest_whole_file(
        &mut self,
        store: &ArchiveCasStorage,
        input: &Path,
    ) -> Result<ArchiveIngestResult> {
        let result = {
            let proactor = &self.proactor;
            let handle = &self.handle;
            let thermal = &mut self.thermal;
            store.ingest_file_with_proactor_admitted(proactor, handle, input, |proactor, handle| {
                thermal.admit(proactor, handle).map_err(anyhow::Error::from)
            })
        }
        .with_context(|| format!("failed to ingest {} through the proactor", input.display()))?;
        self.service_ready()
            .context("failed to service proactor events after whole-file ingest")?;
        Ok(result)
    }

    fn verify_content(&mut self, store: &ArchiveCasStorage, object: ArchiveObject) -> Result<()> {
        self.service_ready()
            .context("failed to service proactor events before CAS verification")?;
        let result = {
            let proactor = &self.proactor;
            let handle = &self.handle;
            let thermal = &mut self.thermal;
            store.verify_object_with_proactor_admitted(
                proactor,
                handle,
                object,
                |proactor, handle| thermal.admit(proactor, handle).map_err(anyhow::Error::from),
            )
        }
        .context("failed to verify CAS object through the proactor");
        self.service_ready()
            .context("failed to service proactor events after CAS verification")?;
        result
    }

    fn restore_content(
        &mut self,
        store: &ArchiveCasStorage,
        object: ArchiveObject,
        destination: &Path,
    ) -> Result<()> {
        self.service_ready()
            .context("failed to service proactor events before CAS restore")?;
        let result = {
            let proactor = &self.proactor;
            let handle = &self.handle;
            let thermal = &mut self.thermal;
            store.restore_object_to_path_with_proactor_admitted(
                proactor,
                handle,
                object,
                destination,
                |proactor, handle| thermal.admit(proactor, handle).map_err(anyhow::Error::from),
            )
        }
        .with_context(|| format!("failed to restore CAS object to {}", destination.display()));
        self.service_ready()
            .context("failed to service proactor events after CAS restore")?;
        result
    }

    fn serialize_manifest(&mut self, manifest: Arc<FileManifest>) -> Result<Vec<u8>> {
        self.admit_optional_work()?;
        let task = self.workers.try_submit(&self.handle, move || {
            manifest
                .to_canonical_json()
                .map_err(|error| error.to_string())
        })?;
        self.wait_worker(task)
    }

    fn hash_path_offloaded(&mut self, path: PathBuf) -> Result<(CasHash, u64)> {
        self.admit_optional_work()?;
        let task = self.workers.try_submit(&self.handle, move || {
            hash_path(&path).map_err(|error| error.to_string())
        })?;
        self.wait_worker(task)
    }

    fn wait_worker<T>(&mut self, mut task: WorkerTask<Result<T, String>>) -> Result<T> {
        loop {
            if let Some(result) = task.try_take() {
                return result
                    .map_err(|panic| anyhow!(panic.to_string()))?
                    .map_err(anyhow::Error::msg);
            }
            self.proactor
                .run_once()
                .context("failed to dispatch proactor completions while waiting for CPU work")?;
            self.thermal.observe_if_due(false);
        }
    }
}

fn main() {
    if let Err(error) = run() {
        if error.chain().any(|cause| {
            cause
                .downcast_ref::<std::io::Error>()
                .is_some_and(|io| io.kind() == std::io::ErrorKind::BrokenPipe)
        }) {
            return;
        }
        eprintln!("repository_chunk_probe: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || matches!(args[0].as_str(), "--help" | "-h" | "help") {
        print_help();
        return Ok(());
    }
    let flags = parse_flags(&args)?;
    let cas_root = required_path(&flags, "--cas-root")?;
    let whole_cas_root = flags
        .get("--whole-cas-root")
        .map(PathBuf::from)
        .unwrap_or_else(|| cas_root.clone());
    let hybrid_cas_root = flags.get("--hybrid-cas-root").map(PathBuf::from);
    if let Some(hybrid_cas_root) = &hybrid_cas_root {
        if hybrid_cas_root == &cas_root || hybrid_cas_root == &whole_cas_root {
            bail!("hybrid CAS root must be separate from chunk and whole-file CAS roots");
        }
    }
    let input = required_path(&flags, "--input")?;
    let manifest_path = required_path(&flags, "--manifest")?;
    let output = required_path(&flags, "--output")?;
    let whole_file_output = required_path(&flags, "--whole-file-output")?;
    for (label, path) in [
        ("input", input.as_path()),
        ("manifest", manifest_path.as_path()),
        ("output", output.as_path()),
        ("whole-file output", whole_file_output.as_path()),
    ] {
        if !path.is_absolute() {
            bail!("{label} path must be absolute: {}", path.display());
        }
    }
    let distinct_paths = [
        input.as_path(),
        manifest_path.as_path(),
        output.as_path(),
        whole_file_output.as_path(),
    ];
    for (index, path) in distinct_paths.iter().enumerate() {
        if distinct_paths[index + 1..].contains(path) {
            bail!("input, manifest, and output paths must all be different");
        }
    }
    if manifest_path.exists() || output.exists() || whole_file_output.exists() {
        bail!("manifest and output paths must not already exist");
    }

    let started = Instant::now();
    let store = ArchiveCasStorage::new(cas_root)?;
    let whole_store = ArchiveCasStorage::new(whole_cas_root)?;
    let chunk_ingest_started = Instant::now();
    let mut ingest = ingest(&store, &input)?;
    let chunk_ingest_ms = chunk_ingest_started.elapsed().as_millis();
    let whole_ingest_started = Instant::now();
    let whole_file = ingest.runtime.ingest_whole_file(&whole_store, &input)?;
    let whole_file_ingest_ms = whole_ingest_started.elapsed().as_millis();
    if whole_file.object.hash != ingest.manifest.file_hash
        || whole_file.object.size != ingest.manifest.file_size
    {
        bail!("whole-file CAS identity differs from the chunk manifest");
    }
    ingest
        .runtime
        .verify_content(&whole_store, whole_file.object)?;
    let whole_restore_started = Instant::now();
    ingest
        .runtime
        .restore_content(&whole_store, whole_file.object, &whole_file_output)?;
    let (restored_hash, restored_size) = ingest
        .runtime
        .hash_path_offloaded(whole_file_output.clone())?;
    if restored_hash != whole_file.object.hash || restored_size != whole_file.object.size {
        bail!("whole-file CAS reconstruction differs from its object identity");
    }
    let whole_file_restore_ms = whole_restore_started.elapsed().as_millis();
    let (hybrid_strategy, hybrid_object_count_added, hybrid_object_bytes_added, hybrid_ingest_ms) =
        if let Some(hybrid_cas_root) = hybrid_cas_root {
            let hybrid_store = ArchiveCasStorage::new(hybrid_cas_root)?;
            let hybrid_started = Instant::now();
            let (strategy, object_count, object_bytes) =
                if ingest.manifest.file_size < MIN_CHUNK_BYTES as u64 {
                    let result = ingest.runtime.ingest_whole_file(&hybrid_store, &input)?;
                    (
                        "whole-file",
                        if result.inserted { 1 } else { 0 },
                        if result.inserted {
                            result.object.size
                        } else {
                            0
                        },
                    )
                } else {
                    let result = ingest_chunked(&hybrid_store, &input, &mut ingest.runtime)?;
                    let chunks = result
                        .manifest
                        .chunks
                        .iter()
                        .filter(|chunk| chunk.inserted)
                        .try_fold((0_usize, 0_u64), |(count, bytes), chunk| {
                            Ok::<_, anyhow::Error>((
                                count + 1,
                                bytes
                                    .checked_add(chunk.object.size)
                                    .ok_or_else(|| anyhow!("hybrid chunk bytes exceed u64"))?,
                            ))
                        })?;
                    let manifest_count = if result.manifest_inserted { 1 } else { 0 };
                    let manifest_bytes = if result.manifest_inserted {
                        result.manifest_object.size
                    } else {
                        0
                    };
                    (
                        "chunked",
                        chunks.0 + manifest_count,
                        chunks
                            .1
                            .checked_add(manifest_bytes)
                            .ok_or_else(|| anyhow!("hybrid CAS bytes exceed u64"))?,
                    )
                };
            (
                strategy,
                object_count,
                object_bytes,
                hybrid_started.elapsed().as_millis(),
            )
        } else {
            ("not-measured", 0, 0, 0)
        };
    let manifest = ingest.manifest.as_ref();
    let manifest_bytes = manifest.to_canonical_json()?;
    let manifest_object = ingest.manifest_object;
    publish_new_file(&manifest_path, &manifest_bytes)?;
    // The on-disk copy is the input to verification, so the probe exercises the
    // durable representation instead of trusting its in-memory manifest.
    let mut bytes = Vec::new();
    File::open(&manifest_path)?
        .take(MAX_MANIFEST_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    let stored_manifest = FileManifest::from_canonical_json(&bytes, MAX_MANIFEST_BYTES, MAX_CHUNKS)
        .context("failed to parse the newly written canonical chunk manifest")?;
    let chunk_restore_started = Instant::now();
    let reconstructed = reconstruct(&store, &stored_manifest, &output, &mut ingest.runtime)?;
    let chunk_restore_ms = chunk_restore_started.elapsed().as_millis();
    if reconstructed != stored_manifest.file_hash {
        bail!("reconstructed file hash differs from the manifest");
    }

    let unique_chunks = manifest
        .chunks
        .iter()
        .filter(|chunk| chunk.inserted)
        .count();
    let reused_chunks = manifest.chunks.len() - unique_chunks;
    let (new_chunk_bytes, reused_chunk_bytes) = manifest.chunks.iter().try_fold(
        (0_u64, 0_u64),
        |(new_bytes, reused_bytes), chunk| -> Result<_> {
            if chunk.inserted {
                Ok((
                    new_bytes
                        .checked_add(chunk.object.size)
                        .ok_or_else(|| anyhow!("new chunk byte count exceeds u64"))?,
                    reused_bytes,
                ))
            } else {
                Ok((
                    new_bytes,
                    reused_bytes
                        .checked_add(chunk.object.size)
                        .ok_or_else(|| anyhow!("reused chunk byte count exceeds u64"))?,
                ))
            }
        },
    )?;
    println!("profile\t{PROFILE}");
    println!("input_bytes\t{}", manifest.file_size);
    println!("chunks\t{}", manifest.chunks.len());
    println!("unique_chunks_this_run\t{unique_chunks}");
    println!("already_present_chunks\t{reused_chunks}");
    println!("new_chunk_bytes\t{new_chunk_bytes}");
    println!("reused_chunk_bytes\t{reused_chunk_bytes}");
    println!("manifest_bytes\t{}", manifest_bytes.len());
    println!("manifest_object_bytes\t{}", manifest_object.size);
    println!("manifest_inserted\t{}", ingest.manifest_inserted);
    println!("chunk_ingest_ms\t{chunk_ingest_ms}");
    println!("whole_file_ingest_ms\t{whole_file_ingest_ms}");
    println!("chunk_restore_ms\t{chunk_restore_ms}");
    println!("whole_file_restore_ms\t{whole_file_restore_ms}");
    println!("hybrid_strategy\t{hybrid_strategy}");
    println!("hybrid_small_file_whole_threshold_bytes\t{MIN_CHUNK_BYTES}");
    println!("hybrid_objects_added\t{hybrid_object_count_added}");
    println!("hybrid_object_bytes_added\t{hybrid_object_bytes_added}");
    println!("hybrid_ingest_ms\t{hybrid_ingest_ms}");
    println!(
        "whole_file_cas_bytes_added\t{}",
        if whole_file.inserted {
            whole_file.object.size
        } else {
            0
        }
    );
    println!("whole_file_hash\t{}", whole_file.object.hash);
    println!("whole_file_inserted\t{}", whole_file.inserted);
    println!("whole_file_reconstructed\t{}", whole_file_output.display());
    println!("file_hash\t{}", manifest.file_hash);
    println!("manifest_hash\t{}", manifest_object.hash);
    println!("thermal_samples\t{}", ingest.runtime.thermal.samples);
    println!(
        "thermal_pressure\t{:?}",
        ingest.runtime.thermal.governor.snapshot().pressure
    );
    println!("reconstructed\t{}", output.display());
    println!("elapsed_ms\t{}", started.elapsed().as_millis());
    Ok(())
}

struct IngestResult {
    manifest: Arc<FileManifest>,
    manifest_object: ArchiveObject,
    manifest_inserted: bool,
    runtime: ProbeRuntime,
}

fn ingest(store: &ArchiveCasStorage, input: &Path) -> Result<IngestResult> {
    let mut reader = ProbeRuntime::open()
        .context("failed to create the platform proactor for chunked ingest")?;
    let result = ingest_chunked(store, input, &mut reader)?;
    Ok(IngestResult {
        manifest: result.manifest,
        manifest_object: result.manifest_object,
        manifest_inserted: result.manifest_inserted,
        runtime: reader,
    })
}

fn ingest_chunked(
    store: &ArchiveCasStorage,
    input: &Path,
    reader: &mut ProbeRuntime,
) -> Result<ChunkIngestResult> {
    let manifest = {
        let proactor = &reader.proactor;
        let handle = &reader.handle;
        let thermal = &mut reader.thermal;
        store.ingest_chunked_file_with_proactor_admitted(
            proactor,
            handle,
            input,
            MAX_CHUNKS,
            |proactor, handle| thermal.admit(proactor, handle).map_err(anyhow::Error::from),
        )
    }
    .with_context(|| format!("failed to ingest {} as CAS chunks", input.display()))?;
    reader.service_ready()?;
    let manifest = Arc::new(manifest);
    let manifest_bytes = reader.serialize_manifest(Arc::clone(&manifest))?;
    let manifest_ingest = reader.store_content(store, manifest_bytes)?;
    let manifest_object = manifest_ingest.object;
    Ok(ChunkIngestResult {
        manifest,
        manifest_object,
        manifest_inserted: manifest_ingest.inserted,
    })
}

struct ChunkIngestResult {
    manifest: Arc<FileManifest>,
    manifest_object: ArchiveObject,
    manifest_inserted: bool,
}

fn reconstruct(
    store: &ArchiveCasStorage,
    manifest: &FileManifest,
    output: &Path,
    reader: &mut ProbeRuntime,
) -> Result<CasHash> {
    let temporary = temporary_path(output)?;
    let result = (|| -> Result<CasHash> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .with_context(|| format!("failed to create {}", temporary.display()))?;
        let mut hasher = blake3::Hasher::new();
        let mut total = 0_u64;
        let mut buffer = vec![0_u8; MAX_CHUNK_BYTES];
        for (index, chunk) in manifest.chunks.iter().enumerate() {
            let size = usize::try_from(chunk.object.size)
                .context("chunk size exceeds the local address space")?;
            let count = reader
                .read_content_range_into(store, chunk.object.hash, 0, &mut buffer[..size])
                .with_context(|| format!("failed to read chunk {index}"))?;
            let bytes = &buffer[..count];
            let chunk_size = count as u64;
            let chunk_hash = CasHash::from_bytes(*blake3::hash(bytes).as_bytes());
            if chunk_hash != chunk.object.hash || chunk_size != chunk.object.size {
                bail!("chunk {index} failed its hash or size check");
            }
            reader.admit_optional_work()?;
            hasher.update(bytes);
            file.write_all(bytes)?;
            total = total
                .checked_add(chunk_size)
                .ok_or_else(|| anyhow!("reconstructed size exceeds u64"))?;
        }
        if total != manifest.file_size {
            bail!(
                "reconstructed size is {total}, expected {}",
                manifest.file_size
            );
        }
        file.sync_all()?;
        let hash = CasHash::from_bytes(*hasher.finalize().as_bytes());
        if hash != manifest.file_hash {
            bail!(
                "reconstructed file hash is {hash}, expected {}",
                manifest.file_hash
            );
        }
        drop(file);
        fs::hard_link(&temporary, output)
            .with_context(|| format!("failed to publish {}", output.display()))?;
        Ok(hash)
    })();
    let _ = fs::remove_file(&temporary);
    result
}

fn hash_path(path: &Path) -> Result<(CasHash, u64)> {
    let mut file =
        File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut buffer = [0_u8; HASH_BUFFER_BYTES];
    let mut hasher = blake3::Hasher::new();
    let mut size = 0_u64;
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
        size = size
            .checked_add(count as u64)
            .ok_or_else(|| anyhow!("file size exceeds u64"))?;
    }
    Ok((CasHash::from_bytes(*hasher.finalize().as_bytes()), size))
}

fn publish_new_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let temporary = temporary_path(path)?;
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        // A hard link publishes without replacing a destination created by a race.
        fs::hard_link(&temporary, path)
            .with_context(|| format!("failed to publish {}", path.display()))?;
        let _ = fs::remove_file(&temporary);
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn temporary_path(path: &Path) -> Result<PathBuf> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let name = path
        .file_name()
        .ok_or_else(|| anyhow!("file path has no name: {}", path.display()))?
        .to_string_lossy();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    Ok(parent.join(format!(".{name}.{}.{nanos}.partial", std::process::id())))
}

fn parse_flags(args: &[String]) -> Result<BTreeMap<&'static str, String>> {
    let known = [
        "--cas-root",
        "--whole-cas-root",
        "--hybrid-cas-root",
        "--input",
        "--manifest",
        "--output",
        "--whole-file-output",
    ];
    if !args.len().is_multiple_of(2) {
        bail!("expected a value for {}; pass --help", args.last().unwrap());
    }
    let mut flags = BTreeMap::new();
    for pair in args.as_chunks::<2>().0 {
        let name = pair[0].as_str();
        if !known.contains(&name) {
            bail!("unknown flag {name:?}; pass --help");
        }
        let key = known.iter().copied().find(|known| *known == name).unwrap();
        if flags.insert(key, pair[1].clone()).is_some() {
            bail!("flag {name} was passed more than once; pass --help");
        }
    }
    Ok(flags)
}

fn required_path(flags: &BTreeMap<&'static str, String>, name: &'static str) -> Result<PathBuf> {
    let value = flags
        .get(name)
        .ok_or_else(|| anyhow!("missing required flag {name}; pass --help"))?;
    Ok(PathBuf::from(value))
}

fn print_help() {
    println!(
        "repository_chunk_probe — measure the repository CDC profile against whole-file CAS\
         \n\
         \nRequired flags:\
         \n  --cas-root PATH   [required] Archive CAS root used for immutable chunk and manifest objects\
         \n  --whole-cas-root PATH [optional] Separate whole-file comparison CAS; defaults to --cas-root\
         \n  --hybrid-cas-root PATH [optional] Separate v3 hybrid policy CAS for the working-tree workload\
         \n  --input PATH      [required] Absolute source file to split and hash\
         \n  --manifest PATH   [required] New output path for the canonical chunk manifest\
         \n  --output PATH     [required] New output path for verified reconstruction\
         \n  --whole-file-output PATH [required] New output path restored from whole-file CAS\
         \n\
         \nThis is the current repository CDC profile v2: 64-byte rolling Buzhash with 8 KiB minimum,\
         \n64 KiB target and 128 KiB maximum chunks. It is Xet-inspired and is not\
         \nwire-compatible with Xet. Each chunk and the reconstructed file are\
         \nverified by BLAKE3. Existing manifest and output files are refused.\
         \nSource reads, whole-file streaming ingest, raw CAS verification/writes,\
         \nand raw range reconstruction use loadngo-proactor. Whole-file restore also\
         \nuses proactor reads and writes. Source/CAS reads and bounded payload or\
         \nrestore writes use loadngo-thermal admission. Hashing, sync,\
         \ncompression, and publication remain on the caller.\
         \n\
         \nExample:\
         \n  repository_chunk_probe --cas-root /tmp/repo-chunks --whole-cas-root /tmp/repo-whole --input /work/model.bin --manifest /tmp/model.chunks.json --output /tmp/model.reconstructed --whole-file-output /tmp/model.whole-file"
    );
}
