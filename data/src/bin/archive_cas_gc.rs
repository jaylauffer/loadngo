//! Sweeps blob objects that no manifest currently in the CAS root
//! references. Blobs are globally content-addressed with no refcounting, so
//! this always scans every manifest still on disk (not just one archive_id)
//! before treating anything as orphaned -- a blob two unrelated archives
//! happen to share is never touched while either manifest still lists it.
//!
//! Dry-run by default; pass `--execute` to actually delete. This is the only
//! step in the delete/GC toolchain that frees disk space, and the only
//! irreversible one -- run `archive_cas_verify` on anything you still care
//! about beforehand if you have any doubt.

use anyhow::{Context, Result};
use data::archive_cas::ArchiveCasStorage;
use data::cas::CasHash;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn main() {
    if let Err(error) = run() {
        eprintln!("archive_cas_gc: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args = Args::parse()?;
    let store = ArchiveCasStorage::new(&args.cas_root)?;

    let manifest_paths = store.list_manifests()?;
    let mut referenced: BTreeSet<CasHash> = BTreeSet::new();
    let mut manifest_count = 0usize;
    for path in &manifest_paths {
        let manifest = store
            .read_manifest(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        manifest_count += 1;
        // The manifest's own bytes are a CAS object too (write_manifest
        // stores it via add_content), and it is far smaller than any blob it
        // references, so it is never worth including in the sweep target --
        // but it must count as "referenced" or GC would delete the very
        // manifest files it just read.
        referenced.insert(manifest.digest()?);
        for entry in &manifest.entries {
            if let data::archive_cas::ArchiveEntry::File { object, .. } = entry {
                referenced.insert(object.hash);
            }
        }
    }

    // The blob directory walk below is one `read_dir` + one `metadata()`
    // stat per object; on a large CAS over slow or removable storage that
    // can run long with zero feedback otherwise. `referenced.len()` is
    // already the expected object count on a fully-GC'd store (every
    // manifest's own bytes plus every blob it points at), and it came from
    // manifests already in memory -- no extra disk work to know it, so
    // there's no excuse for the walk below to be silent.
    let expected_total = referenced.len();
    let scan_started = Instant::now();
    let mut last_reported_at = Instant::now();
    let objects = store.list_objects_with_progress(|found| {
        if found == expected_total || last_reported_at.elapsed() >= Duration::from_secs(5) {
            last_reported_at = Instant::now();
            let elapsed = scan_started.elapsed();
            let rate = found as f64 / elapsed.as_secs_f64().max(0.001);
            let percent = (found as f64 / expected_total.max(1) as f64) * 100.0;
            let eta = if rate > 0.0 && found < expected_total {
                Some(Duration::from_secs_f64(
                    (expected_total - found) as f64 / rate,
                ))
            } else {
                None
            };
            eprintln!(
                "gc scan progress: {found} of ~{expected_total} objects found on disk ({percent:.1}%, from manifests), elapsed {}{}",
                format_duration(elapsed),
                eta.map(|eta| format!(", eta {}", format_duration(eta)))
                    .unwrap_or_default(),
            );
        }
    })?;
    let mut orphaned: Vec<(CasHash, u64)> = objects
        .into_iter()
        .filter(|(hash, _)| !referenced.contains(hash))
        .collect();
    orphaned.sort_by_key(|(hash, _)| *hash);

    let total_bytes: u64 = orphaned.iter().map(|(_, size)| size).sum();
    println!(
        "{} manifest(s) scanned, {} referenced object(s) (blobs + manifest bytes).",
        manifest_count,
        referenced.len()
    );
    println!(
        "{} orphaned blob(s), {} reclaimable.",
        orphaned.len(),
        format_bytes(total_bytes)
    );
    for (hash, size) in &orphaned {
        println!("  {hash}  {}", format_bytes(*size));
    }

    if orphaned.is_empty() {
        return Ok(());
    }
    if !args.execute {
        println!();
        println!("Dry run only -- nothing removed. Pass --execute to actually delete these blobs.");
        return Ok(());
    }

    let mut freed = 0u64;
    for (hash, _) in &orphaned {
        freed += store
            .remove_object(*hash)
            .with_context(|| format!("failed to remove object {hash}"))?;
    }
    println!(
        "Removed {} blob(s), freed {}.",
        orphaned.len(),
        format_bytes(freed)
    );
    Ok(())
}

#[derive(Debug)]
struct Args {
    cas_root: PathBuf,
    execute: bool,
}

impl Args {
    fn parse() -> Result<Self> {
        let mut cas_root = None;
        let mut execute = false;
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--cas-root" => cas_root = args.next().map(PathBuf::from),
                "--execute" => execute = true,
                "--dry-run" => execute = false,
                "--help" | "-h" => {
                    print_usage();
                    std::process::exit(0);
                }
                other => return Err(anyhow::anyhow!("unknown argument: {other}")),
            }
        }
        Ok(Self {
            cas_root: cas_root
                .ok_or_else(|| anyhow::anyhow!("missing --cas-root <archive-directory>"))?,
            execute,
        })
    }
}

fn print_usage() {
    eprintln!(
        "Usage: cargo run -p data --bin archive_cas_gc -- --cas-root <archive-directory> [--execute]\n\nDry-run by default; reports blob objects no manifest in the CAS root\nreferences. Pass --execute to actually delete them."
    );
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

fn format_duration(duration: Duration) -> String {
    let total_secs = duration.as_secs();
    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    let secs = total_secs % 60;
    if hours > 0 {
        format!("{hours}h{minutes:02}m{secs:02}s")
    } else if minutes > 0 {
        format!("{minutes}m{secs:02}s")
    } else {
        format!("{secs}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_duration_picks_the_coarsest_useful_unit() {
        assert_eq!(format_duration(Duration::from_secs(9)), "9s");
        assert_eq!(format_duration(Duration::from_secs(65)), "1m05s");
        assert_eq!(format_duration(Duration::from_secs(3_725)), "1h02m05s");
    }

    #[test]
    fn format_bytes_matches_the_units_a_reader_expects() {
        assert_eq!(format_bytes(999), "999 B");
        assert_eq!(format_bytes(1_048_576), "1.00 MiB");
    }
}
