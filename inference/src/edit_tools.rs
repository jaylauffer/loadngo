//! Text editing a local model may do: `text_read`, `text_edit` and `text_write`, inside
//! one workspace and under the workspace's collaboration rules (`COLLABORATION.md`).
//!
//! - **Revisions.** Every read shows the file's revision (a BLAKE3 prefix). An edit or
//!   a replacement must start from the revision the model last read or wrote, so it
//!   never writes over a change it has not seen.
//! - **Where.** Relative paths start at the workspace; absolute paths must lie inside
//!   it. Symlinks, `..`, `.git`, build output (`target`, `node_modules`, directories
//!   marked with `CACHEDIR.TAG`), files a repository ignores, and secrets are refused.
//!   So are the workspace's own rules and coordination files (`AGENTS.md`,
//!   `CLAUDE.md`, `COLLABORATION.md`, `AGENT-BOARD.md` at its root).
//! - **Other agents' work.** A file with uncommitted changes this session did not
//!   make is someone's unfinished work and is not edited. Nor is a path another agent
//!   holds on the agent board.
//! - **Claims.** The first write in an area (a repository, or the workspace root)
//!   claims it on the board under this agent's name, listing the files written.
//!   [`EditSession::release`] hands the work off when the session ends. Nothing is
//!   committed; the changes stay for review.
//!
//! Bounded like the read-only tools: files up to 1 MiB of UTF-8, reads of at most 400
//! lines.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::rc::Rc;

use serde_json::{json, Value};

use crate::board::{self, Claim};
use crate::rust_text::{bracket_problem, outline};
use crate::tools::Tool;

/// Largest file read or written.
pub const MAX_FILE_BYTES: usize = 1024 * 1024;
/// Lines one `text_read` shows by default and at most.
const READ_LINES: usize = 400;
/// Items shown in a partial read's outline of a Rust file.
const OUTLINE_ITEMS: usize = 40;
/// The workspace-root files no tool may change.
const PROTECTED: [&str; 4] = [
    "AGENTS.md",
    "CLAUDE.md",
    "COLLABORATION.md",
    "AGENT-BOARD.md",
];
/// Directory names never written into.
const NEVER: [&str; 4] = [".git", "target", "node_modules", ".loadngo"];

/// A file's revision: the first 16 hex digits of its BLAKE3.
pub fn revision(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex()[..16].to_owned()
}

/// Where a path lies.
struct Target {
    /// Absolute path (it may not exist yet).
    path: PathBuf,
    /// Relative to the workspace, `/`-separated.
    shown: String,
    /// The area claimed for it: its repository's name, or `root`.
    area: String,
    /// The repository's root and the path within it, when inside one.
    repo: Option<(PathBuf, String)>,
}

/// What one agent's editing session has done, shared by its three tools and kept by
/// the caller to hand the work off.
pub struct EditSession {
    workspace: PathBuf,
    agent: String,
    /// What the session is for, shown in its board claim and handoff.
    task: RefCell<String>,
    /// The revision each file was last read or written at.
    seen: RefCell<HashMap<PathBuf, String>>,
    /// Files this session wrote, by area.
    written: RefCell<BTreeMap<String, BTreeSet<String>>>,
    /// Absolute paths this session wrote (their uncommitted changes are its own).
    own: RefCell<BTreeSet<PathBuf>>,
    /// Revisions each file had before this session's own writes replaced them: a model
    /// that passes one of these has missed only its own edits.
    past: RefCell<HashMap<PathBuf, BTreeSet<String>>>,
    today: String,
}

impl EditSession {
    /// A session for `agent` in `workspace` (made canonical), dated `today` on the
    /// board.
    ///
    /// # Errors
    /// When the workspace does not exist.
    pub fn new(workspace: &Path, agent: &str, today: &str) -> Result<Rc<Self>, String> {
        let workspace = workspace
            .canonicalize()
            .map_err(|e| format!("workspace {}: {e}", workspace.display()))?;
        Ok(Rc::new(Self {
            workspace,
            agent: agent.to_owned(),
            task: RefCell::new(String::new()),
            seen: RefCell::default(),
            written: RefCell::default(),
            own: RefCell::default(),
            past: RefCell::default(),
            today: today.to_owned(),
        }))
    }

    /// Sets what the work is for (the user's request), for claims and handoffs.
    pub fn set_task(&self, task: &str) {
        let mut short: String = task.chars().take(160).collect();
        if task.chars().count() > 160 {
            short.push('…');
        }
        *self.task.borrow_mut() = short;
    }

    /// The files written, by area.
    pub fn written(&self) -> BTreeMap<String, BTreeSet<String>> {
        self.written.borrow().clone()
    }

    /// `text_read`, `text_edit`, `text_write` and `text_format` for this session.
    pub fn tools(self: &Rc<Self>) -> Vec<Box<dyn Tool>> {
        vec![
            Box::new(TextRead(Rc::clone(self))),
            Box::new(TextEdit(Rc::clone(self))),
            Box::new(TextWrite(Rc::clone(self))),
            Box::new(TextFormat(Rc::clone(self))),
        ]
    }

    fn board_path(&self) -> PathBuf {
        self.workspace.join("AGENT-BOARD.md")
    }

    /// Resolves `path` for reading (`write == false`) or writing.
    fn resolve(&self, path: &str, write: bool) -> Result<Target, String> {
        let given = Path::new(path.trim());
        if path.trim().is_empty() {
            return Err("empty path".into());
        }
        let relative = if given.is_absolute() {
            given
                .strip_prefix(&self.workspace)
                .map_err(|_| {
                    format!(
                        "{path} is outside the workspace {}",
                        self.workspace.display()
                    )
                })?
                .to_path_buf()
        } else {
            given.to_path_buf()
        };
        let mut names = Vec::new();
        for component in relative.components() {
            match component {
                Component::Normal(name) => names.push(name.to_string_lossy().into_owned()),
                Component::CurDir => {}
                _ => {
                    return Err(format!(
                        "{path}: only plain names inside the workspace (no `..`)"
                    ))
                }
            }
        }
        if names.is_empty() {
            return Err(format!("{path} names the workspace itself, not a file"));
        }
        let shown = names.join("/");
        // Every existing step must be a real directory or file, not a link out.
        let mut at = self.workspace.clone();
        for (i, name) in names.iter().enumerate() {
            at.push(name);
            match fs::symlink_metadata(&at) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    return Err(format!(
                        "{shown}: {} is a symbolic link; edits stay off links",
                        names[..=i].join("/")
                    ));
                }
                Ok(meta) if i + 1 < names.len() && !meta.is_dir() => {
                    return Err(format!(
                        "{shown}: {} is not a directory",
                        names[..=i].join("/")
                    ));
                }
                Ok(meta) if meta.is_dir() && at.join("CACHEDIR.TAG").exists() && write => {
                    return Err(format!(
                        "{shown} is in build output ({})",
                        names[..=i].join("/")
                    ));
                }
                _ => {}
            }
        }
        let lower = names.last().expect("one name").to_ascii_lowercase();
        if lower.ends_with(".key")
            || lower.ends_with(".pem")
            || lower.starts_with("id_")
            || lower == ".env"
        {
            return Err(format!("{shown} looks like a secret; not touched"));
        }
        if write {
            if names.iter().any(|n| NEVER.contains(&n.as_str())) {
                return Err(format!(
                    "{shown}: .git, build output and .loadngo are not edited"
                ));
            }
            if names.len() == 1 && PROTECTED.contains(&names[0].as_str()) {
                return Err(format!(
                    "{shown} holds the workspace's rules or its coordination board; only Jay changes it"
                ));
            }
        }
        let first = self.workspace.join(&names[0]);
        let repo = if names.len() > 1 && first.join(".git").exists() {
            Some((first, names[1..].join("/")))
        } else {
            None
        };
        let area = if repo.is_some() {
            names[0].clone()
        } else {
            "root".into()
        };
        Ok(Target {
            path: at,
            shown,
            area,
            repo,
        })
    }

    /// The collaboration checks before writing `target`.
    fn may_write(&self, target: &Target) -> Result<(), String> {
        if let Some((root, inner)) = &target.repo {
            let git = |args: &[&str]| {
                Command::new("git")
                    .arg("-C")
                    .arg(root)
                    .args(args)
                    .output()
                    .map_err(|e| format!("cannot run git: {e}"))
            };
            if git(&["check-ignore", "-q", "--", inner])?.status.success() {
                return Err(format!(
                    "{} is ignored by {}; not edited",
                    target.shown, target.area
                ));
            }
            if target.path.exists() && !self.own.borrow().contains(&target.path) {
                let status = git(&[
                    "status",
                    "--porcelain=v1",
                    "--untracked-files=all",
                    "--",
                    inner,
                ])?;
                if !String::from_utf8_lossy(&status.stdout).trim().is_empty() {
                    return Err(format!(
                        "{} already has uncommitted changes this session did not make: someone's \
                         unfinished work, so it is left alone. Ask Jay.",
                        target.shown
                    ));
                }
            }
            if let Ok(text) = fs::read_to_string(self.board_path()) {
                if let Some(claim) = board::conflict(&text, &self.agent, &target.area, inner) {
                    return Err(format!(
                        "{} is claimed on the agent board by {} ({}: {}); not edited. Ask Jay.",
                        target.shown, claim.agent, claim.since, claim.task
                    ));
                }
            }
        }
        Ok(())
    }

    /// The file's text and revision.
    fn current(target: &Target) -> Result<(String, String), String> {
        let bytes = fs::read(&target.path).map_err(|e| format!("{}: {e}", target.shown))?;
        if bytes.len() > MAX_FILE_BYTES {
            return Err(format!("{} is over 1 MiB; not edited here", target.shown));
        }
        let rev = revision(&bytes);
        let text =
            String::from_utf8(bytes).map_err(|_| format!("{} is not UTF-8 text", target.shown))?;
        Ok((text, rev))
    }

    /// Checks the model replaces the file as it is now: from the revision it gave, or
    /// the one it last read or wrote. A model may pass more than the revision (the whole
    /// `text_read` header): the last 16-hex-digit word is taken.
    fn check_revision(
        &self,
        target: &Target,
        now: &str,
        given: Option<&str>,
    ) -> Result<(), String> {
        let given = given.and_then(|text| {
            text.split(|c: char| !c.is_ascii_hexdigit())
                .rfind(|w| w.len() == 16)
                .map(str::to_ascii_lowercase)
        });
        let known = given.or_else(|| self.seen.borrow().get(&target.path).cloned());
        match known {
            None => Err(format!(
                "read {} with text_read first: edits start from the revision you read",
                target.shown
            )),
            Some(known)
                if known != now
                    && self.seen.borrow().get(&target.path).is_some_and(|s| *s == now)
                    && self
                        .past
                        .borrow()
                        .get(&target.path)
                        .is_some_and(|p| p.contains(&known)) =>
            {
                // Stale only by this session's own writes since: the file is as it left it.
                Ok(())
            }
            Some(known) if known != now => Err(format!(
                "{} changed since revision {known} (it is {now} now); text_read it again before editing",
                target.shown
            )),
            Some(_) => Ok(()),
        }
    }

    /// Writes `text` to `target` through a temporary sibling, keeping permissions, then
    /// records it and claims it.
    fn commit(&self, target: &Target, text: &str) -> Result<String, String> {
        if text.len() > MAX_FILE_BYTES || text.contains('\0') {
            return Err("the text must be under 1 MiB with no NUL bytes".into());
        }
        if let Ok(before) = fs::read(&target.path) {
            let before = String::from_utf8_lossy(&before);
            if target.shown.ends_with(".rs") {
                if let (None, Some(problem)) = (bracket_problem(&before), bracket_problem(text)) {
                    return Err(format!(
                        "not written: {}'s brackets balance now and would not after this change: in \
                         the changed file, {problem}. Fix the change (an edit's new_text should open \
                         and close the same brackets it replaces) and try again",
                        target.shown
                    ));
                }
            }
            self.past
                .borrow_mut()
                .entry(target.path.clone())
                .or_default()
                .insert(revision(before.as_bytes()));
        }
        let parent = target.path.parent().ok_or("no parent directory")?;
        fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        let name = target
            .path
            .file_name()
            .ok_or("no file name")?
            .to_string_lossy();
        let temporary = parent.join(format!(".{name}.edit-{}.tmp", std::process::id()));
        fs::write(&temporary, text).map_err(|e| format!("{}: {e}", target.shown))?;
        if let Ok(meta) = fs::metadata(&target.path) {
            let _ = fs::set_permissions(&temporary, meta.permissions());
        }
        fs::rename(&temporary, &target.path).map_err(|e| {
            let _ = fs::remove_file(&temporary);
            format!("{}: {e}", target.shown)
        })?;
        let rev = revision(text.as_bytes());
        self.seen
            .borrow_mut()
            .insert(target.path.clone(), rev.clone());
        self.own.borrow_mut().insert(target.path.clone());
        let inner = target
            .repo
            .as_ref()
            .map_or(target.shown.clone(), |(_, inner)| inner.clone());
        let fresh = self
            .written
            .borrow_mut()
            .entry(target.area.clone())
            .or_default()
            .insert(inner);
        if fresh {
            self.claim(&target.area);
        }
        Ok(rev)
    }

    /// Claims (or widens the claim on) `area` with the files written there.
    fn claim(&self, area: &str) {
        let path = self.board_path();
        let Ok(text) = fs::read_to_string(&path) else {
            return;
        };
        let files = self.written.borrow().get(area).cloned().unwrap_or_default();
        let claim = Claim {
            since: self.today.clone(),
            agent: self.agent.clone(),
            area: area.to_owned(),
            paths: files
                .iter()
                .map(|f| format!("`{f}`"))
                .collect::<Vec<_>>()
                .join(", "),
            task: format!("{} (local model, for Jay)", self.task.borrow()),
        };
        if let Some(updated) = board::set_claim(&text, &claim) {
            let _ = fs::write(&path, updated);
        }
    }

    /// Ends the session on the board: each claim becomes a handoff that lists the
    /// files left uncommitted, with `summary` (what was done) and `open` (what is left).
    pub fn release(&self, summary: &str, open: &str) {
        let path = self.board_path();
        let Ok(mut text) = fs::read_to_string(&path) else {
            return;
        };
        for (area, files) in self.written.borrow().iter() {
            text = board::remove_claim(&text, &self.agent, area);
            let listed = files
                .iter()
                .map(|f| format!("`{f}`"))
                .collect::<Vec<_>>()
                .join(", ");
            if let Some(updated) = board::add_handoff(
                &text,
                &self.today,
                &self.agent,
                &format!("{} {summary}", self.task.borrow()),
                &format!("{area}: {listed} (uncommitted)"),
                open,
            ) {
                text = updated;
            }
        }
        let _ = fs::write(&path, text);
    }
}

fn string<'a>(args: &'a Value, name: &str) -> Result<&'a str, String> {
    args.get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing string argument `{name}`"))
}

/// Lines `first..=last` (1-based) of `text`, numbered as `text_read` shows them.
fn numbered(text: &str, first: usize, last: usize) -> String {
    let mut out = String::new();
    for (i, line) in text
        .lines()
        .enumerate()
        .skip(first.saturating_sub(1))
        .take(last + 1 - first.max(1))
    {
        let _ = writeln!(out, "{:5}|{line}", i + 1);
    }
    out
}

/// `text` with `text_read`'s gutter (`   12|`: digits right-aligned in at least five
/// columns, then `|`) taken off every line that has one, or `None` when no line has
/// one. A model copying from a read keeps the numbers, sometimes on only some lines.
fn without_gutter(text: &str) -> Option<String> {
    let gutter = |line: &str| -> Option<usize> {
        let digits = line.trim_start();
        let start = line.len() - digits.len();
        let n = digits.chars().take_while(char::is_ascii_digit).count();
        (n > 0 && start + n >= 5 && digits[n..].starts_with('|')).then_some(start + n + 1)
    };
    let lines: Vec<&str> = text.split('\n').collect();
    if !lines.iter().any(|l| gutter(l).is_some()) {
        return None;
    }
    Some(
        lines
            .iter()
            .map(|l| gutter(l).map_or(*l, |at| &l[at..]))
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

struct TextRead(Rc<EditSession>);
struct TextEdit(Rc<EditSession>);
struct TextWrite(Rc<EditSession>);
struct TextFormat(Rc<EditSession>);

/// The Rust edition of the crate holding `file`: the nearest `Cargo.toml` that states
/// one (a member using `edition.workspace` takes its workspace's), `2021` otherwise.
fn edition(file: &Path, stop: &Path) -> String {
    let stated = |text: &str| -> Option<String> {
        text.lines().find_map(|line| {
            let rest = line.trim().strip_prefix("edition")?.trim_start();
            let value = rest.strip_prefix('=')?.trim().trim_matches('"');
            value
                .chars()
                .all(|c| c.is_ascii_digit())
                .then(|| value.to_owned())
        })
    };
    let mut dir = file.parent();
    while let Some(at) = dir {
        if let Ok(text) = fs::read_to_string(at.join("Cargo.toml")) {
            if let Some(edition) = stated(&text) {
                return edition;
            }
        }
        if at == stop {
            break;
        }
        dir = at.parent();
    }
    "2021".into()
}

impl Tool for TextRead {
    fn name(&self) -> &'static str {
        "text_read"
    }
    fn description(&self) -> &'static str {
        "Read a text file in the workspace before editing it: numbered lines (`  12|text`) and its revision."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "path": {"type": "string", "description": "relative to the workspace, e.g. loadngo/README.md"},
            "line_start": {"type": "integer", "description": "first line, 1-based (default 1)"},
            "line_count": {"type": "integer", "description": "how many lines (default and at most 400)"}},
            "required": ["path"]})
    }
    fn call(&self, args: &Value) -> Result<String, String> {
        let target = self.0.resolve(string(args, "path")?, false)?;
        let (text, rev) = EditSession::current(&target)?;
        self.0
            .seen
            .borrow_mut()
            .insert(target.path.clone(), rev.clone());
        let total = text.lines().count();
        let first = args
            .get("line_start")
            .and_then(Value::as_u64)
            .unwrap_or(1)
            .max(1) as usize;
        let count = (args
            .get("line_count")
            .and_then(Value::as_u64)
            .unwrap_or(READ_LINES as u64) as usize)
            .clamp(1, READ_LINES);
        let last = (first + count - 1).min(total);
        let mut out = format!("{} revision {rev}, {total} lines", target.shown);
        if total == 0 {
            out.push_str(" (empty)");
            return Ok(out);
        }
        let _ = writeln!(out, "; lines {first}-{last}:");
        out.push_str(&numbered(&text, first, last));
        if last < total {
            let _ = writeln!(out, "(more: line_start {})", last + 1);
        }
        if target.shown.ends_with(".rs") && (first > 1 || last < total) {
            let _ = write!(
                out,
                "Outline of the whole file:\n{}",
                outline(&text, OUTLINE_ITEMS)
            );
        }
        Ok(out)
    }
}

impl Tool for TextEdit {
    fn name(&self) -> &'static str {
        "text_edit"
    }
    fn description(&self) -> &'static str {
        "Replace one exact piece of a text file: old_text must appear exactly once (copy it \
         from text_read or fs_read without the line numbers, with its indentation). Shows the \
         lines around the change."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "path": {"type": "string"},
            "old_text": {"type": "string", "description": "the exact text to replace, unique in the file"},
            "new_text": {"type": "string", "description": "what replaces it"},
            "revision": {"type": "string", "description": "optional: the revision text_read showed, to refuse the edit if the file changed since"}},
            "required": ["path", "old_text", "new_text"]})
    }
    fn call(&self, args: &Value) -> Result<String, String> {
        let session = &self.0;
        let target = session.resolve(string(args, "path")?, true)?;
        session.may_write(&target)?;
        let (text, now) = EditSession::current(&target)?;
        // An edit lands only where old_text appears exactly once, which shows the model
        // knows the text there; a revision, when given, must still be current.
        if args.get("revision").and_then(Value::as_str).is_some() {
            session.check_revision(&target, &now, args.get("revision").and_then(Value::as_str))?;
        }
        let mut old = string(args, "old_text")?.to_owned();
        let mut new = string(args, "new_text")?.to_owned();
        let mut note = String::new();
        if !text.contains(&old) {
            if let Some(bare) = without_gutter(&old) {
                old = bare;
                new = without_gutter(&new).unwrap_or(new);
                note = " (line numbers taken off old_text)".into();
            }
        }
        if old.is_empty() {
            return Err("old_text is empty; to write a whole file use text_write".into());
        }
        let at: Vec<usize> = text.match_indices(&old).map(|(i, _)| i).collect();
        let line_of = |byte: usize| text[..byte].matches('\n').count() + 1;
        match at.len() {
            1 => {}
            0 => {
                let first = old
                    .lines()
                    .map(str::trim)
                    .find(|l| !l.is_empty())
                    .unwrap_or("");
                let near: Vec<usize> = text
                    .lines()
                    .enumerate()
                    .filter(|(_, l)| !first.is_empty() && l.trim() == first)
                    .map(|(i, _)| i + 1)
                    .take(5)
                    .collect();
                return Err(if near.is_empty() {
                    format!(
                        "old_text is not in {}; text_read it and copy the text exactly",
                        target.shown
                    )
                } else {
                    format!(
                        "old_text is not in {} as given, but its first line is at line(s) {near:?}: the \
                         lines around it, their indentation or their whitespace differ; text_read those \
                         lines and copy them exactly",
                        target.shown
                    )
                });
            }
            n => {
                let lines: Vec<usize> = at.iter().map(|&i| line_of(i)).collect();
                return Err(format!(
                    "old_text appears {n} times in {} (lines {lines:?}); include more surrounding lines \
                     so it appears once",
                    target.shown
                ));
            }
        }
        let start = at[0];
        let edited = format!("{}{new}{}", &text[..start], &text[start + old.len()..]);
        let rev = session.commit(&target, &edited)?;
        let first = line_of(start);
        let last = first + new.matches('\n').count();
        Ok(format!(
            "edited {}{note}: revision {rev}; lines {}-{} now read:\n{}",
            target.shown,
            first.saturating_sub(2).max(1),
            last + 2,
            numbered(
                &edited,
                first.saturating_sub(2).max(1),
                (last + 2).min(edited.lines().count().max(1))
            )
        ))
    }
}

impl Tool for TextWrite {
    fn name(&self) -> &'static str {
        "text_write"
    }
    fn description(&self) -> &'static str {
        "Create a new text file, or replace a whole file you have read, in the workspace."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "path": {"type": "string"},
            "content": {"type": "string", "description": "the whole file"},
            "revision": {"type": "string", "description": "for an existing file: the revision text_read showed (default: the last one you read)"}},
            "required": ["path", "content"]})
    }
    fn call(&self, args: &Value) -> Result<String, String> {
        let session = &self.0;
        let target = session.resolve(string(args, "path")?, true)?;
        let content = string(args, "content")?;
        let existed = target.path.exists();
        session.may_write(&target)?;
        if existed {
            let (_, now) = EditSession::current(&target)?;
            session.check_revision(&target, &now, args.get("revision").and_then(Value::as_str))?;
        }
        let rev = session.commit(&target, content)?;
        Ok(format!(
            "{} {} ({} lines), revision {rev}",
            if existed { "replaced" } else { "created" },
            target.shown,
            content.lines().count()
        ))
    }
}

impl Tool for TextFormat {
    fn name(&self) -> &'static str {
        "text_format"
    }
    fn description(&self) -> &'static str {
        "Format a Rust file you created or changed in this session with rustfmt, at its \
         crate's edition (cargo fmt --check must pass before you report)."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]})
    }
    fn call(&self, args: &Value) -> Result<String, String> {
        let session = &self.0;
        let target = session.resolve(string(args, "path")?, true)?;
        if !session.own.borrow().contains(&target.path) {
            return Err(format!(
                "{} was not written in this session; only your own files are formatted",
                target.shown
            ));
        }
        if target.path.extension().and_then(|e| e.to_str()) != Some("rs") {
            return Err(format!("{} is not a Rust file", target.shown));
        }
        let (_, before) = EditSession::current(&target)?;
        let stop = target
            .repo
            .as_ref()
            .map_or(session.workspace.clone(), |(root, _)| root.clone());
        let mut command = Command::new("rustfmt");
        command
            .arg("--edition")
            .arg(edition(&target.path, &stop))
            .arg(&target.path);
        let ran = crate::work_tools::run(command, std::time::Duration::from_secs(60))?;
        if ran.status != Some(0) {
            return Err(format!(
                "rustfmt failed on {}:\n{}",
                target.shown,
                ran.stderr.trim_end()
            ));
        }
        let (_, after) = EditSession::current(&target)?;
        session
            .seen
            .borrow_mut()
            .insert(target.path.clone(), after.clone());
        Ok(if after == before {
            format!("{} was already formatted (revision {after})", target.shown)
        } else {
            format!(
                "formatted {}: revision {after}; text_read it again before editing by revision",
                target.shown
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::Toolbox;

    /// A workspace with one repository (one committed file, one with someone's
    /// uncommitted change, an ignored file) and a board where another agent holds a
    /// path.
    fn workspace(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("loadngo-edit-{}-{test}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let repo = dir.join("game");
        fs::create_dir_all(repo.join("src")).unwrap();
        fs::write(
            repo.join("src/lib.rs"),
            "fn a() {\n    one();\n}\n\nfn b() {\n    one();\n}\n",
        )
        .unwrap();
        fs::write(repo.join("src/peer.rs"), "// committed\n").unwrap();
        fs::write(repo.join(".gitignore"), "secret.txt\n").unwrap();
        fs::create_dir_all(repo.join("held")).unwrap();
        fs::write(repo.join("held/x.rs"), "// held\n").unwrap();
        let git = |args: &[&str]| {
            assert!(
                Command::new("git")
                    .arg("-C")
                    .arg(&repo)
                    .args(args)
                    .output()
                    .unwrap()
                    .status
                    .success(),
                "{args:?}"
            );
        };
        git(&["init", "-q"]);
        git(&["-c", "user.name=t", "-c", "user.email=t@t", "add", "."]);
        git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "start",
        ]);
        fs::write(repo.join("src/peer.rs"), "// someone's change\n").unwrap();
        fs::write(dir.join("AGENTS.md"), "rules\n").unwrap();
        fs::write(
            dir.join("AGENT-BOARD.md"),
            "## Active claims\n\n| Since | Agent | Repo / area | Paths | Task | Status |\n|---|---|---|---|---|---|\n\
             | today | Codex | game | `held/` | Hold | ok |\n\n## Handoffs\n\n| When | Agent | What | Where | Open |\n|---|---|---|---|---|\n",
        )
        .unwrap();
        dir
    }

    fn tools(dir: &Path) -> (Toolbox, Rc<EditSession>) {
        let session = EditSession::new(dir, "gpt-oss", "2026-10-04").unwrap();
        session.set_task("Rename one to two");
        let mut toolbox = Toolbox::default();
        for tool in session.tools() {
            toolbox.push(tool);
        }
        (toolbox, session)
    }

    #[test]
    fn edits_start_from_a_read_revision_and_must_be_unique() {
        let dir = workspace("edit");
        let (tools, _) = tools(&dir);
        // A whole-file replacement needs a read; an edit by unique old_text does not.
        let blind = tools
            .call("text_write", r#"{"path":"game/src/lib.rs","content":"x"}"#)
            .unwrap_err();
        assert!(blind.contains("text_read first"), "{blind}");
        let read = tools
            .call("text_read", r#"{"path":"game/src/lib.rs"}"#)
            .unwrap();
        assert!(read.contains("    2|    one();"), "{read}");
        let twice = tools
            .call(
                "text_edit",
                r#"{"path":"game/src/lib.rs","old_text":"    one();","new_text":"    two();"}"#,
            )
            .unwrap_err();
        assert!(
            twice.contains("2 times") && twice.contains("[2, 6]"),
            "{twice}"
        );
        // Copied with the gutter: the numbers are taken off.
        let done = tools
            .call("text_edit", r#"{"path":"game/src/lib.rs","old_text":"    1|fn a() {\n    2|    one();","new_text":"    1|fn a() {\n    2|    two();"}"#)
            .unwrap();
        assert!(done.contains("line numbers taken off"), "{done}");
        // Numbers on some lines only, as a model copies them; `1|2` in code is no gutter.
        assert_eq!(
            without_gutter("    }\n  574|    }\n  575|").as_deref(),
            Some("    }\n    }\n")
        );
        assert_eq!(without_gutter("match x { 1|2 => 0 }"), None);
        assert_eq!(
            fs::read_to_string(dir.join("game/src/lib.rs")).unwrap(),
            "fn a() {\n    two();\n}\n\nfn b() {\n    one();\n}\n"
        );
        // Its own write is its revision: a second edit needs no new read...
        tools.call("text_edit", r#"{"path":"game/src/lib.rs","old_text":"fn b() {\n    one();","new_text":"fn b() {\n    three();"}"#).unwrap();
        // ...but a change by someone else since does, for a revision given in any form.
        let header = tools
            .call("text_read", r#"{"path":"game/src/lib.rs"}"#)
            .unwrap();
        let header = header.lines().next().unwrap().to_owned();
        fs::write(dir.join("game/src/lib.rs"), "fn changed() {}\n").unwrap();
        let stale = tools
            .call("text_edit", &json!({"path":"game/src/lib.rs","old_text":"changed","new_text":"x","revision":header}).to_string())
            .unwrap_err();
        assert!(stale.contains("changed since"), "{stale}");
        let replaced = tools
            .call("text_write", r#"{"path":"game/src/lib.rs","content":"x"}"#)
            .unwrap_err();
        assert!(replaced.contains("changed since"), "{replaced}");
        let missing = tools
            .call("text_read", r#"{"path":"game/src/lib.rs"}"#)
            .and_then(|_| {
                tools.call(
                    "text_edit",
                    r#"{"path":"game/src/lib.rs","old_text":"fn  changed","new_text":"x"}"#,
                )
            });
        assert!(missing.unwrap_err().contains("not in game/src/lib.rs"));
    }

    #[test]
    fn rust_edits_keep_brackets_balanced_and_own_edits_do_not_stale_a_revision() {
        let dir = workspace("rust");
        let (tools, _) = tools(&dir);
        let read = tools
            .call("text_read", r#"{"path":"game/src/lib.rs","line_count":3}"#)
            .unwrap();
        // A partial read of Rust source shows the whole file's items.
        assert!(
            read.contains("Outline of the whole file:\n     1|fn a() {\n     5|fn b() {"),
            "{read}"
        );
        let revision = read.split_whitespace().nth(2).unwrap().to_owned();
        let unbalanced = tools
            .call("text_edit", r#"{"path":"game/src/lib.rs","old_text":"    one();\n}\n\nfn b","new_text":"    one();\n}\n}\n\nfn b"}"#)
            .unwrap_err();
        assert!(
            unbalanced.contains("not written") && unbalanced.contains("line 4 closes nothing"),
            "{unbalanced}"
        );
        tools
            .call("text_edit", r#"{"path":"game/src/lib.rs","old_text":"fn a() {\n    one();","new_text":"fn a() {\n    two();"}"#)
            .unwrap();
        // The revision read before that edit is stale only by the session's own write.
        tools
            .call("text_edit", &json!({"path":"game/src/lib.rs","old_text":"fn b() {\n    one();","new_text":"fn b() {\n    three();","revision":revision}).to_string())
            .unwrap();
        assert_eq!(
            fs::read_to_string(dir.join("game/src/lib.rs")).unwrap(),
            "fn a() {\n    two();\n}\n\nfn b() {\n    three();\n}\n"
        );
    }

    #[test]
    fn other_agents_work_rules_and_unsafe_paths_are_refused() {
        let dir = workspace("refuse");
        let (tools, _) = tools(&dir);
        for (path, why) in [
            ("game/src/peer.rs", "uncommitted changes"),
            ("game/held/x.rs", "claimed on the agent board by Codex"),
            ("AGENTS.md", "only Jay changes it"),
            ("AGENT-BOARD.md", "only Jay changes it"),
            ("game/secret.txt", "ignored"),
            ("../outside.rs", "no `..`"),
            ("/etc/hosts", "outside the workspace"),
            ("game/target/x.rs", "build output"),
            ("game/.git/config", ".git"),
            ("game/signing.key", "secret"),
        ] {
            let error = tools
                .call(
                    "text_write",
                    &json!({"path": path, "content": "x"}).to_string(),
                )
                .unwrap_err();
            assert!(error.contains(why), "{path}: {error}");
        }
        assert!(!dir.join("game/secret.txt").exists());
    }

    #[test]
    fn writes_claim_the_area_and_release_hands_it_off() {
        let dir = workspace("claim");
        let (tools, session) = tools(&dir);
        tools
            .call(
                "text_write",
                r#"{"path":"game/src/new.rs","content":"pub fn n() {}\n"}"#,
            )
            .unwrap();
        tools
            .call("text_read", r#"{"path":"game/src/lib.rs"}"#)
            .unwrap();
        tools
            .call(
                "text_edit",
                r#"{"path":"game/src/lib.rs","old_text":"fn a() {","new_text":"fn a2() {"}"#,
            )
            .unwrap();
        let board = fs::read_to_string(dir.join("AGENT-BOARD.md")).unwrap();
        let ours: Vec<Claim> = board::claims(&board)
            .into_iter()
            .filter(|c| c.agent == "gpt-oss")
            .collect();
        assert_eq!(ours.len(), 1, "{board}");
        assert_eq!(
            (ours[0].area.as_str(), ours[0].paths.as_str()),
            ("game", "`src/lib.rs`, `src/new.rs`")
        );
        // Its own uncommitted changes do not stop it.
        tools
            .call(
                "text_edit",
                r#"{"path":"game/src/new.rs","old_text":"n()","new_text":"m()"}"#,
            )
            .unwrap();
        session.release("Done; tests pass.", "Review and commit: Jay");
        let board = fs::read_to_string(dir.join("AGENT-BOARD.md")).unwrap();
        assert!(board::claims(&board).iter().all(|c| c.agent != "gpt-oss"));
        assert!(board.contains("| 2026-10-04 | gpt-oss | Rename one to two Done; tests pass. | game: `src/lib.rs`, `src/new.rs` (uncommitted) | Review and commit: Jay |"), "{board}");
        assert_eq!(
            fs::read_to_string(dir.join("game/src/new.rs")).unwrap(),
            "pub fn m() {}\n"
        );
    }

    #[test]
    fn only_its_own_rust_files_are_formatted_at_the_crate_edition() {
        let dir = workspace("format");
        fs::write(
            dir.join("game/Cargo.toml"),
            "[package]\nname = \"game\"\nedition = \"2021\"\n",
        )
        .unwrap();
        assert_eq!(
            edition(&dir.join("game/src/lib.rs"), &dir.join("game")),
            "2021"
        );
        let (tools, _) = tools(&dir);
        let refused = tools
            .call("text_format", r#"{"path":"game/src/lib.rs"}"#)
            .unwrap_err();
        assert!(refused.contains("not written in this session"), "{refused}");
        tools
            .call(
                "text_write",
                r#"{"path":"game/src/new.rs","content":"pub fn n( ) -> u8 {1}\n"}"#,
            )
            .unwrap();
        let done = tools
            .call("text_format", r#"{"path":"game/src/new.rs"}"#)
            .unwrap();
        assert!(done.starts_with("formatted"), "{done}");
        assert_eq!(
            fs::read_to_string(dir.join("game/src/new.rs")).unwrap(),
            "pub fn n() -> u8 {\n    1\n}\n"
        );
        let again = tools
            .call("text_format", r#"{"path":"game/src/new.rs"}"#)
            .unwrap();
        assert!(again.contains("already formatted"), "{again}");
    }
}
