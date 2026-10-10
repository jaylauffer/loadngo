//! `cargo check` for the package holding a file, with its errors and
//! warnings parsed from `--message-format=json`.
//!
//! [`run`] blocks for as long as cargo runs (seconds to minutes), so the app
//! runs it on a thread of its own, never a frame or an offload worker, and
//! [`Cancel`] stops it early.

use std::collections::HashSet;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    Error,
    Warning,
}

/// One error or warning at its primary location.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Diagnostic {
    pub level: Level,
    pub file: PathBuf,
    /// One-based line and character column, as rustc reports them.
    pub line: usize,
    pub column: usize,
    pub end_line: usize,
    pub end_column: usize,
    pub message: String,
    /// The primary span's label, e.g. "expected `u32`, found `&str`".
    pub label: Option<String>,
    pub code: Option<String>,
}

/// How a check ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckOutcome {
    pub diagnostics: Vec<Diagnostic>,
    /// cargo reported the build finished successfully.
    pub success: bool,
    pub cancelled: bool,
    /// cargo could not run, or ended without its final report: why.
    pub failure: Option<String>,
    pub elapsed: Duration,
    /// The folder cargo ran in.
    pub package_dir: Option<PathBuf>,
}

impl CheckOutcome {
    /// A check that could not run at all.
    pub fn failed(reason: impl Into<String>) -> Self {
        Self {
            diagnostics: Vec::new(),
            success: false,
            cancelled: false,
            failure: Some(reason.into()),
            elapsed: Duration::ZERO,
            package_dir: None,
        }
    }
}

/// Stops a running check: kills cargo, so [`run`] returns soon after.
#[derive(Debug, Clone, Default)]
pub struct Cancel {
    child: Arc<Mutex<Option<Child>>>,
    cancelled: Arc<Mutex<bool>>,
}

impl Cancel {
    pub fn cancel(&self) {
        *self
            .cancelled
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = true;
        if let Some(child) = self
            .child
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_mut()
        {
            let _ = child.kill();
        }
    }

    fn is_cancelled(&self) -> bool {
        *self
            .cancelled
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

/// The folder of the nearest `Cargo.toml` at or above `file`'s folder.
pub fn package_dir(file: &Path) -> Option<PathBuf> {
    file.ancestors()
        .skip(1)
        .find(|dir| dir.join("Cargo.toml").is_file())
        .map(Path::to_path_buf)
}

/// `cargo`, from `PATH` or else `~/.cargo/bin` (an app opened from the
/// Finder has no shell `PATH`).
fn cargo_program() -> PathBuf {
    let on_path = std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join("cargo").is_file()));
    if on_path {
        return PathBuf::from("cargo");
    }
    std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(".cargo/bin/cargo"))
        .filter(|cargo| cargo.is_file())
        .unwrap_or_else(|| PathBuf::from("cargo"))
}

/// The workspace root cargo reports paths against, run in `package_dir`,
/// spelled the way `package_dir` is (cargo gives the canonical path, e.g.
/// `/private/var/…` for `/var/…`), so diagnostics name files as the editor
/// opened them.
fn workspace_root(cargo: &Path, package_dir: &Path) -> Option<PathBuf> {
    let output = Command::new(cargo)
        .args(["locate-project", "--workspace", "--message-format", "plain"])
        .current_dir(package_dir)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let manifest = String::from_utf8(output.stdout).ok()?;
    let root = Path::new(manifest.trim()).parent()?.to_path_buf();
    let canonical_dir = package_dir.canonicalize().ok()?;
    let depth = canonical_dir.strip_prefix(&root).ok()?.components().count();
    package_dir.ancestors().nth(depth).map(Path::to_path_buf)
}

/// Runs `cargo check --all-targets` for the package holding `file`. Blocks.
pub fn run(file: &Path, cancel: &Cancel) -> CheckOutcome {
    let started = Instant::now();
    let mut outcome = CheckOutcome {
        diagnostics: Vec::new(),
        success: false,
        cancelled: false,
        failure: None,
        elapsed: Duration::ZERO,
        package_dir: None,
    };
    let Some(dir) = package_dir(file) else {
        outcome.failure = Some(format!("no Cargo.toml above {}", file.display()));
        return outcome;
    };
    outcome.package_dir = Some(dir.clone());
    let cargo = cargo_program();
    let root = workspace_root(&cargo, &dir).unwrap_or_else(|| dir.clone());
    let spawned = Command::new(&cargo)
        .args(["check", "--all-targets", "--message-format=json"])
        .current_dir(&dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(error) => {
            outcome.failure = Some(format!("cannot run cargo: {error}"));
            return outcome;
        }
    };
    let stdout = child.stdout.take().expect("piped");
    let mut stderr = child.stderr.take().expect("piped");
    *cancel.child.lock().unwrap_or_else(PoisonError::into_inner) = Some(child);
    if cancel.is_cancelled() {
        cancel.cancel();
    }
    // stderr carries cargo's progress and its own errors; keep its end for a
    // failure report without blocking cargo on a full pipe.
    let stderr_reader = std::thread::Builder::new()
        .name("cargo-check-stderr".to_string())
        .spawn(move || {
            let mut text = String::new();
            let _ = stderr.read_to_string(&mut text);
            let start = text.len().saturating_sub(2048);
            let start = (start..text.len())
                .find(|&i| text.is_char_boundary(i))
                .unwrap_or(text.len());
            text[start..].to_string()
        })
        .ok();

    let mut finished = None;
    let mut seen = HashSet::new();
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else {
            break;
        };
        match parse_line(&line, &root) {
            Some(Message::Diagnostic(diagnostic)) => {
                if seen.insert(diagnostic.clone()) {
                    outcome.diagnostics.push(diagnostic);
                }
            }
            Some(Message::Finished(success)) => finished = Some(success),
            None => {}
        }
    }
    let child = cancel
        .child
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take();
    if let Some(mut child) = child {
        let _ = child.wait();
    }
    let stderr_tail = stderr_reader
        .and_then(|reader| reader.join().ok())
        .unwrap_or_default();
    outcome.cancelled = cancel.is_cancelled();
    match finished {
        Some(success) => outcome.success = success,
        None if !outcome.cancelled => {
            outcome.failure = Some(if stderr_tail.trim().is_empty() {
                "cargo ended without finishing the check".to_string()
            } else {
                stderr_tail.trim().to_string()
            });
        }
        None => {}
    }
    outcome.diagnostics.sort_by(|a, b| {
        (a.level, &a.file, a.line, a.column).cmp(&(b.level, &b.file, b.line, b.column))
    });
    outcome.elapsed = started.elapsed();
    outcome
}

enum Message {
    Diagnostic(Diagnostic),
    Finished(bool),
}

/// One line of cargo's JSON output. Paths are made absolute against `root`.
fn parse_line(line: &str, root: &Path) -> Option<Message> {
    let value: Value = serde_json::from_str(line).ok()?;
    match value.get("reason")?.as_str()? {
        "build-finished" => Some(Message::Finished(value.get("success")?.as_bool()?)),
        "compiler-message" => {
            parse_diagnostic(value.get("message")?, root).map(Message::Diagnostic)
        }
        _ => None,
    }
}

fn parse_diagnostic(message: &Value, root: &Path) -> Option<Diagnostic> {
    let level = match message.get("level")?.as_str()? {
        "error" => Level::Error,
        "warning" => Level::Warning,
        _ => return None,
    };
    let span = message
        .get("spans")?
        .as_array()?
        .iter()
        .find(|span| span.get("is_primary").and_then(Value::as_bool) == Some(true))?;
    let number = |key: &str| span.get(key).and_then(Value::as_u64).map(|n| n as usize);
    let file = PathBuf::from(span.get("file_name")?.as_str()?);
    Some(Diagnostic {
        level,
        file: if file.is_absolute() {
            file
        } else {
            root.join(file)
        },
        line: number("line_start")?,
        column: number("column_start")?,
        end_line: number("line_end")?,
        end_column: number("column_end")?,
        message: message.get("message")?.as_str()?.to_string(),
        label: span
            .get("label")
            .and_then(Value::as_str)
            .map(str::to_string),
        code: message
            .get("code")
            .and_then(|code| code.get("code"))
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ERROR_LINE: &str = r#"{"reason":"compiler-message","package_id":"cc 0.1.0","manifest_path":"/w/cc/Cargo.toml","target":{"name":"cc"},"message":{"rendered":"error[E0308]: mismatched types","$message_type":"diagnostic","children":[],"code":{"code":"E0308","explanation":"..."},"level":"error","message":"mismatched types","spans":[{"byte_end":40,"byte_start":36,"column_end":22,"column_start":18,"expansion":null,"file_name":"src/main.rs","is_primary":true,"label":"expected `u32`, found `&str`","line_end":2,"line_start":2,"suggested_replacement":null,"suggestion_applicability":null,"text":[]},{"byte_end":33,"byte_start":30,"column_end":15,"column_start":12,"expansion":null,"file_name":"src/main.rs","is_primary":false,"label":"expected due to this","line_end":2,"line_start":2,"suggested_replacement":null,"suggestion_applicability":null,"text":[]}]}}"#;

    #[test]
    fn a_compiler_error_parses_at_its_primary_span() {
        let Some(Message::Diagnostic(diagnostic)) = parse_line(ERROR_LINE, Path::new("/w/cc"))
        else {
            panic!("not parsed");
        };
        assert_eq!(
            diagnostic,
            Diagnostic {
                level: Level::Error,
                file: PathBuf::from("/w/cc/src/main.rs"),
                line: 2,
                column: 18,
                end_line: 2,
                end_column: 22,
                message: "mismatched types".to_string(),
                label: Some("expected `u32`, found `&str`".to_string()),
                code: Some("E0308".to_string()),
            }
        );
    }

    #[test]
    fn notes_without_a_location_and_other_reasons_are_skipped() {
        let note = r#"{"reason":"compiler-message","message":{"level":"failure-note","message":"For more information","spans":[],"code":null}}"#;
        assert!(parse_line(note, Path::new("/w")).is_none());
        assert!(parse_line(r#"{"reason":"compiler-artifact"}"#, Path::new("/w")).is_none());
        assert!(matches!(
            parse_line(
                r#"{"reason":"build-finished","success":false}"#,
                Path::new("/w")
            ),
            Some(Message::Finished(false))
        ));
        assert!(parse_line("not json", Path::new("/w")).is_none());
    }

    #[test]
    fn the_package_is_the_nearest_cargo_toml_above_the_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("crate/src/bin")).unwrap();
        std::fs::write(dir.path().join("crate/Cargo.toml"), "").unwrap();
        assert_eq!(
            package_dir(&dir.path().join("crate/src/bin/tool.rs")),
            Some(dir.path().join("crate"))
        );
        assert_eq!(package_dir(&dir.path().join("loose.rs")), None);
    }

    /// Runs the real cargo on a small broken crate: two copies of the same
    /// error (bin and test targets) become one, with an absolute path.
    #[test]
    fn a_real_check_reports_the_error_once() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"broken\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[workspace]\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("src/main.rs"),
            "fn main() {\n    let x: u32 = \"no\";\n}\n",
        )
        .unwrap();
        let outcome = run(&dir.path().join("src/main.rs"), &Cancel::default());
        assert!(!outcome.success);
        assert_eq!(outcome.failure, None);
        let errors: Vec<_> = outcome
            .diagnostics
            .iter()
            .filter(|d| d.level == Level::Error)
            .collect();
        assert_eq!(errors.len(), 1, "{:?}", outcome.diagnostics);
        assert_eq!(errors[0].file, dir.path().join("src/main.rs"));
        assert_eq!((errors[0].line, errors[0].column), (2, 18));
    }

    #[test]
    fn a_cancelled_check_ends_quietly() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"quiet\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[workspace]\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        let cancel = Cancel::default();
        cancel.cancel();
        let outcome = run(&dir.path().join("src/main.rs"), &cancel);
        assert!(outcome.cancelled);
        assert_eq!(outcome.failure, None);
    }
}
