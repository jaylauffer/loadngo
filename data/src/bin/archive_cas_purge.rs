//! Frees the space removed files still take in an Archive CAS root: retires superseded
//! manifests and deletes the objects nothing references any more, for every archive in
//! the root. Prints the exact list by default; deletes only with `--execute <plan-id>`,
//! and only if the plan it computes again has that id. See `data::archive_cas_purge`.

use anyhow::{bail, Result};
use data::archive_cas::ArchiveCasStorage;
use data::archive_cas_purge::{
    execute_purge, plan_archive_deletion, plan_purge, PurgePlan, PurgeProgress, Sweep,
};
use data::cli::{ArgDoc, Usage};
use std::io::Write as _;
use std::path::PathBuf;

fn main() {
    if let Err(error) = run() {
        eprintln!("archive_cas_purge: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let mut cas_root = None;
    let mut execute = None;
    let mut sweep = Sweep::Retired;
    let mut delete_archives = Vec::new();
    let mut yes = false;
    let mut args = data::cli::read_args(&usage(), true).into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--cas-root" => cas_root = args.next().map(PathBuf::from),
            "--execute" => execute = args.next(),
            "--full" => sweep = Sweep::Full,
            "--delete-archive" => delete_archives.extend(args.next()),
            "--yes" => yes = true,
            other => bail!("unknown argument: {other}\n{}", usage().hint()),
        }
    }
    let Some(cas_root) = cas_root else {
        bail!("missing --cas-root <archive-directory>\n{}", usage().hint());
    };
    let store = ArchiveCasStorage::new(&cas_root)?;
    if !delete_archives.is_empty() {
        return delete(&store, &delete_archives, yes);
    }
    let plan = plan_purge(&store, sweep, show)?;
    eprintln!();
    print_plan(&plan);
    if plan.is_empty() {
        return Ok(());
    }
    match execute {
        None => println!(
            "\nNothing deleted. To delete exactly this list:\n  archive_cas_purge --cas-root {}{} --execute {}",
            cas_root.display(),
            if sweep == Sweep::Full { " --full" } else { "" },
            plan.id()
        ),
        Some(id) if id == plan.id() => {
            let outcome = execute_purge(&store, &plan, show)?;
            eprintln!();
            println!(
                "Purged: {} files ({} objects), {} freed.",
                outcome.files_removed,
                outcome.objects_removed,
                human(outcome.bytes_freed)
            );
        }
        Some(id) => bail!(
            "plan {id} is not the current plan ({}); nothing deleted. Review the list above and run again with its id.",
            plan.id()
        ),
    }
    Ok(())
}

/// Deletes each archive in turn, keeping its delete and add logs (the record of what was
/// removed and added). Prints each plan; deletes only with `--yes`.
fn delete(store: &ArchiveCasStorage, archives: &[String], yes: bool) -> Result<()> {
    let mut freed = 0;
    for archive in archives {
        let mut plan = plan_archive_deletion(store, archive, show)?;
        for manifest in &mut plan.manifests {
            manifest.files.retain(|file| {
                let name = file.to_string_lossy();
                !(name.ends_with(".delete-log.json") || name.ends_with(".add-log.json"))
            });
        }
        plan.manifest_file_bytes = plan
            .manifests
            .iter()
            .flat_map(|m| &m.files)
            .map(|f| std::fs::metadata(f).map_or(0, |m| m.len()))
            .sum();
        eprintln!();
        print_plan(&plan);
        if yes {
            let outcome = execute_purge(store, &plan, show)?;
            eprintln!();
            println!(
                "Deleted archive {archive}: {} files ({} objects), {} freed.\n",
                outcome.files_removed,
                outcome.objects_removed,
                human(outcome.bytes_freed)
            );
            freed += outcome.bytes_freed;
        }
    }
    if yes {
        println!("Total freed: {}", human(freed));
    } else {
        println!("\nNothing deleted. Add --yes to delete these archives (their delete/add logs are kept).");
    }
    Ok(())
}

fn show(progress: PurgeProgress) {
    match progress {
        PurgeProgress::ReadingManifests { done, total } => {
            eprint!("\rreading manifests {done}/{total}   ");
        }
        PurgeProgress::ListingObjects { found } => eprint!("\rlisting objects {found}   "),
        PurgeProgress::Deleting { done, total } => eprint!("\rdeleting {done}/{total}   "),
    }
    let _ = std::io::stderr().flush();
}

fn print_plan(plan: &PurgePlan) {
    println!("Purge plan {} for {}", plan.id(), plan.cas_root.display());
    if plan.is_empty() {
        println!("Nothing to purge: no superseded manifests and no unreferenced objects.");
        return;
    }
    println!(
        "Frees {} in {} files.",
        human(plan.bytes()),
        plan.file_count()
    );
    println!("\nSuperseded manifests ({}):", plan.manifests.len());
    for manifest in &plan.manifests {
        println!(
            "  {} {} ({})",
            manifest.archive_id,
            manifest.root,
            manifest.superseded_by.map_or_else(
                || "archive deleted".to_string(),
                |next| format!("superseded by {}", &next.to_hex()[..12])
            )
        );
        for file in &manifest.files {
            println!("    - {}", file.display());
        }
    }
    println!("\nUnreferenced objects ({}):", plan.objects.len());
    for object in &plan.objects {
        println!(
            "  {:>10}  {}  {}",
            human(object.size),
            &object.hash.to_hex()[..16],
            object.origin
        );
    }
}

fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    #[allow(clippy::cast_precision_loss)]
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn usage() -> Usage {
    const ARGS: &[ArgDoc] = &[
        ArgDoc::required(
            "--cas-root",
            "<archive-directory>",
            "Archive CAS root to purge (every archive in it)",
        ),
        ArgDoc::switch(
            "--full",
            "also list every object to find strays no manifest ever listed (slow on a spinning drive)",
        ),
        ArgDoc::repeated(
            "--delete-archive",
            "<archive-id>",
            "delete this archive entirely (all versions; objects other archives list stay; delete/add logs kept)",
        ),
        ArgDoc::switch("--yes", "with --delete-archive: actually delete (without it, prints the lists)"),
        ArgDoc::optional(
            "--execute",
            "<plan-id>",
            "delete exactly the plan with this id, as printed by a run without --execute",
        ),
    ];
    const EXAMPLES: &[&str] = &[
        "cargo run -p data --bin archive_cas_purge -- --cas-root \"/Volumes/Loadngo Archive Staging/loadngo-archive-cas\"",
        "cargo run -p data --bin archive_cas_purge -- --cas-root \"/Volumes/Loadngo Archive Staging/loadngo-archive-cas\" --execute 1a2b3c4d5e6f",
    ];
    const NOTES: &[&str] = &[
        "Without --execute, prints what would be deleted and deletes nothing.",
        "Retires every superseded manifest (with its signature and delete/add log) and deletes every object no remaining manifest lists.",
        "--execute recomputes the plan and deletes only if its id matches, so what is deleted is exactly the list you reviewed.",
        "By default checks only the objects the retired manifests could free, one lookup each; --full lists every object, which on a spinning USB drive runs at about 90 objects a second.",
    ];
    Usage {
        bin: "archive_cas_purge",
        invocation: "cargo run -p data --bin archive_cas_purge --",
        about: "free the space removed files still take: retire superseded manifests and delete unreferenced objects",
        args: ARGS,
        examples: EXAMPLES,
        notes: NOTES,
    }
}
