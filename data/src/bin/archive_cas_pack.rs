//! Build and verify an experimental immutable packed snapshot of loose Archive CAS blobs.
//! The source root is never changed; compressed `.zst` objects stay outside the snapshot.

use anyhow::{anyhow, bail, Context, Result};
use data::archive_cas::pack::PackedSnapshot;
use data::archive_cas::ArchiveCasStorage;
use data::cli::{ArgDoc, Usage};
use data::repository::runtime::{RepositoryArchiveCasRestoreIo, RepositoryRuntime};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    if let Err(error) = run() {
        eprintln!("archive_cas_pack: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let (mut cas_root, mut destination, mut verify_mounted) = (None, None, false);
    let mut args = data::cli::read_args(&usage(), true).into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--cas-root" => cas_root = args.next().map(PathBuf::from),
            "--destination" => destination = args.next().map(PathBuf::from),
            "--verify-mounted" => verify_mounted = true,
            other => bail!("unknown argument: {other}\n{}", usage().hint()),
        }
    }
    let cas_root = cas_root.context("missing --cas-root <archive-directory>")?;
    if verify_mounted {
        if destination.is_some() {
            bail!("--verify-mounted does not take --destination");
        }
        return verify_mounted_snapshots(&cas_root);
    }
    let destination = destination.context("missing --destination <new-directory>")?;
    let store = ArchiveCasStorage::new(&cas_root)?;
    let mut runtime = RepositoryRuntime::new()?;
    let (proactor, handle, thermal, blocking_io) = runtime.blocking_io_parts_mut()?;
    let mut restore_io = RepositoryArchiveCasRestoreIo::new(proactor, handle, thermal, blocking_io);
    let snapshot = PackedSnapshot::create_from_loose_objects(
        &store,
        &destination,
        proactor,
        handle,
        &mut restore_io,
    )?;
    snapshot.verify_all(proactor, handle, &mut restore_io)?;
    let read_count = snapshot.object_count().min(256);
    let sample_step = snapshot.object_count().div_ceil(read_count.max(1));
    let ranges = snapshot
        .objects()
        .enumerate()
        .filter(|(index, (_, object))| object.size > 0 && index % sample_step.max(1) == 0)
        .take(256)
        .map(|(_, (hash, object))| {
            let size = object.size.min(4096) as usize;
            let mut selector = [0_u8; 8];
            selector.copy_from_slice(&hash.as_bytes()[..8]);
            let offset = u64::from_be_bytes(selector) % (object.size - size as u64 + 1);
            (hash, offset, size)
        })
        .collect::<Vec<_>>();
    let mut range_checks = 0;
    snapshot.read_ranges_with_proactor(
        proactor,
        handle,
        &mut restore_io,
        ranges,
        |hash, offset, packed| {
            let mut source = File::open(store.object_path(hash))?;
            source.seek(SeekFrom::Start(offset))?;
            let mut loose = vec![0_u8; packed.len()];
            source.read_exact(&mut loose)?;
            if loose != packed {
                return Err(anyhow!("packed range differs from loose object {hash}"));
            }
            range_checks += 1;
            Ok(())
        },
    )?;
    println!(
        "Packed and BLAKE3-verified {} loose objects ({} data bytes, {} index bytes); {} proactor range reads matched loose CAS bytes. Snapshot: {}. Source CAS objects remain unchanged.",
        snapshot.object_count(),
        snapshot.pack_bytes(),
        snapshot.index_bytes()?,
        range_checks,
        destination.display()
    );
    Ok(())
}

fn verify_mounted_snapshots(cas_root: &std::path::Path) -> Result<()> {
    let store = ArchiveCasStorage::new(cas_root)?;
    let objects: Vec<_> = store.packed_objects().collect();
    if objects.is_empty() {
        bail!("{} has no mounted packed snapshots", cas_root.display());
    }
    let missing_loose = objects
        .iter()
        .filter(|object| {
            !store.object_path(object.hash).exists()
                && !store.compressed_object_path(object.hash).exists()
        })
        .count();
    let mut runtime = RepositoryRuntime::new()?;
    for object in &objects {
        let (proactor, handle, thermal, _) = runtime.parts_mut();
        store.verify_object_with_proactor_admitted(
            proactor,
            handle,
            *object,
            |proactor, handle| {
                thermal
                    .admit(proactor, handle)
                    .map(|_| ())
                    .map_err(anyhow::Error::from)
            },
        )?;
    }
    let sample_step = objects.len().div_ceil(256).max(1);
    let mut range_checks = 0;
    for object in objects.iter().step_by(sample_step).take(256) {
        if object.size == 0 {
            continue;
        }
        let range_size = usize::try_from(object.size.min(4096))?;
        let mut range = vec![0; range_size];
        let (proactor, handle, thermal, _) = runtime.parts_mut();
        let count = store.read_range_into_with_proactor_admitted(
            proactor,
            handle,
            object.hash,
            0,
            &mut range,
            |proactor, handle| {
                thermal
                    .admit(proactor, handle)
                    .map(|_| ())
                    .map_err(anyhow::Error::from)
            },
        )?;
        if count != range_size {
            bail!("mounted packed range for {} was truncated", object.hash);
        }
        range_checks += 1;
    }
    let restore_sample = objects[0];
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let restore_path = std::env::temp_dir().join(format!(
        "loadngo-packed-object-restore-{}-{nonce}",
        std::process::id()
    ));
    let (proactor, handle, thermal, blocking_io) = runtime.blocking_io_parts_mut()?;
    let mut restore_io = RepositoryArchiveCasRestoreIo::new(proactor, handle, thermal, blocking_io);
    store.restore_object_to_path_with_proactor_restore_io(
        proactor,
        handle,
        restore_sample,
        &restore_path,
        &mut restore_io,
    )?;
    if fs::metadata(&restore_path)?.len() != restore_sample.size {
        let _ = fs::remove_file(&restore_path);
        bail!("mounted packed object restore size mismatch");
    }
    fs::remove_file(&restore_path)?;
    println!(
        "Verified {} mounted packed objects through ArchiveCasStorage; {} had no loose copy; {} proactor range reads succeeded; one packed object restored transactionally.",
        objects.len(),
        missing_loose,
        range_checks
    );
    Ok(())
}

fn usage() -> Usage {
    const ARGS: &[ArgDoc] = &[
        ArgDoc::required(
            "--cas-root",
            "<archive-directory>",
            "source Archive CAS root",
        ),
        ArgDoc::required(
            "--destination",
            "<new-directory>",
            "new directory for the packed snapshot and index",
        ),
        ArgDoc::switch(
            "--verify-mounted",
            "verify snapshots discovered below --cas-root/packs, without --destination",
        ),
    ];
    const EXAMPLES: &[&str] = &[
        "cargo run -p data --bin archive_cas_pack -- --cas-root /Volumes/Backup/archive-cas --destination /Volumes/Backup/archive-cas-pack",
    ];
    const NOTES: &[&str] = &[
        "Experimental snapshot format; the source CAS stays authoritative and unchanged.",
        "Packs raw .blob objects only. Compressed .zst objects are skipped.",
        "Uses the repository proactor and thermal admission for bounded pack writes, full hash verification, and sampled range reads.",
        "A partial range is not independently hash-verifiable; callers must validate the complete object or reconstructed file.",
        "This does not yet integrate packs with ArchiveCasStorage lookup, transfer, or GC.",
    ];
    Usage {
        bin: "archive_cas_pack",
        invocation: "cargo run -p data --bin archive_cas_pack --",
        about: "create a snapshot or verify packed objects mounted under an Archive CAS root",
        args: ARGS,
        examples: EXAMPLES,
        notes: NOTES,
    }
}
