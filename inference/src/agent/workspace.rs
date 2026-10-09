//! The tools and standing instructions an agent gets in Jay's workspace, the same for
//! every model: files (read-only), editing and checking under `COLLABORATION.md`, the
//! Archive CAS, notes, and the web. Also the chat's own check of the crates it changed
//! and its board handoff.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use crate::cas_tools::{cas_tools, Archives};
use crate::edit_tools::EditSession;
use crate::memory_tools::{format_notes, MemoryStore};
use crate::tools::{FsTools, Toolbox};
use crate::web_tools::WebTools;
use crate::work_tools::{self, WorkTools};

/// How long the chat's own `cargo check` or `cargo test` of a crate may run.
const VERIFY_TIMEOUT: Duration = Duration::from_secs(20 * 60);

/// What to give an agent.
pub struct WorkspaceOptions {
    /// The workspace; relative paths start here.
    pub base: PathBuf,
    /// The name the agent claims work under on the board (`gpt-oss`, `Kimi`).
    pub agent: String,
    /// The board date, `YYYY-MM-DD`.
    pub today: String,
    /// Editing (`text_*`) and checking (`cargo`, `git`) tools.
    pub edit: bool,
    /// Notes that last across conversations, kept in this file.
    pub memory: Option<PathBuf>,
    pub web: bool,
    /// Archive CAS roots beside those found on attached drives.
    pub cas_roots: Vec<PathBuf>,
    /// The public key archive signatures are checked against (else the one kept on
    /// this machine).
    pub cas_key: Option<PathBuf>,
}

/// The tools, the editing session, and the standing instructions.
pub struct Workspace {
    pub tools: Toolbox,
    pub edits: Option<Rc<EditSession>>,
    pub instructions: String,
    pub base: PathBuf,
    /// One line per kind of tool, for the terminal at startup.
    pub described: Vec<String>,
}

impl Workspace {
    /// # Errors
    /// When the workspace does not exist or the CAS key cannot be read.
    pub fn new(o: &WorkspaceOptions) -> Result<Self, String> {
        let mut tools = Toolbox::default();
        let mut described = Vec::new();
        let base = &o.base;
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let mut instructions = format!(
            "You are a local assistant on Jay's Mac mini, working in his workspace at {}: his \
             projects (each a repository), his archives and your notes. Relative paths start \
             there.\n\n\
             How to work:\n\
             - Look things up instead of guessing. Prefer what is on this machine: the workspace \
             (fs_find, fs_grep, fs_read), the archives (cas_archives, cas_grep, cas_read), your \
             notes (memory_search) and the conversation itself. Use web_search and web_fetch only \
             for what cannot be here (outside facts, recent events, other people's \
             documentation), or when Jay asks for the web.\n\
             - For an unfamiliar name, check workspace contents, archive contents and notes before \
             concluding local information is missing or going to the web. cas_archives lists \
             archive names; pass one exact name to each search, never an empty archive. cas_find \
             searches file paths ONLY, not contents; use **/*NAME* for filenames at any depth \
             (* alone does not cross directories). To find references inside files, use cas_grep \
             in each relevant archive, then cas_read matching files. A zero-match filename search \
             says nothing about file contents.\n\
             - Claim only searches and checks that actually ran, using the recorded tool \
             evidence. Earlier assistant answers are not evidence. Name the archive, pattern and \
             scope searched. Report limits, skipped files and errors; an incomplete search cannot \
             prove absence. Never claim variants, notes, other archives or tools were checked \
             unless they were.\n\
             - When you summarise or review work, state what the documents and code say, with \
             the numbers they give; do not rate work above what the evidence shows.\n",
            base.display()
        );
        described.push(format!(
            "file tools: local drive, read-only, relative to {}",
            base.display()
        ));
        for tool in FsTools::new(base, home.as_deref()).into_tools() {
            tools.push(tool);
        }
        let edits = if o.edit {
            let session = EditSession::new(base, &o.agent, &o.today)?;
            for tool in session.tools() {
                tools.push(tool);
            }
            for tool in WorkTools::new(base)?.into_tools() {
                tools.push(tool);
            }
            described.push(format!(
                "editing: text_read, text_edit, text_write, text_format in the workspace; cargo \
                 (check, test, clippy, build, fmt --check) and git (status, diff, log, show); \
                 claims on AGENT-BOARD.md as {}; nothing is committed",
                o.agent
            ));
            instructions.push_str(
                "- To change files: find the code (fs_grep searches file contents; fs_find matches \
                 file names), text_read it, change it with text_edit (text_write for a new file), \
                 and put new unit tests in the file's existing tests module. Then check: \
                 text_format each Rust file you changed, and run cargo test, clippy and fmt \
                 (--check) in the crate, and git diff to review. Report what you changed, where, \
                 and how you checked it; say plainly if a check failed or you could not finish.\n\
                 - Change only what the request needs. You cannot commit, push or run other \
                 commands: Jay reviews your changes. A file another agent is working on is \
                 refused; tell Jay instead of working around it.\n",
            );
            Some(session)
        } else {
            described.push("editing: off".into());
            instructions.push_str("- You can read but not change files.\n");
            None
        };
        instructions
            .push_str("- Answer briefly and concretely, and name the files you read or changed.");

        let key = match &o.cas_key {
            Some(path) => Some(
                data::archive_cas_sign::read_public_key(path)
                    .map_err(|e| format!("CAS key {}: {e:#}", path.display()))?,
            ),
            None => data::archive_cas_sign::default_trusted_key().ok().flatten(),
        };
        let archives = Archives::new(o.cas_roots.clone(), key);
        let roots = archives.roots();
        described.push(format!(
            "archive tools: {} Archive CAS root(s) attached ({})",
            roots.len(),
            roots
                .iter()
                .map(|r| r.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ));
        for tool in cas_tools(archives) {
            tools.push(tool);
        }

        if let Some(path) = &o.memory {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let memory = MemoryStore::new(path);
            described.push(format!(
                "memory: {} (memory_save, memory_search, memory_list, memory_forget)",
                path.display()
            ));
            instructions.push_str(
                "\n\nYou have notes that last across conversations: save facts, decisions and the \
                 state of ongoing work with memory_save when they will matter later, look them up \
                 with memory_search, and drop wrong or outdated ones with memory_forget.",
            );
            match memory.recall(4096) {
                Ok(notes) if !notes.is_empty() => {
                    instructions.push_str(&format!(
                        " Your most recent notes:\n{}",
                        format_notes(&notes)
                    ));
                }
                Ok(_) => instructions.push_str(" You have no notes yet."),
                Err(e) => described.push(format!("memory: cannot read the notes: {e}")),
            }
            for tool in memory.into_tools() {
                tools.push(tool);
            }
        }
        if o.web {
            described.push(
                "web tools: web_search (DuckDuckGo) and web_fetch; queries leave this machine"
                    .into(),
            );
            for tool in WebTools::new().into_tools() {
                tools.push(tool);
            }
        } else {
            described.push("web tools: off".into());
        }
        Ok(Self {
            tools,
            edits,
            instructions,
            base: base.clone(),
            described,
        })
    }
}

/// The crates holding files the session wrote: the nearest directory with a
/// `Cargo.toml` above each, inside its repository.
#[must_use]
pub fn changed_crates(base: &Path, edits: &EditSession) -> BTreeSet<PathBuf> {
    let mut crates = BTreeSet::new();
    for (area, files) in edits.written() {
        if area == "root" {
            continue;
        }
        let repo = base.join(&area);
        for file in files {
            let mut dir = repo.join(&file).parent().map(PathBuf::from);
            while let Some(at) = dir {
                if at.join("Cargo.toml").is_file() {
                    crates.insert(at);
                    break;
                }
                if at == repo {
                    break;
                }
                dir = at.parent().map(PathBuf::from);
            }
        }
    }
    crates
}

/// Checks, independently of what the model said, every crate the session changed:
/// `cargo check`, then `cargo test`, each child waiting in a loadngo proactor
/// (`work_tools::run`). Returns one line per step run and whether all passed; `None`
/// when no crate was changed.
#[must_use]
pub fn verify(base: &Path, edits: &EditSession) -> Option<(Vec<String>, bool)> {
    let crates = changed_crates(base, edits);
    if crates.is_empty() {
        return None;
    }
    let mut lines = Vec::new();
    let mut all = true;
    for dir in crates {
        let shown = dir.strip_prefix(base).unwrap_or(&dir).display().to_string();
        for step in ["check", "test"] {
            let mut command = std::process::Command::new("cargo");
            command
                .current_dir(&dir)
                .env("CARGO_TERM_COLOR", "never")
                .args([step, "-q"]);
            let passed = matches!(
                work_tools::run(command, VERIFY_TIMEOUT),
                Ok(ran) if ran.status == Some(0)
            );
            all &= passed;
            lines.push(format!(
                "cargo {step} in {shown}: {}",
                if passed { "passes" } else { "FAILS" }
            ));
            if !passed {
                break;
            }
        }
    }
    Some((lines, all))
}

/// Hands the session's edits off on the board (claims become handoffs that list the
/// uncommitted files), with the last answer and the chat's own check. Returns the files
/// left uncommitted, by area.
pub fn hand_off(
    edits: &EditSession,
    last_answer: &str,
    verified: Option<&(Vec<String>, bool)>,
) -> Vec<(String, String)> {
    let written = edits.written();
    if written.is_empty() {
        return Vec::new();
    }
    let answer: String = last_answer
        .replace(['*', '`', '#'], "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(200)
        .collect();
    let open = match verified {
        Some((checks, true)) => format!(
            "Checked by the chat: {}. Review and commit: Jay",
            checks.join("; ")
        ),
        Some((checks, false)) => format!(
            "NEEDS JAY: checked by the chat: {}. Review, fix or revert",
            checks.join("; ")
        ),
        None => "Not checked. Review and commit or revert: Jay".into(),
    };
    edits.release(&format!("Last answer: {answer}"), &open);
    written
        .into_iter()
        .flat_map(|(area, files)| files.into_iter().map(move |f| (area.clone(), f)))
        .collect()
}
