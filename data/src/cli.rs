//! Shared command-line conventions for loadngo's standalone tools and test
//! harnesses.
//!
//! Every one of these binaries is run by hand, from memory, often months
//! apart, by whichever agent or person is on call -- there is no shell
//! history to lean on. The convention here (see `docs/CLI_CONVENTIONS.md`
//! for the full rationale) is:
//!
//! - Running a tool with `--help`/`-h`, or with *no arguments at all* when
//!   the tool has any required argument, prints a full description of what
//!   it does, every flag it accepts, and at least one worked example --
//!   to stdout, exit code 0. It is documentation, not an error.
//! - Any other parse failure (an unknown flag, a missing required one)
//!   prints one short line to stderr and a reminder to pass `--help`,
//!   rather than repeating the whole usage block or silently guessing.
//!
//! A binary in a crate that cannot depend on `data` should still follow the
//! same shape; `docs/CLI_CONVENTIONS.md` gives the minimal copy-paste
//! pattern for that case.

use std::path::{Path, PathBuf};

/// One documented command-line argument (or boolean switch).
pub struct ArgDoc {
    /// Flag spelling as the user types it, e.g. `"--cas-root"`. May also be
    /// a short phrase for a subcommand-scoped flag, e.g. `"sign --manifest"`.
    pub flag: &'static str,
    /// Placeholder shown after the flag, e.g. `"<archive-directory>"`.
    /// `None` for a boolean switch that takes no value.
    pub value: Option<&'static str>,
    pub required: bool,
    /// Whether the flag may be passed more than once (e.g. repeated `--path`).
    pub repeatable: bool,
    pub help: &'static str,
}

impl ArgDoc {
    pub const fn required(flag: &'static str, value: &'static str, help: &'static str) -> Self {
        Self {
            flag,
            value: Some(value),
            required: true,
            repeatable: false,
            help,
        }
    }

    pub const fn optional(flag: &'static str, value: &'static str, help: &'static str) -> Self {
        Self {
            flag,
            value: Some(value),
            required: false,
            repeatable: false,
            help,
        }
    }

    pub const fn repeated(flag: &'static str, value: &'static str, help: &'static str) -> Self {
        Self {
            flag,
            value: Some(value),
            required: false,
            repeatable: true,
            help,
        }
    }

    pub const fn switch(flag: &'static str, help: &'static str) -> Self {
        Self {
            flag,
            value: None,
            required: false,
            repeatable: false,
            help,
        }
    }

    fn signature(&self) -> String {
        let base = match self.value {
            Some(value) => format!("{} {value}", self.flag),
            None => self.flag.to_string(),
        };
        if self.repeatable {
            format!("{base} [...]")
        } else {
            base
        }
    }
}

/// The full documentation for one binary's command line, printed verbatim
/// by `--help`, by running with no arguments (see [`read_args`]), and
/// referenced by name in a parse error.
pub struct Usage {
    /// Binary name as `cargo run --bin` sees it, e.g. `"archive_cas_ingest"`.
    pub bin: &'static str,
    /// The exact invocation prefix to show before flags, e.g.
    /// `"cargo run -p data --bin archive_cas_ingest --"`.
    pub invocation: &'static str,
    /// One or two sentences: what this tool does and does not do.
    pub about: &'static str,
    pub args: &'static [ArgDoc],
    /// Full command lines, without the leading `$ `.
    pub examples: &'static [&'static str],
    /// Anything a reader needs before they run this: dry-run defaults,
    /// what "success" means, safety notes.
    pub notes: &'static [&'static str],
}

impl Usage {
    /// Prints the full help text to stdout.
    pub fn print(&self) {
        println!("{} -- {}", self.bin, self.about);
        println!();
        println!("Usage:");
        println!("  {} [OPTIONS]", self.invocation);
        if !self.args.is_empty() {
            println!();
            println!("Options:");
            let width = self
                .args
                .iter()
                .map(|arg| arg.signature().len())
                .max()
                .unwrap_or(0);
            for arg in self.args {
                let tag = if arg.required { "required" } else { "optional" };
                println!(
                    "  {:width$}  ({tag:8})  {}",
                    arg.signature(),
                    arg.help,
                    width = width
                );
            }
        }
        if !self.notes.is_empty() {
            println!();
            println!("Notes:");
            for note in self.notes {
                println!("  - {note}");
            }
        }
        if !self.examples.is_empty() {
            println!();
            println!("Examples:");
            for example in self.examples {
                println!("  {example}");
            }
        }
    }

    /// A one-line reminder to append to a parse error, e.g.
    /// `"run `cargo run -p data --bin archive_cas_ingest -- --help` for the full option list"`.
    pub fn hint(&self) -> String {
        format!("run `{} --help` for the full option list", self.invocation)
    }
}

/// Reads `argv` (skipping the program name) and handles the shared help
/// conventions before any tool-specific parsing runs:
///
/// - `--help`/`-h` anywhere in the arguments always prints `usage` and
///   exits 0.
/// - If `help_on_empty` is set and no arguments were given at all, the same
///   happens. Pass `true` for any tool that has at least one required
///   argument (an empty invocation could not have succeeded anyway, so
///   showing the docs beats a bare "missing --x" on the first run). Pass
///   `false` for a tool that is fully usable with zero arguments (every
///   flag has a working default).
///
/// Otherwise returns the arguments unchanged for the caller to parse.
pub fn read_args(usage: &Usage, help_on_empty: bool) -> Vec<String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let wants_help = args.iter().any(|arg| arg == "--help" || arg == "-h");
    if wants_help || (help_on_empty && args.is_empty()) {
        usage.print();
        std::process::exit(0);
    }
    args
}

/// Removes duplicates from `paths`, keeping the first spelling seen, using
/// each path's canonical (symlink-resolved, absolute) form as the identity
/// -- so `/a/b`, a relative `./b` run from `/a`, and a symlink alias that
/// also reaches `/a/b` all collapse to one entry instead of being treated
/// as different roots. A path that does not exist yet (or that a race
/// unmounts mid-call) is compared by its own literal form instead of
/// failing outright; the caller's next real filesystem access is what
/// should report that error.
pub fn dedup_by_canonical_path(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::with_capacity(paths.len());
    for path in paths {
        let key = path.canonicalize().unwrap_or_else(|_| path.clone());
        if seen.insert(key) {
            result.push(path);
        }
    }
    result
}

/// Storage-device discovery for tools that operate on a CAS root but should
/// not force the caller to already know its path -- see
/// `docs/CLI_CONVENTIONS.md`'s "Discovering a CAS root" section.
pub mod discover {
    use super::*;
    use std::fs;

    /// Directory names that are never worth descending into while scanning
    /// a mounted volume for a CAS root: OS bookkeeping, trash, and recycle
    /// bins that are typically large, sometimes permission-denied, and
    /// never contain one.
    const SKIP_DIR_NAMES: &[&str] = &[
        "System Volume Information",
        ".Trashes",
        ".fseventsd",
        ".Spotlight-V100",
        ".DocumentRevisions-V100",
        ".TemporaryItems",
        "lost+found",
        "$RECYCLE.BIN",
        "Recovery",
    ];

    /// How many directory levels below a candidate mount point to search.
    /// A CAS root's own `objects/` tree is enormous and must never be
    /// descended into once found (handled separately below); this bound
    /// keeps the scan itself fast even on a large, unfamiliar volume.
    const MAX_SCAN_DEPTH: u32 = 3;

    /// True if `path` looks like the root of a loadngo Archive CAS: it has
    /// both an `objects/` and a `manifests/` subdirectory. This never
    /// creates anything, unlike `ArchiveCasStorage::new`.
    pub fn is_archive_cas_root(path: &Path) -> bool {
        path.join("objects").is_dir() && path.join("manifests").is_dir()
    }

    /// Top-level mount points to search: removable/external volumes on
    /// this platform, plus any extra roots the caller names in
    /// `LOADNGO_CAS_SCAN_ROOTS` (a `PATH`-style list, for a mount layout
    /// this function does not already know about).
    pub fn candidate_mount_points() -> Vec<PathBuf> {
        let mut points = Vec::new();

        #[cfg(target_os = "macos")]
        {
            if let Ok(entries) = fs::read_dir("/Volumes") {
                points.extend(entries.flatten().filter_map(non_symlink_dir_path));
            }
        }

        #[cfg(target_os = "linux")]
        {
            let user = std::env::var("USER").unwrap_or_default();
            let mut bases = vec![PathBuf::from("/media"), PathBuf::from("/mnt")];
            if !user.is_empty() {
                bases.push(PathBuf::from("/media").join(&user));
                bases.push(PathBuf::from("/run/media").join(&user));
            }
            for base in bases {
                if let Ok(entries) = fs::read_dir(&base) {
                    points.extend(entries.flatten().filter_map(non_symlink_dir_path));
                } else if base.is_dir() {
                    points.push(base);
                }
            }
        }

        #[cfg(target_os = "windows")]
        {
            for letter in b'A'..=b'Z' {
                let root = PathBuf::from(format!("{}:\\", letter as char));
                if root.is_dir() {
                    points.push(root);
                }
            }
        }

        if let Ok(extra) = std::env::var("LOADNGO_CAS_SCAN_ROOTS") {
            points.extend(std::env::split_paths(&extra));
        }

        points.sort();
        points.dedup();
        points
    }

    /// Filters a `/Volumes`, `/media`, or `/run/media` directory listing
    /// down to real (non-symlink) directories. macOS always has
    /// `/Volumes/Macintosh HD -> /`; treating that as a scannable directory
    /// would make `scan_directory` walk the entire boot volume (`/Volumes`
    /// is itself a real, non-symlink directory, so the walk loops straight
    /// back into every other mount point under a second, bogus path like
    /// `/Volumes/Macintosh HD/Volumes/Zhoenus II/pudding-cas`) -- rediscovering
    /// every external CAS root a second time under an alias path that
    /// `dedup_by_canonical_path` would otherwise have to clean up after the
    /// fact. Skipping the symlink here is cheaper and stops the wasted scan
    /// of the whole boot volume in the first place.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn non_symlink_dir_path(entry: fs::DirEntry) -> Option<PathBuf> {
        let file_type = entry.file_type().ok()?;
        (file_type.is_dir() && !file_type.is_symlink()).then(|| entry.path())
    }

    #[cfg(test)]
    mod real_machine_smoke {
        use super::*;

        #[test]
        #[ignore]
        fn print_real_candidate_mount_points_and_scan_results() {
            println!("candidate mount points:");
            for point in candidate_mount_points() {
                println!("  {}", point.display());
            }
            println!("discovered CAS roots:");
            for root in scan_for_cas_roots() {
                println!("  {}", root.display());
            }
        }
    }

    /// Walks every candidate mount point (bounded to [`MAX_SCAN_DEPTH`]
    /// levels) and returns every directory that looks like an Archive CAS
    /// root. Never follows symlinks and never opens a file, so it is safe
    /// to run against a large or unfamiliar external drive.
    pub fn scan_for_cas_roots() -> Vec<PathBuf> {
        let mut found = Vec::new();
        for mount in candidate_mount_points() {
            scan_directory(&mount, 0, &mut found);
        }
        let mut found = dedup_by_canonical_path(found);
        found.sort();
        found
    }

    fn scan_directory(dir: &Path, depth: u32, found: &mut Vec<PathBuf>) {
        if is_archive_cas_root(dir) {
            found.push(dir.to_path_buf());
            // A CAS root's own objects/ subtree is not worth descending
            // into looking for a *nested* CAS root.
            return;
        }
        if depth >= MAX_SCAN_DEPTH {
            return;
        }
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_dir() || file_type.is_symlink() {
                continue;
            }
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if name.starts_with('.') || SKIP_DIR_NAMES.contains(&name) {
                continue;
            }
            scan_directory(&path, depth + 1, found);
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use tempfile::tempdir;

        #[test]
        fn recognizes_a_directory_with_both_marker_subdirectories() {
            let root = tempdir().unwrap();
            assert!(!is_archive_cas_root(root.path()));
            fs::create_dir_all(root.path().join("objects")).unwrap();
            assert!(!is_archive_cas_root(root.path()));
            fs::create_dir_all(root.path().join("manifests")).unwrap();
            assert!(is_archive_cas_root(root.path()));
        }

        #[test]
        fn scan_finds_a_cas_root_nested_under_a_mount_point_and_does_not_descend_into_it() {
            let mount = tempdir().unwrap();
            let cas_root = mount.path().join("staging").join("loadngo-archive-cas");
            fs::create_dir_all(cas_root.join("objects").join("ab")).unwrap();
            fs::create_dir_all(cas_root.join("manifests")).unwrap();
            // A file inside objects/ that would fail a naive re-descend if
            // it were mistaken for a directory to keep scanning.
            fs::write(cas_root.join("objects").join("ab").join("x.blob"), b"x").unwrap();

            let mut found = Vec::new();
            scan_directory(mount.path(), 0, &mut found);
            assert_eq!(found, vec![cas_root]);
        }

        // Reproduces the real bug: macOS always has a symlink alias like
        // `/Volumes/Macintosh HD -> /` sitting next to real volume mounts.
        // `/Volumes` itself is a real (non-symlink) directory reachable
        // through that alias, so following it during the mount-point scan
        // loops straight back into every other real mount and rediscovers
        // the same CAS root a second time under a bogus alias path --
        // which is exactly what showed up as a duplicated device banner in
        // the browser.
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        #[test]
        fn non_symlink_dir_path_skips_a_symlink_but_keeps_a_real_directory() {
            let parent = tempdir().unwrap();
            let real_dir = parent.path().join("Zhoenus II");
            fs::create_dir_all(&real_dir).unwrap();
            let alias = parent.path().join("Macintosh HD");
            #[cfg(unix)]
            std::os::unix::fs::symlink(parent.path(), &alias).unwrap();

            let mut kept = Vec::new();
            for entry in fs::read_dir(parent.path()).unwrap().flatten() {
                if let Some(path) = non_symlink_dir_path(entry) {
                    kept.push(path);
                }
            }
            assert_eq!(kept, vec![real_dir]);
        }

        #[cfg(unix)]
        #[test]
        fn dedup_by_canonical_path_collapses_a_symlink_alias_of_the_same_directory() {
            let parent = tempdir().unwrap();
            let real_root = parent
                .path()
                .join("Volumes")
                .join("Zhoenus II")
                .join("pudding-cas");
            fs::create_dir_all(real_root.join("objects")).unwrap();
            fs::create_dir_all(real_root.join("manifests")).unwrap();
            let alias_of_parent = parent.path().join("alias");
            std::os::unix::fs::symlink(parent.path(), &alias_of_parent).unwrap();
            let aliased_root = alias_of_parent
                .join("Volumes")
                .join("Zhoenus II")
                .join("pudding-cas");
            assert!(is_archive_cas_root(&aliased_root));

            let deduped = dedup_by_canonical_path(vec![real_root.clone(), aliased_root]);
            assert_eq!(deduped, vec![real_root]);
        }

        #[test]
        fn scan_skips_known_bookkeeping_directories() {
            let mount = tempdir().unwrap();
            let noise = mount.path().join("System Volume Information");
            fs::create_dir_all(noise.join("objects")).unwrap();
            fs::create_dir_all(noise.join("manifests")).unwrap();

            let mut found = Vec::new();
            scan_directory(mount.path(), 0, &mut found);
            assert!(found.is_empty());
        }

        #[test]
        fn scan_respects_the_depth_bound() {
            let mount = tempdir().unwrap();
            let mut deep = mount.path().to_path_buf();
            for name in ["a", "b", "c", "d"] {
                deep = deep.join(name);
            }
            fs::create_dir_all(deep.join("objects")).unwrap();
            fs::create_dir_all(deep.join("manifests")).unwrap();

            let mut found = Vec::new();
            scan_directory(mount.path(), 0, &mut found);
            assert!(
                found.is_empty(),
                "a CAS root 4 levels down should be past the scan bound"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_marks_a_repeatable_flag() {
        let arg = ArgDoc::repeated("--path", "<relative-file-path>", "help");
        assert_eq!(arg.signature(), "--path <relative-file-path> [...]");
    }

    #[test]
    fn signature_of_a_bare_switch_has_no_placeholder() {
        let arg = ArgDoc::switch("--execute", "help");
        assert_eq!(arg.signature(), "--execute");
    }

    #[test]
    fn hint_names_the_invocation() {
        let usage = Usage {
            bin: "archive_cas_ingest",
            invocation: "cargo run -p data --bin archive_cas_ingest --",
            about: "about",
            args: &[],
            examples: &[],
            notes: &[],
        };
        assert_eq!(
            usage.hint(),
            "run `cargo run -p data --bin archive_cas_ingest -- --help` for the full option list"
        );
    }
}
