//! Tools for checking work: `cargo` (build, test, lint, format check) and `git`
//! (status, diff, log, show), run in a directory inside the workspace.
//!
//! No shell: the program and its arguments are fixed here, and arguments that would
//! reach outside the workspace or swap in other programs (`--manifest-path`,
//! `--config`, `--target-dir`, git's `-c`, `--output`, external diff and text
//! conversion) are refused. `cargo fmt` only checks (`--check`); git only reads. A build
//! or test runs the workspace's own code, which the model may have edited: the same
//! trust as the editing tools.
//!
//! Each child runs under a loadngo proactor: threads drain its output and post it to the
//! proactor as completed work, a proactor deadline kills a child that runs too long, and
//! the caller waits in the proactor. Nothing polls. The result is the exit status and
//! the end of each stream.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use loadngo_proactor::{new_platform_proactor, CompletionKind};
use serde_json::{json, Value};

use crate::tools::Tool;

/// The end of each stream kept, in bytes.
const TAIL_BYTES: usize = 8 * 1024;
const CARGO_TIMEOUT: Duration = Duration::from_secs(20 * 60);
const GIT_TIMEOUT: Duration = Duration::from_secs(60);

/// What a finished command left.
pub struct Ran {
    pub status: Option<i32>,
    pub timed_out: bool,
    pub elapsed: Duration,
    pub stdout: String,
    pub stderr: String,
}

/// The last `TAIL_BYTES` of `bytes` as text, starting at a line.
fn tail(bytes: &[u8]) -> String {
    let start = bytes.len().saturating_sub(TAIL_BYTES);
    let text = String::from_utf8_lossy(&bytes[start..]);
    match (start > 0, text.find('\n')) {
        (true, Some(cut)) => format!("[…]\n{}", &text[cut + 1..]),
        _ => text.into_owned(),
    }
}

/// Runs `command` to completion or `timeout`, waiting in a loadngo proactor.
///
/// # Errors
/// When the program cannot be started or the proactor fails.
pub fn run(mut command: Command, timeout: Duration) -> Result<Ran, String> {
    let proactor = new_platform_proactor().map_err(|e| format!("proactor: {e}"))?;
    let handle = proactor.handle();
    let started = Instant::now();
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot start {:?}: {e}", command.get_program()))?;
    let streams: Arc<Mutex<[Option<Vec<u8>>; 2]>> = Arc::default();
    let pipes: [Box<dyn Read + Send>; 2] = [
        Box::new(child.stdout.take().expect("piped")),
        Box::new(child.stderr.take().expect("piped")),
    ];
    for (i, mut pipe) in pipes.into_iter().enumerate() {
        let handle = handle.clone();
        let streams = Arc::clone(&streams);
        std::thread::spawn(move || {
            // Keep the end of the stream, a bounded amount, as it arrives.
            let mut kept = Vec::new();
            let mut chunk = [0_u8; 16 * 1024];
            while let Ok(n) = pipe.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                kept.extend_from_slice(&chunk[..n]);
                if kept.len() > 4 * TAIL_BYTES {
                    kept.drain(..kept.len() - 2 * TAIL_BYTES);
                }
            }
            let _ = handle.enqueue_work(move |_| {
                if let Ok(mut s) = streams.lock() {
                    s[i] = Some(kept);
                }
            });
        });
    }
    let expired = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&expired);
    handle
        .defer_for(timeout, CompletionKind::Timer, 0, move |_| {
            flag.store(true, Ordering::Release)
        })
        .map_err(|e| format!("proactor: {e}"))?;
    let mut killed = false;
    loop {
        if streams
            .lock()
            .map(|s| s.iter().all(Option::is_some))
            .unwrap_or(true)
        {
            break;
        }
        proactor.run_once().map_err(|e| format!("proactor: {e}"))?;
        if expired.load(Ordering::Acquire) && !killed {
            // Closing the child closes its pipes, which ends the readers.
            let _ = child.kill();
            killed = true;
        }
    }
    let status = child
        .wait()
        .map_err(|e| format!("waiting for the command: {e}"))?;
    let [out, err] = std::mem::take(&mut *streams.lock().map_err(|_| "stream lock poisoned")?);
    Ok(Ran {
        status: status.code(),
        timed_out: killed,
        elapsed: started.elapsed(),
        stdout: tail(&out.unwrap_or_default()),
        stderr: tail(&err.unwrap_or_default()),
    })
}

/// How a finished command reads to the model.
fn report(what: &str, ran: &Ran) -> String {
    let status = match (ran.timed_out, ran.status) {
        (true, _) => "stopped: it ran past its time limit".to_owned(),
        (false, Some(0)) => "succeeded (exit 0)".to_owned(),
        (false, Some(code)) => format!("FAILED (exit {code})"),
        (false, None) => "ended by a signal".to_owned(),
    };
    let mut out = format!("{what}: {status} after {:.1}s", ran.elapsed.as_secs_f64());
    for (name, text) in [("stderr", &ran.stderr), ("stdout", &ran.stdout)] {
        if !text.trim().is_empty() {
            out.push_str(&format!("\n--- {name}:\n{}", text.trim_end()));
        }
    }
    out
}

/// Tools that run in directories of one workspace.
pub struct WorkTools {
    workspace: PathBuf,
}

impl WorkTools {
    /// # Errors
    /// When the workspace does not exist.
    pub fn new(workspace: &Path) -> Result<Self, String> {
        Ok(Self {
            workspace: workspace
                .canonicalize()
                .map_err(|e| format!("workspace {}: {e}", workspace.display()))?,
        })
    }

    /// `cargo` and `git`.
    pub fn into_tools(self) -> Vec<Box<dyn Tool>> {
        let shared = std::rc::Rc::new(self);
        vec![
            Box::new(Cargo(std::rc::Rc::clone(&shared))),
            Box::new(Git(shared)),
        ]
    }

    /// A directory inside the workspace (`.` for the workspace), and how it is shown.
    fn dir(&self, given: &str) -> Result<(PathBuf, String), String> {
        let joined = self.workspace.join(given.trim().trim_start_matches("./"));
        let real = joined.canonicalize().map_err(|e| format!("{given}: {e}"))?;
        if !real.starts_with(&self.workspace) {
            return Err(format!("{given} is outside the workspace"));
        }
        if !real.is_dir() {
            return Err(format!("{given} is not a directory"));
        }
        let shown = real
            .strip_prefix(&self.workspace)
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        Ok((real, if shown.is_empty() { ".".into() } else { shown }))
    }
}

/// The `args` array, refusing any that starts with one of `refused`.
fn arguments(args: &Value, refused: &[&str]) -> Result<Vec<String>, String> {
    let list = match args.get("args") {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(list)) => list,
        Some(_) => return Err("`args` must be an array of strings".into()),
    };
    let mut out = Vec::new();
    for item in list {
        let arg = item.as_str().ok_or("`args` must be an array of strings")?;
        if let Some(bad) = refused.iter().find(|r| arg.starts_with(*r)) {
            return Err(format!("`{arg}` is not allowed here ({bad} reaches outside the workspace or runs other programs)"));
        }
        out.push(arg.to_owned());
    }
    Ok(out)
}

fn command_name<'a>(args: &'a Value, allowed: &[&str]) -> Result<&'a str, String> {
    let name = args
        .get("command")
        .and_then(Value::as_str)
        .ok_or("missing string argument `command`")?;
    allowed
        .contains(&name)
        .then_some(name)
        .ok_or_else(|| format!("command must be one of {allowed:?}"))
}

struct Cargo(std::rc::Rc<WorkTools>);
struct Git(std::rc::Rc<WorkTools>);

const CARGO_COMMANDS: [&str; 5] = ["check", "test", "clippy", "build", "fmt"];
const GIT_COMMANDS: [&str; 4] = ["status", "diff", "log", "show"];

impl Tool for Cargo {
    fn name(&self) -> &'static str {
        "cargo"
    }
    fn description(&self) -> &'static str {
        "Run cargo check, test, clippy, build or fmt (check only) in a Rust crate or workspace \
         directory, to verify your changes. Shows the exit status and the end of the output."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "dir": {"type": "string", "description": "directory with a Cargo.toml, relative to the workspace, e.g. loadngo or loadngo/line-editor"},
            "command": {"type": "string", "enum": CARGO_COMMANDS},
            "args": {"type": "array", "items": {"type": "string"}, "description": "further arguments, e.g. [\"-p\", \"loadngo-gpt-oss\"] or a test name"}},
            "required": ["dir", "command"]})
    }
    fn call(&self, args: &Value) -> Result<String, String> {
        let (dir, shown) = self.0.dir(
            args.get("dir")
                .and_then(Value::as_str)
                .ok_or("missing string argument `dir`")?,
        )?;
        let name = command_name(args, &CARGO_COMMANDS)?;
        if !dir.join("Cargo.toml").is_file() {
            return Err(format!("{shown} has no Cargo.toml"));
        }
        let extra = arguments(
            args,
            &[
                "--manifest-path",
                "--config",
                "--target-dir",
                "-Z",
                "--out-dir",
                "--artifact-dir",
            ],
        )?;
        let mut command = Command::new("cargo");
        command
            .current_dir(&dir)
            .env("CARGO_TERM_COLOR", "never")
            .arg(name);
        if name == "fmt" {
            command.arg("--check");
        }
        command.args(&extra);
        let ran = run(command, CARGO_TIMEOUT)?;
        let fmt_check = if name == "fmt" { " --check" } else { "" };
        Ok(report(
            &format!("cargo {name}{fmt_check} {} in {shown}", extra.join(" ")),
            &ran,
        ))
    }
}

impl Tool for Git {
    fn name(&self) -> &'static str {
        "git"
    }
    fn description(&self) -> &'static str {
        "Read a repository's state: git status, diff, log or show (read-only), e.g. to review \
         your own changes before reporting them."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "dir": {"type": "string", "description": "a directory in the repository, relative to the workspace"},
            "command": {"type": "string", "enum": GIT_COMMANDS},
            "args": {"type": "array", "items": {"type": "string"}, "description": "further arguments, e.g. [\"--stat\"] or [\"-n\", \"5\"]"}},
            "required": ["dir", "command"]})
    }
    fn call(&self, args: &Value) -> Result<String, String> {
        let (dir, shown) = self.0.dir(
            args.get("dir")
                .and_then(Value::as_str)
                .ok_or("missing string argument `dir`")?,
        )?;
        let name = command_name(args, &GIT_COMMANDS)?;
        let extra = arguments(
            args,
            &[
                "--output",
                "--ext-diff",
                "--textconv",
                "-c",
                "--exec",
                "--upload-pack",
                "--config",
            ],
        )?;
        let mut command = Command::new("git");
        command
            .current_dir(&dir)
            .env("GIT_PAGER", "cat")
            .env_remove("GIT_EXTERNAL_DIFF")
            .args(["--no-pager", "-c", "color.ui=never", name]);
        if name != "status" {
            command.args(["--no-ext-diff", "--no-textconv"]);
        }
        command.args(&extra);
        let ran = run(command, GIT_TIMEOUT)?;
        Ok(report(
            &format!("git {name} {} in {shown}", extra.join(" ")),
            &ran,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::Toolbox;

    fn toolbox(dir: &Path) -> Toolbox {
        let mut tools = Toolbox::default();
        for tool in WorkTools::new(dir).unwrap().into_tools() {
            tools.push(tool);
        }
        tools
    }

    fn command(program: &str, args: &[&str]) -> Command {
        let mut command = Command::new(program);
        command.args(args);
        command
    }

    #[test]
    fn commands_report_status_and_output_and_a_time_limit_kills() {
        let ran = run(
            command("sh", &["-c", "echo out; echo err >&2; exit 3"]),
            Duration::from_secs(30),
        )
        .unwrap();
        assert_eq!((ran.status, ran.timed_out), (Some(3), false));
        assert_eq!(
            (ran.stdout.as_str(), ran.stderr.as_str()),
            ("out\n", "err\n")
        );
        let started = Instant::now();
        let slow = run(command("sleep", &["30"]), Duration::from_millis(300)).unwrap();
        assert!(slow.timed_out && started.elapsed() < Duration::from_secs(10));
        let long = run(
            command("sh", &["-c", "seq 1 20000"]),
            Duration::from_secs(30),
        )
        .unwrap();
        assert!(
            long.stdout.starts_with("[…]\n")
                && long.stdout.ends_with("20000\n")
                && long.stdout.len() < TAIL_BYTES + 16
        );
    }

    #[test]
    fn git_reads_and_refuses_writing_or_outside_arguments() {
        let dir = std::env::temp_dir().join(format!("loadngo-work-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("repo")).unwrap();
        assert!(Command::new("git")
            .args(["init", "-q"])
            .current_dir(dir.join("repo"))
            .status()
            .unwrap()
            .success());
        std::fs::write(dir.join("repo/a.txt"), "x\n").unwrap();
        let tools = toolbox(&dir);
        let status = tools
            .call(
                "git",
                r#"{"dir":"repo","command":"status","args":["--short"]}"#,
            )
            .unwrap();
        assert!(
            status.contains("succeeded") && status.contains("?? a.txt"),
            "{status}"
        );
        for (call, why) in [
            (r#"{"dir":"repo","command":"commit"}"#, "must be one of"),
            (
                r#"{"dir":"repo","command":"diff","args":["--output=/tmp/x"]}"#,
                "not allowed",
            ),
            (
                r#"{"dir":"repo","command":"log","args":["-c","core.pager=x"]}"#,
                "not allowed",
            ),
            (
                r#"{"dir":"..","command":"status"}"#,
                "outside the workspace",
            ),
            (r#"{"dir":"repo","command":"fmt"}"#, "must be one of"),
        ] {
            let error = tools.call("git", call).unwrap_err();
            assert!(error.contains(why), "{call}: {error}");
        }
        let error = tools
            .call("cargo", r#"{"dir":"repo","command":"check"}"#)
            .unwrap_err();
        assert!(error.contains("no Cargo.toml"), "{error}");
        let error = tools
            .call("cargo", r#"{"dir":".","command":"run"}"#)
            .unwrap_err();
        assert!(error.contains("must be one of"), "{error}");
    }
}
