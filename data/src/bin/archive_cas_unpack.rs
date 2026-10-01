//! Unpacks the zips in an archive (.zip, .ipa, .jar, .apk; zips inside them too), so
//! their contents are stored and deduplicated as ordinary files. Writes a new version of
//! the archive, whose change record names each zip; deletes nothing. The zips' own bytes stay stored until
//! the superseded version is purged (archive_cas_purge, or the browser's Purge drive).

use anyhow::{bail, Context, Result};
use data::archive_cas::ArchiveCasStorage;
use data::archive_cas_unpack::{survey, unpack_zips, UnpackProgress, DEFAULT_MAX_DEPTH};
use data::archive_view::list_archives;
use data::cli::{ArgDoc, Usage};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    if let Err(error) = run() {
        eprintln!("archive_cas_unpack: {error:#}");
        std::process::exit(1);
    }
}

fn gib(bytes: u64) -> String {
    #[allow(clippy::cast_precision_loss)]
    let value = bytes as f64 / f64::from(1u32 << 30);
    format!("{value:.2} GiB")
}

fn run() -> Result<()> {
    let (mut cas_root, mut archive, mut dry_run, mut max_depth, mut reason, mut actor) =
        (None, None, false, DEFAULT_MAX_DEPTH, None, None);
    let mut args = data::cli::read_args(&usage(), true).into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--cas-root" => cas_root = args.next().map(PathBuf::from),
            "--archive" => archive = args.next(),
            "--dry-run" => dry_run = true,
            "--max-depth" => {
                max_depth = args
                    .next()
                    .context("--max-depth requires a value")?
                    .parse()
                    .context("--max-depth must be a whole number")?;
            }
            "--reason" => reason = args.next(),
            "--actor" => actor = args.next(),
            other => bail!("unknown argument: {other}\n{}", usage().hint()),
        }
    }
    let cas_root = cas_root.context("missing --cas-root")?;
    let archive = archive.context("missing --archive <archive-id>")?;
    let store = ArchiveCasStorage::new(&cas_root)?;
    let current: Vec<_> = list_archives(&cas_root, None)?
        .into_iter()
        .filter(|l| l.archive_id == archive && !l.superseded)
        .collect();
    let listing = match current.as_slice() {
        [one] => one,
        [] => bail!("no archive {archive:?} in {}", cas_root.display()),
        _ => bail!("archive {archive:?} has more than one current manifest"),
    };
    eprintln!("reading {}", listing.manifest_path.display());
    let (manifest, root) = store.read_manifest_and_root(&listing.manifest_path)?;

    if dry_run {
        let (unpackable, skipped) = survey(&store, &manifest)?;
        let (zips, members, bytes) = unpackable.iter().fold((0u64, 0usize, 0u64), |a, z| {
            (a.0 + z.object.size, a.1 + z.members, a.2 + z.member_bytes)
        });
        println!(
            "{} zips to unpack ({} stored), {members} members ({} uncompressed); {} left whole.",
            unpackable.len(),
            gib(zips),
            gib(bytes),
            skipped.len()
        );
        for skip in &skipped {
            println!("  left whole: {}: {}", skip.path, skip.reason);
        }
        println!(
            "Nothing written (top-level zips only; zips inside them are unpacked too when run)."
        );
        return Ok(());
    }

    let reason = reason.context("missing --reason")?;
    let actor = actor.context("missing --actor")?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_secs();
    let (unpacked, report) = unpack_zips(
        &store,
        &manifest,
        root,
        max_depth,
        now,
        &actor,
        &reason,
        |p| match p {
            UnpackProgress::Zip {
                path,
                members,
                bytes,
            } => {
                eprintln!("unpacked {path}: {members} members, {}", gib(bytes));
            }
            UnpackProgress::Skipped { path, reason } => eprintln!("left whole {path}: {reason}"),
        },
    )?;
    let Some(unpacked) = unpacked else {
        println!(
            "No zip could be unpacked ({} left whole); nothing written.",
            report.skipped.len()
        );
        return Ok(());
    };
    let (path, new_root) = store.write_manifest(&unpacked)?;
    let zip_bytes: u64 = report.unpacked.iter().map(|z| z.object.size).sum();
    println!("New version of {archive}: {}", path.display());
    println!(
        "Root: {} (made from {})",
        new_root.hash,
        &root.to_hex()[..12]
    );
    println!(
        "Unpacked {} zips ({} stored); {} left whole.",
        report.unpacked.len(),
        gib(zip_bytes),
        report.skipped.len()
    );
    println!(
        "Members: {} newly stored ({}), {} already stored (deduplicated).",
        report.new_objects,
        gib(report.new_object_bytes),
        report.reused_objects
    );
    println!();
    println!("Nothing was deleted. Purging the superseded version frees the zips' own bytes");
    println!("(those not still used elsewhere); sign the new version with archive_cas_sign.");
    Ok(())
}

fn usage() -> Usage {
    const ARGS: &[ArgDoc] = &[
        ArgDoc::required(
            "--cas-root",
            "<archive-directory>",
            "Archive CAS root holding the archive",
        ),
        ArgDoc::required(
            "--archive",
            "<archive-id>",
            "archive whose zips to unpack (its current version)",
        ),
        ArgDoc::switch(
            "--dry-run",
            "list the zips and their members' total size; write nothing",
        ),
        ArgDoc::optional(
            "--max-depth",
            "<n>",
            "levels of zips inside zips to unpack (default 3)",
        ),
        ArgDoc::optional(
            "--reason",
            "<why>",
            "recorded in the new version's change record (required unless --dry-run)",
        ),
        ArgDoc::optional(
            "--actor",
            "<who>",
            "recorded in the new version's change record (required unless --dry-run)",
        ),
    ];
    const EXAMPLES: &[&str] = &[
        "cargo run --release -p data --bin archive_cas_unpack -- --cas-root \"/Volumes/Loadngo Archive Staging/loadngo-archive-cas\" --archive loadngo-archive --dry-run",
    ];
    const NOTES: &[&str] = &[
        "Unpacks .zip, .ipa, .jar and .apk; Office documents (.docx, ...) stay whole.",
        "A zip with encrypted members, unsupported compression, unsafe names or a CRC mismatch is left whole and listed.",
        "Writes a new version whose change record names each zip unpacked; deletes nothing.",
    ];
    Usage {
        bin: "archive_cas_unpack",
        invocation: "cargo run --release -p data --bin archive_cas_unpack --",
        about:
            "unpack the zips in an archive so their contents are stored and deduplicated as files",
        args: ARGS,
        examples: EXAMPLES,
        notes: NOTES,
    }
}
