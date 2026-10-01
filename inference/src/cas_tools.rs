//! `cas_archives`, `cas_list`, `cas_find`, `cas_read`, `cas_grep`: the [`crate::tools`]
//! interface over every Archive CAS archive on the attached drives, the way the Archive
//! CAS browser shows them ([`data::archive_view`]).
//!
//! `cas_archives` finds the Archive CAS roots on attached storage
//! ([`data::cli::discover`]) and lists each one's archives with their signature status.
//! The other tools take an archive name from that list. Every result begins with the
//! archive's identity (name, drive, root, signed or unsigned) and names the object hash
//! of each file it quotes; every byte read is BLAKE3-checked against the manifest, signed
//! or not, so a model's claims about a file can be checked against the same bytes.

use std::cell::RefCell;
use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use data::archive_cas::ArchiveCasStorage;
use data::archive_cas_compress::read_progress;
use data::archive_view::{list_archives, ArchiveListing, ArchiveView, PublicKey, Tally};
use serde_json::{json, Value};

use crate::tools::{
    as_text, glob_match, numbered_lines, Tool, MAX_GREP_FILE_BYTES, MAX_MATCHES, MAX_SCAN_BYTES,
};

/// Most entries one `cas_list` shows. A model reads every token of a result before it
/// can answer, slowly on the CPU (about 0.4 s a token at 6,000 tokens of context), so a
/// listing stops here and summarises the rest.
const LIST_ENTRIES: usize = 50;

fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing string argument `{key}`"))
}

fn usize_arg(args: &Value, key: &str, default: usize) -> usize {
    args.get(key)
        .and_then(Value::as_u64)
        .and_then(|v| usize::try_from(v).ok())
        .unwrap_or(default)
}

/// The drive an Archive CAS root is on: the volume name under `/Volumes`, `/media`,
/// `/run/media/<user>` or `/mnt`, or the root path itself.
fn drive(root: &Path) -> String {
    let parts: Vec<_> = root
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect();
    let volume = match parts.get(1).map(AsRef::as_ref) {
        Some("Volumes" | "media" | "mnt") => parts.get(2),
        Some("run") if parts.get(2).map(AsRef::as_ref) == Some("media") => parts.get(4),
        _ => None,
    };
    volume.map_or_else(|| root.display().to_string(), ToString::to_string)
}

/// `n` with thousands separators, the way people read counts.
fn count(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// `n` and a noun, singular or plural to match.
fn counted(n: usize, one: &str, many: &str) -> String {
    format!("{} {}", count(n), if n == 1 { one } else { many })
}

/// A byte total in the largest unit it reaches, to two decimals.
#[allow(clippy::cast_precision_loss)] // shown to two decimals
fn size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];
    if bytes < 1024 {
        return format!("{bytes} bytes");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.2} {}", UNITS[unit])
}

/// A duration in seconds, roughly, in the units a person would say it in.
fn span(secs: u64) -> String {
    match secs {
        0..=99 => format!("{secs} s"),
        100..=5_999 => format!("{} min", secs / 60),
        6_000..=172_799 => format!("{} h {} min", secs / 3_600, secs % 3_600 / 60),
        _ => format!("{} days", secs / 86_400),
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// A compression pass that has not recorded progress for this long has stopped (a
/// running pass records it about every ten seconds).
const PROGRESS_STALE_SECS: u64 = 120;

/// What a drive's store does with new objects, and how far its last compression pass
/// got ([`read_progress`]), as lines for `cas_archives`.
fn store_status(root: &Path, now: u64) -> String {
    let mut out = String::new();
    match ArchiveCasStorage::new(root).map(|store| store.compression_level()) {
        Ok(Some(level)) => {
            let _ = writeln!(
                out,
                "  compression: new objects stored with zstd level {level}"
            );
        }
        Ok(None) => out.push_str(
            "  compression: off; objects stored as they are
",
        ),
        Err(_) => {}
    }
    let Some(pass) = read_progress(root) else {
        return out;
    };
    let quiet = now.saturating_sub(pass.updated_at_unix_secs);
    let state = if pass.finished && pass.done() >= pass.objects {
        format!("finished {} ago", span(quiet))
    } else if pass.finished {
        format!("stopped at its size limit {} ago", span(quiet))
    } else if quiet <= PROGRESS_STALE_SECS {
        format!("running (pid {}, updated {} ago)", pass.pid, span(quiet))
    } else {
        format!("stopped without finishing (no update for {})", span(quiet))
    };
    let resume = if !pass.finished && quiet > PROGRESS_STALE_SECS {
        "; running archive_cas_compress again resumes it"
    } else {
        ""
    };
    #[allow(clippy::cast_precision_loss)] // a percentage to one decimal
    let percent = pass.done() as f64 * 100.0 / pass.objects.max(1) as f64;
    let _ = writeln!(
        out,
        "  compression pass at zstd level {}, started {} ago, {state}: {} of {} stored objects done ({percent:.1}%), {} left; {} compressed, saving {}; {} kept as they are; {} failed{resume}",
        pass.level,
        span(now.saturating_sub(pass.started_at_unix_secs)),
        count(pass.done()),
        count(pass.objects),
        count(pass.objects.saturating_sub(pass.done())),
        count(pass.compressed),
        size(pass.saved_bytes),
        count(pass.kept_small + pass.kept_incompressible),
        count(pass.failed),
    );
    out
}

/// One line totalling what a directory of an archive holds.
fn tally_line(dir: &str, tally: &Tally) -> String {
    let mut line = format!(
        "{}: {} ({}) in {}; {} ({}), as equal files are stored once",
        if dir.trim_matches('/').is_empty() {
            "whole archive"
        } else {
            dir
        },
        counted(tally.files, "file", "files"),
        size(tally.bytes),
        counted(tally.dirs, "directory", "directories"),
        counted(tally.objects, "distinct object", "distinct objects"),
        size(tally.object_bytes),
    );
    for (n, what) in [
        (tally.links, "symlinks"),
        (tally.unreadable, "entries the capture could not read"),
        (tally.excluded, "entries excluded by the owner"),
    ] {
        if n > 0 {
            let _ = write!(line, "; {} {what}", count(n));
        }
    }
    line.push('\n');
    line
}

/// A date for a Unix time, UTC.
fn date(secs: u64) -> String {
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    // Civil-from-days (Howard Hinnant), proleptic Gregorian.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year}-{month:02}-{day:02}")
}

/// Every archive on the attached drives, and the one archive currently open.
pub struct Archives {
    /// Roots given explicitly (a launcher's `--cas-root`), searched besides discovery.
    extra_roots: Vec<PathBuf>,
    /// Discovery, replaceable in tests.
    discover: fn() -> Vec<PathBuf>,
    key: Option<PublicKey>,
    /// One open archive at a time: a large manifest's index is hundreds of megabytes.
    open: RefCell<Option<(PathBuf, Rc<ArchiveView>)>>,
}

impl Archives {
    /// Archives on attached storage plus `extra_roots`; signatures checked against `key`.
    pub fn new(extra_roots: Vec<PathBuf>, key: Option<PublicKey>) -> Self {
        Self {
            extra_roots,
            discover: data::cli::discover::scan_for_cas_roots,
            key,
            open: RefCell::new(None),
        }
    }

    /// The Archive CAS roots on attached drives now, each once.
    pub fn roots(&self) -> Vec<PathBuf> {
        let mut roots = self.extra_roots.clone();
        roots.extend((self.discover)());
        data::cli::dedup_by_canonical_path(roots)
    }

    /// Every current (not superseded) archive, per root, and the roots that could not
    /// be read.
    fn current(&self) -> (Vec<ArchiveListing>, Vec<String>) {
        let mut listings = Vec::new();
        let mut unreadable = Vec::new();
        for root in self.roots() {
            match list_archives(&root, self.key.as_ref()) {
                Ok(found) => listings.extend(found.into_iter().filter(|l| !l.superseded)),
                Err(error) => unreadable.push(format!("{}: {error:#}", root.display())),
            }
        }
        (listings, unreadable)
    }

    /// The name a model uses for `listing`: its archive id, or `id@drive` when two
    /// drives hold archives with the same id.
    fn name(listing: &ArchiveListing, all: &[ArchiveListing]) -> String {
        if all
            .iter()
            .filter(|l| l.archive_id == listing.archive_id)
            .count()
            > 1
        {
            format!("{}@{}", listing.archive_id, drive(&listing.cas_root))
        } else {
            listing.archive_id.clone()
        }
    }

    /// Opens archive `name` (from `cas_archives`), or returns it if already open.
    fn view(&self, name: &str) -> Result<Rc<ArchiveView>, String> {
        let (all, _) = self.current();
        let Some(listing) = all.iter().find(|l| Self::name(l, &all) == name) else {
            let names: Vec<String> = all.iter().map(|l| Self::name(l, &all)).collect();
            return Err(format!(
                "no archive named {name:?}; archives on the attached drives: {}",
                if names.is_empty() {
                    "none".to_string()
                } else {
                    names.join(", ")
                }
            ));
        };
        if let Some((path, view)) = self.open.borrow().as_ref() {
            if *path == listing.manifest_path {
                return Ok(Rc::clone(view));
            }
        }
        // Release the previous index before building the next.
        self.open.borrow_mut().take();
        let view = Rc::new(
            ArchiveView::open(&listing.cas_root, &listing.manifest_path, self.key.as_ref())
                .map_err(|e| format!("{e:#}"))?,
        );
        *self.open.borrow_mut() = Some((listing.manifest_path.clone(), Rc::clone(&view)));
        Ok(view)
    }

    /// The archive a tool call names, and its identity line for the result.
    fn with_view(&self, args: &Value) -> Result<(Rc<ArchiveView>, String), String> {
        let name = str_arg(args, "archive")?;
        let view = self.view(name)?;
        let header = format!(
            "archive {name} root {} {}\n",
            view.root().to_hex(),
            view.signature().describe()
        );
        Ok((view, header))
    }
}

/// The five tools over the archives on the attached drives.
pub fn cas_tools(archives: Archives) -> Vec<Box<dyn Tool>> {
    let archives = Rc::new(archives);
    vec![
        Box::new(CasArchives(archives.clone())),
        Box::new(CasList(archives.clone())),
        Box::new(CasFind(archives.clone())),
        Box::new(CasRead(archives.clone())),
        Box::new(CasGrep(archives)),
    ]
}

struct CasArchives(Rc<Archives>);
struct CasList(Rc<Archives>);
struct CasFind(Rc<Archives>);
struct CasRead(Rc<Archives>);
struct CasGrep(Rc<Archives>);

fn archive_param() -> Value {
    json!({"type": "string", "description": "an archive name from cas_archives"})
}

impl Tool for CasArchives {
    fn name(&self) -> &'static str {
        "cas_archives"
    }
    fn description(&self) -> &'static str {
        "List every Archive CAS archive on the attached drives, as the Archive CAS browser shows them: each drive, its compression setting and how far its last compression pass got (objects done and left), and each archive's name, label, date and whether it is signed. Use a name with cas_list (path \"\" gives the archive's file and object counts), cas_find, cas_read and cas_grep."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }
    fn call(&self, _: &Value) -> Result<String, String> {
        let (all, unreadable) = self.0.current();
        let mut out = String::new();
        let now = unix_now();
        for root in self.0.roots() {
            let on_root: Vec<&ArchiveListing> = all.iter().filter(|l| l.cas_root == root).collect();
            let _ = writeln!(
                out,
                "drive {} ({}): {} archives",
                drive(&root),
                root.display(),
                on_root.len()
            );
            out.push_str(&store_status(&root, now));
            for listing in on_root {
                let _ = writeln!(
                    out,
                    "  {}  \"{}\"  {}  {}",
                    Archives::name(listing, &all),
                    listing.source_label,
                    date(listing.created_at_unix_secs),
                    listing.signature.describe()
                );
            }
        }
        for problem in unreadable {
            let _ = writeln!(out, "unreadable: {problem}");
        }
        if out.is_empty() {
            out.push_str("no Archive CAS drives are attached\n");
        }
        Ok(out)
    }
}

impl Tool for CasList {
    fn name(&self) -> &'static str {
        "cas_list"
    }
    fn description(&self) -> &'static str {
        "List a directory in an archive (verified, read-only): first a total of everything below it (files, bytes, directories, distinct objects), then each entry, with the files and bytes below each directory. path \"\" totals the whole archive. cas_read gives each file's object hash."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "archive": archive_param(),
            "path": {"type": "string", "description": "directory inside the archive; \"\" is its root"}},
            "required": ["archive", "path"]})
    }
    fn call(&self, args: &Value) -> Result<String, String> {
        let (view, mut out) = self.0.with_view(args)?;
        let path = str_arg(args, "path")?;
        let children = view.list(path).map_err(|e| format!("{e:#}"))?;
        let tally = view.tally(path).map_err(|e| format!("{e:#}"))?;
        out.push_str(&tally_line(path, &tally));
        // Kinds and sizes only: the root above identifies every entry, and per-entry
        // hashes would triple the prompt tokens a listing costs the model.
        for child in children.iter().take(LIST_ENTRIES) {
            match child.object {
                Some(o) => {
                    let _ = writeln!(out, "{} {:>10}  {}", child.kind, o.size, child.name);
                }
                None => match tally.subdirs.get(&child.name) {
                    Some(&(files, bytes)) if child.kind == "dir" => {
                        let _ = writeln!(
                            out,
                            "dir  {:>9} files {:>12}  {}",
                            count(files),
                            size(bytes),
                            child.name
                        );
                    }
                    _ => {
                        let _ = writeln!(out, "{}  {}", child.kind, child.name);
                    }
                },
            }
        }
        if children.len() > LIST_ENTRIES {
            let rest = &children[LIST_ENTRIES..];
            let files = rest.iter().filter(|c| c.object.is_some()).count();
            let bytes: u64 = rest.iter().filter_map(|c| c.object).map(|o| o.size).sum();
            let _ = writeln!(
                out,
                "[{} more: {files} files ({bytes} bytes), {} other; use cas_find with a pattern to see them]",
                rest.len(),
                rest.len() - files
            );
        }
        Ok(out)
    }
}

impl Tool for CasFind {
    fn name(&self) -> &'static str {
        "cas_find"
    }
    fn description(&self) -> &'static str {
        "Find files in an archive whose path matches a glob (* within a directory, ** across). Shows the first 100 and counts every match: files, bytes and distinct objects."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "archive": archive_param(),
            "pattern": {"type": "string", "description": "e.g. loadngo/**/*.rs or **/*.conf"}},
            "required": ["archive", "pattern"]})
    }
    fn call(&self, args: &Value) -> Result<String, String> {
        let (view, mut out) = self.0.with_view(args)?;
        let pattern = str_arg(args, "pattern")?;
        let (mut matches, mut bytes) = (0, 0);
        let mut objects = HashSet::new();
        for (path, object) in view.files() {
            if glob_match(pattern, path) {
                matches += 1;
                bytes += object.size;
                objects.insert(object.hash);
                if matches <= MAX_MATCHES {
                    let _ = writeln!(
                        out,
                        "{path}  {} bytes  object {}",
                        object.size,
                        object.hash.to_hex()
                    );
                }
            }
        }
        if matches > MAX_MATCHES {
            let _ = writeln!(
                out,
                "[shown: the first {MAX_MATCHES}; narrow the pattern to see the others]"
            );
        }
        let _ = writeln!(
            out,
            "{} matches ({}), {} distinct objects",
            count(matches),
            size(bytes),
            count(objects.len())
        );
        Ok(out)
    }
}

impl Tool for CasRead {
    fn name(&self) -> &'static str {
        "cas_read"
    }
    fn description(&self) -> &'static str {
        "Read a text file from an archive, BLAKE3-verified, with line numbers. At most 16 KiB per call."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "archive": archive_param(),
            "path": {"type": "string"},
            "line_start": {"type": "integer", "description": "first line, 1-based (default 1)"},
            "line_count": {"type": "integer", "description": "number of lines (default 400)"}},
            "required": ["archive", "path"]})
    }
    fn call(&self, args: &Value) -> Result<String, String> {
        let (view, header) = self.0.with_view(args)?;
        let path = str_arg(args, "path")?;
        let (bytes, object) = view.read_file(path).map_err(|e| format!("{e:#}"))?;
        let text = as_text(&bytes).ok_or_else(|| {
            format!(
                "{path} is binary ({} bytes, object {})",
                object.size,
                object.hash.to_hex()
            )
        })?;
        Ok(format!(
            "{header}{path}  {} bytes  object {} (verified)\n{}",
            object.size,
            object.hash.to_hex(),
            numbered_lines(
                text,
                usize_arg(args, "line_start", 1).max(1),
                usize_arg(args, "line_count", 400).max(1)
            )
        ))
    }
}

impl Tool for CasGrep {
    fn name(&self) -> &'static str {
        "cas_grep"
    }
    fn description(&self) -> &'static str {
        "Search text files in an archive for a literal string; returns path:line: text. Bounded to 32 MiB scanned and 100 matches."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "archive": archive_param(),
            "pattern": {"type": "string"},
            "glob": {"type": "string", "description": "only paths matching, e.g. loadngo/**/*.rs"}},
            "required": ["archive", "pattern"]})
    }
    fn call(&self, args: &Value) -> Result<String, String> {
        let (view, mut out) = self.0.with_view(args)?;
        let pattern = str_arg(args, "pattern")?;
        if pattern.is_empty() {
            return Err("empty pattern".into());
        }
        let filter = args.get("glob").and_then(Value::as_str);
        let (mut matches, mut scanned) = (0, 0_u64);
        'files: for (path, object) in view.files() {
            if object.size > MAX_GREP_FILE_BYTES || filter.is_some_and(|g| !glob_match(g, path)) {
                continue;
            }
            let Ok(bytes) = view.read_object(object) else {
                continue;
            };
            scanned += object.size;
            if let Some(text) = as_text(&bytes) {
                for (i, line) in text.lines().enumerate() {
                    if line.contains(pattern) {
                        let shown: String = line.chars().take(240).collect();
                        let _ = writeln!(out, "{path}:{}: {shown}", i + 1);
                        matches += 1;
                        if matches >= MAX_MATCHES {
                            break 'files;
                        }
                    }
                }
            }
            if scanned >= MAX_SCAN_BYTES {
                break;
            }
        }
        let _ = writeln!(
            out,
            "{matches} matches ({scanned} bytes scanned, every file verified)"
        );
        if matches >= MAX_MATCHES || scanned >= MAX_SCAN_BYTES {
            out.push_str("[stopped at the search limit; add a glob]\n");
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::Toolbox;
    use data::archive_cas::{ArchiveCasStorage, ArchiveEntry, ArchiveManifest};

    fn toolbox(archives: Archives) -> Toolbox {
        let mut tools = Toolbox::default();
        for tool in cas_tools(archives) {
            tools.push(tool);
        }
        tools
    }

    fn file(store: &ArchiveCasStorage, path: &str, bytes: &[u8]) -> ArchiveEntry {
        ArchiveEntry::File {
            path: path.into(),
            object: store.add_content(bytes).unwrap().object,
            modified_at_unix_secs: None,
        }
    }

    fn no_discovery() -> Vec<PathBuf> {
        Vec::new()
    }

    #[test]
    fn archives_on_every_root_are_listed_and_read_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a/cas"), dir.path().join("b/cas"));
        let store_a = ArchiveCasStorage::new(&a).unwrap();
        let docs = ArchiveManifest::new(
            "docs",
            "Documents",
            86_400,
            vec![file(
                &store_a,
                "notes/wg.conf",
                b"[Interface]\nAddress = 10.0.0.2\n",
            )],
        )
        .unwrap();
        store_a.write_manifest(&docs).unwrap();
        let store_b = ArchiveCasStorage::new(&b).unwrap();
        let card = ArchiveManifest::new(
            "card",
            "SD card",
            1,
            vec![file(&store_b, "card.img", &[0, 1, 2])],
        )
        .unwrap();
        store_b.write_manifest(&card).unwrap();

        let mut archives = Archives::new(vec![a.clone(), b.clone()], None);
        archives.discover = no_discovery;
        let tools = toolbox(archives);
        let listed = tools.call("cas_archives", "{}").unwrap();
        assert!(
            listed.contains("docs  \"Documents\"  1970-01-02  unsigned"),
            "{listed}"
        );
        assert!(listed.contains("card  \"SD card\""), "{listed}");

        let read = tools
            .call(
                "cas_read",
                r#"{"archive": "docs", "path": "notes/wg.conf"}"#,
            )
            .unwrap();
        assert!(read.starts_with("archive docs root "), "{read}");
        assert!(
            read.contains("unsigned") && read.contains("(verified)"),
            "{read}"
        );
        assert!(read.contains("Address = 10.0.0.2"), "{read}");
        let found = tools
            .call("cas_find", r#"{"archive": "card", "pattern": "*.img"}"#)
            .unwrap();
        assert!(found.contains("card.img  3 bytes"), "{found}");
        let missing = tools
            .call("cas_list", r#"{"archive": "pudding", "path": ""}"#)
            .unwrap_err();
        assert!(
            missing.contains("card") && missing.contains("docs"),
            "{missing}"
        );
    }

    #[test]
    fn a_superseded_version_is_not_listed_and_the_same_id_on_two_drives_is_named_apart() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a/cas"), dir.path().join("b/cas"));
        for root in [&a, &b] {
            let store = ArchiveCasStorage::new(root).unwrap();
            let v1 =
                ArchiveManifest::new("docs", "Docs", 1, vec![file(&store, "x", b"x")]).unwrap();
            store.write_manifest(&v1).unwrap();
            if root == &a {
                let (v2, _) = v1
                    .with_entries_removed(&["x".to_string()], "test", "jay", 2)
                    .unwrap();
                store.write_manifest(&v2).unwrap();
            }
        }
        let mut archives = Archives::new(vec![a.clone(), b.clone()], None);
        archives.discover = no_discovery;
        let (current, _) = archives.current();
        assert_eq!(current.len(), 2, "one current version per root");
        let names: Vec<String> = current
            .iter()
            .map(|l| Archives::name(l, &current))
            .collect();
        assert!(names.iter().all(|n| n.starts_with("docs@")), "{names:?}");
        assert!(archives.view("docs").is_err(), "ambiguous without a drive");
        assert!(archives.view(&names[0]).is_ok());
    }

    #[test]
    fn a_long_listing_shows_fifty_entries_and_summarises_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("cas");
        let store = ArchiveCasStorage::new(&root).unwrap();
        let entries = (0..80)
            .map(|i| file(&store, &format!("f{i:02}.txt"), format!("{i}").as_bytes()))
            .collect();
        store
            .write_manifest(&ArchiveManifest::new("many", "Many", 1, entries).unwrap())
            .unwrap();
        let mut archives = Archives::new(vec![root], None);
        archives.discover = no_discovery;
        let listed = toolbox(archives)
            .call("cas_list", r#"{"archive": "many", "path": ""}"#)
            .unwrap();
        assert!(
            listed.contains("f49.txt") && !listed.contains("f50.txt"),
            "{listed}"
        );
        assert!(
            listed.contains("[30 more: 30 files (60 bytes), 0 other"),
            "{listed}"
        );
    }

    #[test]
    fn listings_and_finds_count_everything_and_drives_show_compression_progress() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("cas");
        let mut store = ArchiveCasStorage::new(&root).unwrap();
        let mut entries = vec![ArchiveEntry::Directory {
            path: "logs".into(),
            modified_at_unix_secs: None,
        }];
        // 120 files, but only 3 distinct contents.
        entries.extend((0..120).map(|i| {
            file(
                &store,
                &format!("logs/{i:03}.log"),
                format!("{}", i % 3).as_bytes(),
            )
        }));
        entries.push(file(&store, "readme.txt", b"hello"));
        store
            .write_manifest(&ArchiveManifest::new("logs", "Logs", 1, entries).unwrap())
            .unwrap();
        store.set_compression(Some(3)).unwrap();
        let mut archives = Archives::new(vec![root.clone()], None);
        archives.discover = no_discovery;
        let tools = toolbox(archives);

        let listed = tools
            .call("cas_list", r#"{"archive": "logs", "path": ""}"#)
            .unwrap();
        assert!(
            listed.contains(
                "whole archive: 121 files (125 bytes) in 1 directory; 4 distinct objects (8 bytes)"
            ),
            "{listed}"
        );
        assert!(
            listed.contains("dir        120 files    120 bytes  logs"),
            "{listed}"
        );
        let inside = tools
            .call("cas_list", r#"{"archive": "logs", "path": "logs"}"#)
            .unwrap();
        assert!(inside.contains("logs: 120 files"), "{inside}");

        let found = tools
            .call("cas_find", r#"{"archive": "logs", "pattern": "**/*.log"}"#)
            .unwrap();
        assert!(found.contains("logs/099.log") && !found.contains("logs/100.log"));
        assert!(
            found.contains("[shown: the first 100;")
                && found.contains("120 matches (120 bytes), 3 distinct objects"),
            "{found}"
        );

        let drives = tools.call("cas_archives", "{}").unwrap();
        assert!(
            drives.contains("compression: new objects stored with zstd level 3"),
            "{drives}"
        );
        assert!(!drives.contains("compression pass"), "{drives}");
        let now = unix_now();
        let pass = |updated: u64, finished: bool| {
            format!(
                r#"{{"level": 3, "pid": 7, "started_at_unix_secs": {}, "updated_at_unix_secs": {updated},
                "finished": {finished}, "objects": 700000, "examined": 400000, "examined_bytes": 1,
                "compressed": 120000, "saved_bytes": 199400000000, "kept_small": 250000,
                "kept_incompressible": 30000, "failed": 0, "already_compressed": 61031}}"#,
                now - 50_000
            )
        };
        let progress = root.join(data::archive_cas_compress::PROGRESS_FILE);
        std::fs::write(&progress, pass(now - 5, false)).unwrap();
        let running = tools.call("cas_archives", "{}").unwrap();
        assert!(
            running.contains("started 13 h 53 min ago, running (pid 7, updated 5 s ago): 461,031 of 700,000 stored objects done (65.9%), 238,969 left; 120,000 compressed, saving 185.71 GiB; 280,000 kept as they are; 0 failed"),
            "{running}"
        );
        std::fs::write(&progress, pass(now - 7_200, false)).unwrap();
        let stopped = tools.call("cas_archives", "{}").unwrap();
        assert!(
            stopped.contains("stopped without finishing (no update for 2 h 0 min): 461,031 of")
                && stopped.contains("0 failed; running archive_cas_compress again resumes it"),
            "{stopped}"
        );
    }

    #[test]
    fn counts_and_sizes_read_the_way_people_say_them() {
        assert_eq!(count(0), "0");
        assert_eq!(count(999), "999");
        assert_eq!(count(1_000), "1,000");
        assert_eq!(count(461_031), "461,031");
        assert_eq!(count(12_345_678), "12,345,678");
        assert_eq!(counted(1, "file", "files"), "1 file");
        assert_eq!(size(1_023), "1023 bytes");
        assert_eq!(size(1_536), "1.50 KiB");
        assert_eq!(size(733 << 30), "733.00 GiB");
        assert_eq!(span(42), "42 s");
        assert_eq!(span(900), "15 min");
        assert_eq!(span(49_759), "13 h 49 min");
        assert_eq!(span(3 * 86_400), "3 days");
    }

    #[test]
    fn drives_are_named_by_volume() {
        assert_eq!(
            drive(Path::new("/Volumes/Zhoenus II/pudding-cas")),
            "Zhoenus II"
        );
        assert_eq!(drive(Path::new("/run/media/jay/Backup/cas")), "Backup");
        assert_eq!(drive(Path::new("/srv/cas")), "/srv/cas");
        assert_eq!(date(1_790_516_905), "2026-09-27");
    }

    /// `cargo test --release -p loadngo-inference --features cas -- --ignored --nocapture`
    #[test]
    #[ignore = "needs the Archive CAS drives attached (Zhoenus II, Loadngo Archive Staging)"]
    fn real_drives_list_every_archive_and_read_the_pudding_snapshot() {
        let key = data::archive_cas_sign::read_public_key(Path::new(
            "/Volumes/Zhoenus II/pudding-cas/keys/jay-macmini.dilithium2.pub",
        ))
        .unwrap();
        let tools = toolbox(Archives::new(Vec::new(), Some(key)));
        let listed = tools.call("cas_archives", "{}").unwrap();
        eprintln!("{listed}");
        assert!(listed.contains("pudding-20260917"), "{listed}");
        // dolores-card-20260916 and untitled-documents-20260917 were merged into
        // loadngo-archive on 2026-09-28.
        assert!(listed.contains("loadngo-archive"), "{listed}");
        let start = std::time::Instant::now();
        let read = tools
            .call(
                "cas_read",
                r#"{"archive": "pudding-20260917", "path": "loadngo/proactor/src/lib.rs", "line_count": 3}"#,
            )
            .unwrap();
        eprintln!("opened and read in {:?}\n{read}", start.elapsed());
        assert!(
            read.contains("signed by") && read.contains("(verified)"),
            "{read}"
        );
        let listed = tools
            .call("cas_list", r#"{"archive": "pudding-20260917", "path": ""}"#)
            .unwrap();
        eprintln!("{listed}");
        assert!(listed.contains("whole archive: 157,874 files"), "{listed}");
    }
}
