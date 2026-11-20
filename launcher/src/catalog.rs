//! The catalog: what the launcher lists, read from one JSON file.
//!
//! ```json
//! {
//!   "entries": [
//!     {
//!       "name": "Code Editor",
//!       "group": "loadngo",
//!       "about": "Edit a folder of source",
//!       "build": { "program": "cargo", "args": ["build", "--release", "-p", "loadngo-code-editor"],
//!                  "dir": "~/src/loadngo" },
//!       "run": { "program": "~/src/loadngo/target/release/code_editor" }
//!     },
//!     {
//!       "name": "Docs site",
//!       "group": "Pages",
//!       "run": { "program": "npm", "args": ["run", "dev", "--", "--port", "5173", "--strictPort"],
//!                "dir": "~/src/docs-site" },
//!       "open": "http://localhost:5173/",
//!       "ready": "Local:"
//!     },
//!     { "name": "Project board", "open": "https://example.org/board" }
//!   ]
//! }
//! ```
//!
//! An entry has up to three parts, used in this order: `build` (a command
//! that must finish first), `run` (the program) and `open` (a page, file or
//! folder to show). What the parts mean together is [`Kind`].

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// What starting an entry does, from the parts it has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `run` alone: a program with a window of its own. It is started and
    /// left running when the launcher quits; its output goes to a log file.
    Program,
    /// `run` and `open`: a program that serves a page. The page is opened
    /// once the program's output has shown `ready` (at once without one),
    /// and the program is stopped when the launcher quits.
    Service,
    /// `open` alone: a page, file or folder opened with the system's own
    /// opener.
    Link,
    /// Built into the launcher itself and run in its window, which it has
    /// to itself until it is left. The catalog file cannot say this: the
    /// launcher's own build says what it holds
    /// ([`Entry::built_in`]).
    BuiltIn,
}

/// A program to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    /// A path, or a bare name looked up on the user's shell `PATH`.
    pub program: String,
    pub args: Vec<String>,
    /// The folder it starts in; the launcher's own when `None`.
    pub dir: Option<PathBuf>,
    /// Variables added to its environment.
    pub env: Vec<(String, String)>,
}

impl Command {
    /// The command as it would be typed, for showing.
    pub fn display(&self) -> String {
        let mut line = self.program.clone();
        for arg in &self.args {
            line.push(' ');
            if arg.is_empty() || arg.contains(' ') {
                line.push('"');
                line.push_str(arg);
                line.push('"');
            } else {
                line.push_str(arg);
            }
        }
        line
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    /// The heading it is listed under; empty for none.
    pub group: String,
    /// One line on what it is; may be empty.
    pub about: String,
    pub build: Option<Command>,
    pub run: Option<Command>,
    pub open: Option<String>,
    pub ready: Option<String>,
    /// It is part of the launcher's own build, not of the catalog file.
    pub built_in: bool,
}

impl Entry {
    /// An entry for something built into the launcher, listed under
    /// `group`.
    pub fn built_in(name: &str, group: &str, about: &str) -> Self {
        Self {
            name: name.to_string(),
            group: group.to_string(),
            about: about.to_string(),
            build: None,
            run: None,
            open: None,
            ready: None,
            built_in: true,
        }
    }

    pub fn kind(&self) -> Kind {
        if self.built_in {
            return Kind::BuiltIn;
        }
        match (&self.run, &self.open) {
            (Some(_), Some(_)) => Kind::Service,
            (Some(_), None) => Kind::Program,
            // `parse` refuses an entry with neither.
            (None, _) => Kind::Link,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Catalog {
    pub entries: Vec<Entry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCatalog {
    entries: Vec<RawEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEntry {
    name: String,
    #[serde(default)]
    group: String,
    #[serde(default)]
    about: String,
    build: Option<RawCommand>,
    run: Option<RawCommand>,
    open: Option<String>,
    ready: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCommand {
    program: String,
    #[serde(default)]
    args: Vec<String>,
    dir: Option<String>,
    #[serde(default)]
    env: std::collections::BTreeMap<String, String>,
}

/// `~` and `~/…` as the home folder, as a shell would read them.
fn expand_home(text: &str, home: Option<&Path>) -> String {
    let Some(home) = home else {
        return text.to_string();
    };
    if text == "~" {
        return home.to_string_lossy().into_owned();
    }
    match text.strip_prefix("~/") {
        Some(rest) => home.join(rest).to_string_lossy().into_owned(),
        None => text.to_string(),
    }
}

fn command(raw: RawCommand, part: &str, home: Option<&Path>) -> Result<Command, String> {
    if raw.program.trim().is_empty() {
        return Err(format!("{part}.program is empty"));
    }
    Ok(Command {
        program: expand_home(raw.program.trim(), home),
        args: raw.args.iter().map(|arg| expand_home(arg, home)).collect(),
        dir: raw
            .dir
            .filter(|dir| !dir.trim().is_empty())
            .map(|dir| PathBuf::from(expand_home(dir.trim(), home))),
        env: raw.env.into_iter().collect(),
    })
}

/// Reads a catalog. `home` is what `~` stands for in programs, arguments,
/// folders and things to open. The error says which entry is wrong and why.
pub fn parse(text: &str, home: Option<&Path>) -> Result<Catalog, String> {
    let raw: RawCatalog = serde_json::from_str(text).map_err(|error| error.to_string())?;
    let mut names = HashSet::new();
    let mut entries = Vec::with_capacity(raw.entries.len());
    for (index, raw) in raw.entries.into_iter().enumerate() {
        let name = raw.name.trim().to_string();
        let wrong = |why: String| {
            if name.is_empty() {
                format!("entry {}: {why}", index + 1)
            } else {
                format!("entry {} (\"{name}\"): {why}", index + 1)
            }
        };
        if name.is_empty() {
            return Err(wrong("it has no name".to_string()));
        }
        if !names.insert(name.clone()) {
            return Err(wrong("another entry has the same name".to_string()));
        }
        let build = raw
            .build
            .map(|raw| command(raw, "build", home))
            .transpose()
            .map_err(wrong)?;
        let run = raw
            .run
            .map(|raw| command(raw, "run", home))
            .transpose()
            .map_err(wrong)?;
        let open = raw
            .open
            .map(|open| expand_home(open.trim(), home))
            .filter(|open| !open.is_empty());
        let ready = raw.ready.filter(|ready| !ready.is_empty());
        if run.is_none() && open.is_none() {
            return Err(wrong(
                "it has nothing to start: give it \"run\", \"open\" or both".to_string(),
            ));
        }
        if build.is_some() && run.is_none() {
            return Err(wrong(
                "\"build\" needs a \"run\" to start after it".to_string(),
            ));
        }
        if ready.is_some() && (run.is_none() || open.is_none()) {
            return Err(wrong(
                "\"ready\" is for a \"run\" whose output says when \"open\" can be shown"
                    .to_string(),
            ));
        }
        entries.push(Entry {
            name,
            group: raw.group.trim().to_string(),
            about: raw.about.trim().to_string(),
            build,
            run,
            open,
            ready,
            built_in: false,
        });
    }
    Ok(Catalog { entries })
}

/// What reading the catalog file gave.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Loaded {
    Catalog(Catalog),
    /// There is no file at the catalog's path yet.
    Missing,
    /// The file is there and could not be read or understood.
    Wrong(String),
}

/// Reads the catalog at `path`. Blocks on the file; call it off the frame
/// thread.
pub fn load(path: &Path, home: Option<&Path>) -> Loaded {
    match std::fs::read_to_string(path) {
        Ok(text) => match parse(&text, home) {
            Ok(catalog) => Loaded::Catalog(catalog),
            Err(error) => Loaded::Wrong(error),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Loaded::Missing,
        Err(error) => Loaded::Wrong(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> PathBuf {
        PathBuf::from("/home/someone")
    }

    #[test]
    fn the_three_kinds_are_read_from_the_parts() {
        let catalog = parse(
            r#"{ "entries": [
                { "name": "Editor", "group": "loadngo", "about": "Edit source",
                  "build": { "program": "cargo", "args": ["build", "--release"], "dir": "~/loadngo" },
                  "run": { "program": "~/loadngo/target/release/code_editor", "args": ["~/loadngo"] } },
                { "name": "Site", "run": { "program": "npm", "args": ["run", "dev"],
                  "env": { "PORT": "5173" } },
                  "open": "http://localhost:5173/", "ready": "Local:" },
                { "name": "Notes", "open": "~/notes.md" }
            ] }"#,
            Some(&home()),
        )
        .unwrap();
        let [editor, site, notes] = &catalog.entries[..] else {
            panic!("three entries");
        };
        assert_eq!(editor.kind(), Kind::Program);
        assert_eq!(editor.group, "loadngo");
        let build = editor.build.as_ref().unwrap();
        // Joined as this system joins paths (a backslash on Windows).
        let under_home = |rest: &str| home().join(rest).to_string_lossy().into_owned();
        assert_eq!(build.dir, Some(home().join("loadngo")));
        let run = editor.run.as_ref().unwrap();
        assert_eq!(
            run.program,
            under_home("loadngo/target/release/code_editor")
        );
        assert_eq!(run.args, [under_home("loadngo")]);
        assert_eq!(site.kind(), Kind::Service);
        assert_eq!(site.ready.as_deref(), Some("Local:"));
        assert_eq!(
            site.run.as_ref().unwrap().env,
            [("PORT".to_string(), "5173".to_string())]
        );
        assert_eq!(notes.kind(), Kind::Link);
        assert_eq!(notes.open, Some(under_home("notes.md")));
    }

    #[test]
    fn a_wrong_entry_is_named_with_the_reason() {
        let wrong = |entries: &str| {
            parse(&format!(r#"{{ "entries": [{entries}] }}"#), Some(&home())).unwrap_err()
        };
        assert_eq!(
            wrong(r#"{ "name": "A", "open": "x" }, { "name": "A", "open": "y" }"#),
            "entry 2 (\"A\"): another entry has the same name"
        );
        assert!(wrong(r#"{ "name": "Empty" }"#).contains("nothing to start"));
        assert!(wrong(r#"{ "name": " ", "open": "x" }"#).starts_with("entry 1: it has no name"));
        assert!(
            wrong(r#"{ "name": "B", "build": { "program": "make" }, "open": "x" }"#)
                .contains("\"build\" needs a \"run\"")
        );
        assert!(wrong(r#"{ "name": "C", "open": "x", "ready": "up" }"#).contains("\"ready\""));
        assert!(
            wrong(r#"{ "name": "D", "run": { "program": " " } }"#).contains("run.program is empty")
        );
        // A misspelt field is refused, with where it is.
        assert!(wrong(r#"{ "name": "E", "opne": "x" }"#).contains("unknown field `opne`"));
    }

    #[test]
    fn a_command_is_shown_as_typed() {
        let command = Command {
            program: "npm".to_string(),
            args: vec!["run".to_string(), "two words".to_string(), String::new()],
            dir: None,
            env: Vec::new(),
        };
        assert_eq!(command.display(), "npm run \"two words\" \"\"");
    }

    #[test]
    fn a_missing_file_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalog.json");
        assert_eq!(load(&path, None), Loaded::Missing);
        std::fs::write(&path, "{").unwrap();
        assert!(matches!(load(&path, None), Loaded::Wrong(_)));
        std::fs::write(&path, r#"{ "entries": [] }"#).unwrap();
        assert_eq!(load(&path, None), Loaded::Catalog(Catalog::default()));
    }
}
