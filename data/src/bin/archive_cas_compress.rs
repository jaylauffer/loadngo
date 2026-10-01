//! Turns compression on for an Archive CAS root, and compresses the objects it already
//! stores. An object stays named by the hash of its uncompressed bytes; each is checked
//! against that hash while it is compressed and replaced only once its compressed copy
//! decompresses back to it. Manifests, roots and signatures do not change.

use anyhow::{bail, Context, Result};
use data::archive_cas::{ArchiveCasStorage, DEFAULT_COMPRESSION_LEVEL};
use data::archive_cas_compress::{compress_objects, CompressOptions, CompressReport};
use data::cli::{ArgDoc, Usage};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

fn main() {
    if let Err(error) = run() {
        eprintln!("archive_cas_compress: {error:#}");
        std::process::exit(1);
    }
}

#[allow(clippy::cast_precision_loss)] // shown to two decimals
fn gib(bytes: u64) -> String {
    format!("{:.2} GiB", bytes as f64 / f64::from(1u32 << 30))
}

#[allow(clippy::cast_precision_loss)] // shown to one decimal
fn percent(part: u64, whole: u64) -> String {
    if whole == 0 {
        return "-".into();
    }
    format!("{:.1}%", part as f64 * 100.0 / whole as f64)
}

fn value<T: std::str::FromStr>(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<T> {
    args.next()
        .with_context(|| format!("{flag} requires a value"))?
        .parse()
        .map_err(|_| anyhow::anyhow!("{flag} has an invalid value"))
}

fn run() -> Result<()> {
    let (mut cas_root, mut level, mut dry_run, mut enable, mut disable) =
        (None, None, false, false, false);
    let (mut max_gib, mut jobs, mut objects) = (None::<f64>, None, true);
    let mut args = data::cli::read_args(&usage(), true).into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--cas-root" => cas_root = args.next().map(PathBuf::from),
            "--level" => level = Some(value::<i32>(&mut args, "--level")?),
            "--dry-run" => dry_run = true,
            "--enable" => enable = true,
            "--disable" => disable = true,
            "--settings-only" => objects = false,
            "--max-gib" => max_gib = Some(value(&mut args, "--max-gib")?),
            "--jobs" => jobs = Some(value::<usize>(&mut args, "--jobs")?),
            other => bail!("unknown argument: {other}\n{}", usage().hint()),
        }
    }
    let cas_root = cas_root.context("missing --cas-root")?;
    if enable && disable {
        bail!("pass only one of --enable and --disable");
    }
    if dry_run && (enable || disable) {
        bail!("--dry-run changes nothing; drop --enable/--disable");
    }
    if !objects && !(enable || disable) {
        bail!("--settings-only needs --enable or --disable");
    }
    if max_gib.is_some_and(|gib| !gib.is_finite() || gib <= 0.0) {
        bail!("--max-gib must be positive");
    }
    let mut store = ArchiveCasStorage::new(&cas_root)?;
    let level = level
        .or(store.compression_level())
        .unwrap_or(DEFAULT_COMPRESSION_LEVEL);
    if disable {
        store.set_compression(None)?;
        println!(
            "Compression off for new objects in {}; stored objects keep their form.",
            cas_root.display()
        );
        return Ok(());
    }
    if enable {
        store.set_compression(Some(level))?;
        println!(
            "Compression on for new objects in {}: zstd level {level}.",
            cas_root.display()
        );
        if !objects {
            return Ok(());
        }
    }

    let jobs = jobs.unwrap_or_else(|| {
        std::thread::available_parallelism()
            .map_or(1, std::num::NonZero::get)
            .min(4)
    });
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // positive, checked
    let max_bytes = max_gib.map(|gib| (gib * f64::from(1u32 << 30)) as u64);
    let options = CompressOptions {
        level,
        dry_run,
        max_bytes,
        jobs,
    };
    eprintln!(
        "{} objects in {} at zstd level {level}, {jobs} at a time{}...",
        if dry_run { "trying" } else { "compressing" },
        cas_root.display(),
        max_gib.map_or(String::new(), |gib| format!(", up to {gib} GiB"))
    );
    let started = Instant::now();
    let last = Mutex::new(Instant::now());
    let report = compress_objects(&store, &options, |report| {
        let mut last = last.lock().unwrap_or_else(|e| e.into_inner());
        if last.elapsed().as_secs() >= 10 {
            *last = Instant::now();
            let done = report.examined + report.already_compressed;
            eprintln!(
                "  {done} of {} objects ({}), {} read, {} saved so far ({:.0} s)",
                report.objects,
                percent(done as u64, report.objects as u64),
                gib(report.examined_bytes),
                gib(report.saved_bytes()),
                started.elapsed().as_secs_f64()
            );
        }
    })?;
    print_report(&report, dry_run);
    if !report.failed.is_empty() {
        bail!(
            "{} objects could not be compressed (listed above)",
            report.failed.len()
        );
    }
    Ok(())
}

#[allow(clippy::cast_precision_loss)] // rates shown to one decimal
fn print_report(report: &CompressReport, dry_run: bool) {
    let verb = if dry_run {
        "would compress"
    } else {
        "compressed"
    };
    println!(
        "Of {} objects, examined {} stored as they were ({}) in {:.0} s ({:.1} MiB/s).",
        report.objects,
        report.examined,
        gib(report.examined_bytes),
        report.seconds,
        report.examined_bytes as f64 / f64::from(1u32 << 20) / report.seconds.max(0.001)
    );
    println!(
        "{verb} {} objects: {} -> {} (saves {}, {} of those objects, {} of all examined).",
        report.compressed,
        gib(report.compressed_from_bytes),
        gib(report.compressed_to_bytes),
        gib(report.saved_bytes()),
        percent(report.saved_bytes(), report.compressed_from_bytes),
        percent(report.saved_bytes(), report.examined_bytes)
    );
    println!(
        "Kept as they are: {} not worth compressing, {} too small. Already compressed: {}.",
        report.kept_incompressible, report.kept_small, report.already_compressed
    );
    for (hash, error) in &report.failed {
        println!("  failed {hash}: {error}");
    }
    if dry_run {
        println!("Nothing was changed.");
    }
}

fn usage() -> Usage {
    const ARGS: &[ArgDoc] = &[
        ArgDoc::required(
            "--cas-root",
            "<archive-directory>",
            "Archive CAS root to compress",
        ),
        ArgDoc::switch(
            "--dry-run",
            "compress into scratch files and report the saving; change nothing",
        ),
        ArgDoc::switch(
            "--enable",
            "also store every new object compressed from now on (writes compression.json)",
        ),
        ArgDoc::switch(
            "--disable",
            "store new objects as they are again; compresses nothing",
        ),
        ArgDoc::switch(
            "--settings-only",
            "with --enable: change the setting, leave stored objects as they are",
        ),
        ArgDoc::optional(
            "--level",
            "<1-22>",
            "zstd level (default: the root's setting, else 9; 19 packs tighter, far slower)",
        ),
        ArgDoc::optional(
            "--max-gib",
            "<gib>",
            "stop after this much stored data; with --dry-run, a sample to estimate from",
        ),
        ArgDoc::optional(
            "--jobs",
            "<n>",
            "objects compressed at once (default: cores, at most 4)",
        ),
    ];
    const EXAMPLES: &[&str] = &[
        "cargo run --release -p data --bin archive_cas_compress -- --cas-root \"/Volumes/Loadngo Archive Staging/loadngo-archive-cas\" --dry-run --max-gib 20",
        "cargo run --release -p data --bin archive_cas_compress -- --cas-root \"/Volumes/Loadngo Archive Staging/loadngo-archive-cas\" --enable",
    ];
    const NOTES: &[&str] = &[
        "Objects keep their names (the BLAKE3 hash of their uncompressed bytes); a compressed one is <hash>.zst instead of <hash>.blob. Every loadngo reader takes both.",
        "Each object is hashed while compressed; a damaged .blob is reported and left alone. The .blob is removed only after the .zst decompresses to the same hash.",
        "Objects under 8 KiB, and those compressing by less than a sixteenth (media, archives), stay as they are; the latter are listed in compression-kept-raw.txt so a later pass skips them.",
        "Safe to stop and run again: it picks up where it left off.",
        "A pass records how far it has got in compression-progress.json at the root, about every 10 s; Kimi's cas_archives shows it.",
        "Older builds of the browser and tools cannot read .zst objects: rebuild before using a compressed root.",
    ];
    Usage {
        bin: "archive_cas_compress",
        invocation: "cargo run --release -p data --bin archive_cas_compress --",
        about: "compress an Archive CAS root's stored objects, and optionally every new one",
        args: ARGS,
        examples: EXAMPLES,
        notes: NOTES,
    }
}
