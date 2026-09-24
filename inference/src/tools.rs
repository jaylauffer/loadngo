//! Read-only tools a local model may call, independent of any model's tool-call
//! encoding. A [`Toolbox`] declares its tools as JSON function schemas (the widely used
//! `{"type":"function","function":{...}}` shape) and runs a call by name with JSON
//! arguments, returning text for the model to read.
//!
//! Everything here is bounded: entries listed, bytes read, bytes scanned, matches
//! returned. Nothing writes, deletes, executes or touches the network. A failed call
//! returns an error message as its result text; it never panics the conversation.

use std::fmt::Write as _;
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde_json::{json, Value};

/// One callable tool.
pub trait Tool {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    /// JSON Schema of the arguments object.
    fn parameters(&self) -> Value;
    /// Runs the call. `Err` text is shown to the model as the result.
    fn call(&self, args: &Value) -> Result<String, String>;
}

/// Tools offered to one conversation.
#[derive(Default)]
pub struct Toolbox {
    tools: Vec<Box<dyn Tool>>,
}

impl Toolbox {
    pub fn push(&mut self, tool: Box<dyn Tool>) {
        self.tools.push(tool);
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    pub fn names(&self) -> Vec<&'static str> {
        self.tools.iter().map(|t| t.name()).collect()
    }

    /// Compact JSON array declaring every tool.
    pub fn declaration(&self) -> String {
        let list: Vec<Value> = self
            .tools
            .iter()
            .map(|t| {
                json!({"type": "function", "function": {
                    "name": t.name(), "description": t.description(), "parameters": t.parameters()}})
            })
            .collect();
        Value::Array(list).to_string()
    }

    /// Runs `name` with `arguments` (JSON text). Unknown tools, malformed arguments and
    /// tool failures all come back as `Err` text for the model.
    pub fn call(&self, name: &str, arguments: &str) -> Result<String, String> {
        let tool = self
            .tools
            .iter()
            .find(|t| t.name() == name)
            .ok_or_else(|| format!("no tool named `{name}`; available: {:?}", self.names()))?;
        let args: Value = if arguments.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(arguments).map_err(|e| format!("arguments are not JSON: {e}"))?
        };
        if !args.is_object() {
            return Err("arguments must be a JSON object".into());
        }
        tool.call(&args)
    }
}

/// Largest text returned by one read.
pub const MAX_READ_BYTES: usize = 16 * 1024;
/// Most entries returned by one listing, and matches by one find or grep.
pub const MAX_ENTRIES: usize = 200;
pub const MAX_MATCHES: usize = 100;
/// Most bytes one grep scans, and the largest single file it opens.
pub const MAX_SCAN_BYTES: u64 = 32 * 1024 * 1024;
pub const MAX_GREP_FILE_BYTES: u64 = 2 * 1024 * 1024;
/// Most directory entries one find or grep walk visits.
pub const MAX_WALK: usize = 50_000;

/// Directory names a walk skips: build output and version-control internals.
const SKIP_DIRS: &[&str] = &[".git", "target", "node_modules", ".cache", "__pycache__"];

/// `*` matches within one path component, `**` across components, `?` one character.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    fn go(p: &[u8], t: &[u8]) -> bool {
        match p.first() {
            None => t.is_empty(),
            Some(b'*') if p.get(1) == Some(&b'*') => {
                let rest = if p.get(2) == Some(&b'/') {
                    &p[3..]
                } else {
                    &p[2..]
                };
                (0..=t.len()).any(|i| go(rest, &t[i..]))
            }
            Some(b'*') => (0..=t.len())
                .take_while(|&i| i == 0 || t[i - 1] != b'/')
                .any(|i| go(&p[1..], &t[i..])),
            Some(b'?') => !t.is_empty() && t[0] != b'/' && go(&p[1..], &t[1..]),
            Some(&c) => t.first() == Some(&c) && go(&p[1..], &t[1..]),
        }
    }
    go(pattern.as_bytes(), text.as_bytes())
}

/// Text for the model, or `None` for binary content (NUL bytes or invalid UTF-8).
pub fn as_text(bytes: &[u8]) -> Option<&str> {
    if bytes.contains(&0) {
        return None;
    }
    std::str::from_utf8(bytes).ok()
}

/// Numbered lines `start..start+count` (1-based) of `text`, cut at [`MAX_READ_BYTES`].
pub fn numbered_lines(text: &str, start: usize, count: usize) -> String {
    let mut out = String::new();
    let total = text.lines().count();
    for (i, line) in text
        .lines()
        .enumerate()
        .skip(start.saturating_sub(1))
        .take(count)
    {
        if out.len() + line.len() + 8 > MAX_READ_BYTES {
            let _ = writeln!(
                out,
                "[truncated at {MAX_READ_BYTES} bytes; ask for later lines]"
            );
            return out;
        }
        let _ = writeln!(out, "{:>5}  {line}", i + 1);
    }
    if start.saturating_sub(1) + count < total {
        let _ = writeln!(
            out,
            "[{total} lines in total; shown {start}..{}]",
            (start + count - 1).min(total)
        );
    }
    out
}

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

/// Read-only access to the local filesystem.
///
/// Relative paths resolve against `base`. Absolute paths anywhere are allowed except the
/// deny list: key and credential stores (`~/.ssh`, `~/.gnupg`, `~/.loadngo/keys`,
/// `~/.aws`, `~/.config/gh`, `~/Library/Keychains`) and files that look like private keys
/// (`*.key`, `*.pem`, `id_*`, `.env`). Symlinks are resolved before the check, so a link
/// cannot reach a denied location.
pub struct FsTools {
    base: PathBuf,
    denied_dirs: Vec<PathBuf>,
}

impl FsTools {
    pub fn new(base: impl Into<PathBuf>, home: Option<&Path>) -> Self {
        let denied_dirs = home
            .map(|h| {
                [
                    ".ssh",
                    ".gnupg",
                    ".loadngo/keys",
                    ".aws",
                    ".config/gh",
                    "Library/Keychains",
                ]
                .iter()
                .map(|d| h.join(d))
                .map(|d| d.canonicalize().unwrap_or(d))
                .collect()
            })
            .unwrap_or_default();
        Self {
            base: base.into(),
            denied_dirs,
        }
    }

    pub fn base(&self) -> &Path {
        &self.base
    }

    /// Resolves and checks `path`; returns the canonical path.
    fn resolve(&self, path: &str) -> Result<PathBuf, String> {
        if path.is_empty() {
            return Err("empty path".into());
        }
        let expanded = if let Some(rest) = path.strip_prefix("~/") {
            std::env::var_os("HOME")
                .map(|h| PathBuf::from(h).join(rest))
                .ok_or("HOME is not set")?
        } else {
            PathBuf::from(path)
        };
        let joined = if expanded.is_absolute() {
            expanded
        } else {
            self.base.join(expanded)
        };
        let real = joined
            .canonicalize()
            .map_err(|e| format!("{}: {e}", joined.display()))?;
        self.check(&real)?;
        Ok(real)
    }

    fn check(&self, real: &Path) -> Result<(), String> {
        if self.denied_dirs.iter().any(|d| real.starts_with(d)) {
            return Err(format!(
                "{} is in a key/credential store; not readable",
                real.display()
            ));
        }
        if let Some(name) = real.file_name().and_then(|n| n.to_str()) {
            let lower = name.to_ascii_lowercase();
            if lower.ends_with(".key")
                || lower.ends_with(".pem")
                || lower.starts_with("id_")
                || lower == ".env"
            {
                return Err(format!(
                    "{} looks like a private key or secret; not readable",
                    real.display()
                ));
            }
        }
        Ok(())
    }

    /// Relative display of `path` against the walk root.
    fn walk(
        &self,
        root: &Path,
        mut visit: impl FnMut(&Path, &fs::Metadata) -> bool,
    ) -> Result<usize, String> {
        let mut stack = vec![root.to_path_buf()];
        let mut visited = 0;
        while let Some(dir) = stack.pop() {
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            let mut entries: Vec<_> = entries.flatten().collect();
            entries.sort_by_key(std::fs::DirEntry::file_name);
            for entry in entries.into_iter().rev() {
                visited += 1;
                if visited > MAX_WALK {
                    return Ok(visited);
                }
                let path = entry.path();
                let Ok(meta) = entry.metadata() else { continue };
                if self.check(&path).is_err() {
                    continue;
                }
                if meta.is_dir() {
                    let name = entry.file_name();
                    if !SKIP_DIRS.iter().any(|s| name == *s)
                        && !self.denied_dirs.iter().any(|d| path.starts_with(d))
                    {
                        stack.push(path.clone());
                    }
                }
                if !visit(&path, &meta) {
                    return Ok(visited);
                }
            }
        }
        Ok(visited)
    }

    /// `fs_list`, `fs_read`, `fs_find` and `fs_grep`, sharing one policy.
    pub fn into_tools(self) -> Vec<Box<dyn Tool>> {
        let shared = std::rc::Rc::new(self);
        vec![
            Box::new(FsList(shared.clone())),
            Box::new(FsRead(shared.clone())),
            Box::new(FsFind(shared.clone())),
            Box::new(FsGrep(shared)),
        ]
    }
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

struct FsList(std::rc::Rc<FsTools>);
struct FsRead(std::rc::Rc<FsTools>);
struct FsFind(std::rc::Rc<FsTools>);
struct FsGrep(std::rc::Rc<FsTools>);

impl Tool for FsList {
    fn name(&self) -> &'static str {
        "fs_list"
    }
    fn description(&self) -> &'static str {
        "List a directory on the local drive (read-only): names, kinds and sizes."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {"path": {"type": "string",
            "description": "directory; relative paths start at the workspace"}}, "required": ["path"]})
    }
    fn call(&self, args: &Value) -> Result<String, String> {
        let dir = self.0.resolve(str_arg(args, "path")?)?;
        let mut entries: Vec<_> = fs::read_dir(&dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?
            .flatten()
            .collect();
        entries.sort_by_key(std::fs::DirEntry::file_name);
        let mut out = format!("{}\n", dir.display());
        for entry in entries.iter().take(MAX_ENTRIES) {
            let Ok(meta) = entry.metadata() else { continue };
            let kind = if meta.is_dir() {
                "dir "
            } else if meta.file_type().is_symlink() {
                "link"
            } else {
                "file"
            };
            let _ = writeln!(
                out,
                "{kind} {:>12}  {}",
                meta.len(),
                entry.file_name().to_string_lossy()
            );
        }
        if entries.len() > MAX_ENTRIES {
            let _ = writeln!(
                out,
                "[{} more entries not shown]",
                entries.len() - MAX_ENTRIES
            );
        }
        Ok(out)
    }
}

impl Tool for FsRead {
    fn name(&self) -> &'static str {
        "fs_read"
    }
    fn description(&self) -> &'static str {
        "Read a text file on the local drive (read-only), with line numbers. At most 16 KiB per call; use line_start/line_count for more."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "path": {"type": "string"},
            "line_start": {"type": "integer", "description": "first line, 1-based (default 1)"},
            "line_count": {"type": "integer", "description": "number of lines (default 400)"}},
            "required": ["path"]})
    }
    fn call(&self, args: &Value) -> Result<String, String> {
        let path = self.0.resolve(str_arg(args, "path")?)?;
        let meta = fs::metadata(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        if meta.is_dir() {
            return Err(format!("{} is a directory; use fs_list", path.display()));
        }
        // Bounded read: enough for the requested window of a large file.
        let limit = 64 * 1024 * 1024;
        if meta.len() > limit {
            return Err(format!(
                "{} is {} bytes; larger than this tool reads",
                path.display(),
                meta.len()
            ));
        }
        let bytes = fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let text = as_text(&bytes)
            .ok_or_else(|| format!("{} is binary ({} bytes)", path.display(), bytes.len()))?;
        Ok(format!(
            "{} ({} bytes)\n{}",
            path.display(),
            bytes.len(),
            numbered_lines(
                text,
                usize_arg(args, "line_start", 1).max(1),
                usize_arg(args, "line_count", 400).max(1)
            )
        ))
    }
}

impl Tool for FsFind {
    fn name(&self) -> &'static str {
        "fs_find"
    }
    fn description(&self) -> &'static str {
        "Find files on the local drive whose path (relative to `root`) matches a glob: * within a directory, ** across directories, ? one character. Skips .git and target."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "pattern": {"type": "string", "description": "e.g. **/*.rs or docs/*.md"},
            "root": {"type": "string", "description": "directory to search (default: the workspace)"}},
            "required": ["pattern"]})
    }
    fn call(&self, args: &Value) -> Result<String, String> {
        let pattern = str_arg(args, "pattern")?;
        let root = self
            .0
            .resolve(args.get("root").and_then(Value::as_str).unwrap_or("."))?;
        let mut found = Vec::new();
        let visited = self.0.walk(&root, |path, _| {
            let rel = relative(&root, path);
            if glob_match(pattern, &rel) {
                found.push(rel);
            }
            found.len() < MAX_MATCHES
        })?;
        found.sort();
        let mut out = format!(
            "{} matches under {} ({visited} entries visited)\n",
            found.len(),
            root.display()
        );
        for path in &found {
            let _ = writeln!(out, "{path}");
        }
        if found.len() >= MAX_MATCHES || visited > MAX_WALK {
            out.push_str("[stopped at the search limit; narrow the pattern or root]\n");
        }
        Ok(out)
    }
}

impl Tool for FsGrep {
    fn name(&self) -> &'static str {
        "fs_grep"
    }
    fn description(&self) -> &'static str {
        "Search text files on the local drive for a literal string; returns path:line: text. Bounded to 32 MiB scanned and 100 matches."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "pattern": {"type": "string", "description": "literal text, case-sensitive"},
            "path": {"type": "string", "description": "file or directory (default: the workspace)"},
            "glob": {"type": "string", "description": "only files whose relative path matches, e.g. **/*.rs"}},
            "required": ["pattern"]})
    }
    fn call(&self, args: &Value) -> Result<String, String> {
        let pattern = str_arg(args, "pattern")?;
        if pattern.is_empty() {
            return Err("empty pattern".into());
        }
        let root = self
            .0
            .resolve(args.get("path").and_then(Value::as_str).unwrap_or("."))?;
        let filter = args.get("glob").and_then(Value::as_str);
        let mut out = String::new();
        let mut matches = 0;
        let mut scanned = 0_u64;
        let mut scan = |path: &Path, rel: &str| -> bool {
            if filter.is_some_and(|g| !glob_match(g, rel)) {
                return true;
            }
            let Ok(bytes) = fs::read(path) else {
                return true;
            };
            scanned += bytes.len() as u64;
            if let Some(text) = as_text(&bytes) {
                for (i, line) in text.lines().enumerate() {
                    if line.contains(pattern) {
                        let shown: String = line.chars().take(240).collect();
                        let _ = writeln!(out, "{rel}:{}: {shown}", i + 1);
                        matches += 1;
                        if matches >= MAX_MATCHES {
                            return false;
                        }
                    }
                }
            }
            scanned < MAX_SCAN_BYTES
        };
        if root.is_file() {
            scan(&root, &root.display().to_string());
        } else {
            self.0.walk(&root, |path, meta| {
                if !meta.is_file() || meta.len() > MAX_GREP_FILE_BYTES {
                    return true;
                }
                scan(path, &relative(&root, path))
            })?;
        }
        Ok(format!(
            "{matches} matches for {pattern:?} under {} ({scanned} bytes scanned)\n{out}{}",
            root.display(),
            if matches >= MAX_MATCHES || scanned >= MAX_SCAN_BYTES {
                "[stopped at the search limit; narrow the path or glob]\n"
            } else {
                ""
            }
        ))
    }
}

/// True when `path` has no `..` component, for callers that join untrusted relative
/// paths onto a root they index themselves (the CAS view).
pub fn is_plain_relative(path: &str) -> bool {
    Path::new(path)
        .components()
        .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("loadngo-tools-{}-{test}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src/nested")).unwrap();
        fs::create_dir_all(dir.join("target")).unwrap();
        fs::create_dir_all(dir.join("home/.ssh")).unwrap();
        fs::write(dir.join("src/lib.rs"), "fn main() {}\n// needle here\n").unwrap();
        fs::write(dir.join("src/nested/deep.rs"), "let needle = 1;\n").unwrap();
        fs::write(dir.join("target/built.rs"), "needle in build output\n").unwrap();
        fs::write(dir.join("home/.ssh/id_ed25519"), "SECRET").unwrap();
        fs::write(dir.join("signing.key"), "SECRET").unwrap();
        fs::write(dir.join("blob.bin"), [0_u8, 1, 2]).unwrap();
        dir
    }

    fn toolbox(dir: &Path) -> Toolbox {
        let mut tools = Toolbox::default();
        for tool in FsTools::new(dir, Some(&dir.join("home"))).into_tools() {
            tools.push(tool);
        }
        tools
    }

    #[test]
    fn glob_distinguishes_one_component_from_many() {
        assert!(glob_match("src/*.rs", "src/lib.rs"));
        assert!(!glob_match("src/*.rs", "src/nested/deep.rs"));
        assert!(glob_match("**/*.rs", "src/nested/deep.rs"));
        assert!(glob_match("**/*.rs", "top.rs"));
        assert!(glob_match("src/?ib.rs", "src/lib.rs"));
        assert!(!glob_match("*.md", "a.rs"));
    }

    #[test]
    fn filesystem_tools_read_find_and_grep_within_bounds() {
        let dir = scratch("read");
        let tools = toolbox(&dir);
        let read = tools.call("fs_read", r#"{"path": "src/lib.rs"}"#).unwrap();
        assert!(read.contains("    2  // needle here"));
        let found = tools.call("fs_find", r#"{"pattern": "**/*.rs"}"#).unwrap();
        assert!(found.contains("src/nested/deep.rs") && !found.contains("target/"));
        let grep = tools.call("fs_grep", r#"{"pattern": "needle"}"#).unwrap();
        assert!(grep.contains("src/lib.rs:2:") && grep.contains("src/nested/deep.rs:1:"));
        assert!(!grep.contains("built.rs"), "build output is skipped");
        assert!(tools
            .call("fs_list", r#"{"path": "src"}"#)
            .unwrap()
            .contains("lib.rs"));
        assert!(tools
            .call("fs_read", r#"{"path": "blob.bin"}"#)
            .unwrap_err()
            .contains("binary"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn secrets_and_bad_calls_are_refused_as_text() {
        let dir = scratch("secrets");
        let tools = toolbox(&dir);
        assert!(tools
            .call("fs_read", r#"{"path": "home/.ssh/id_ed25519"}"#)
            .is_err());
        assert!(tools.call("fs_read", r#"{"path": "signing.key"}"#).is_err());
        assert!(!tools
            .call("fs_grep", r#"{"pattern": "SECRET"}"#)
            .unwrap()
            .contains("SECRET\n"));
        assert!(tools
            .call("fs_delete", "{}")
            .unwrap_err()
            .contains("no tool"));
        assert!(tools.call("fs_read", "not json").is_err());
        assert!(tools.call("fs_read", "[1]").is_err());
        let declared: Value = serde_json::from_str(&tools.declaration()).unwrap();
        assert_eq!(declared.as_array().unwrap().len(), 4);
        assert!(
            is_plain_relative("a/b.rs") && !is_plain_relative("../x") && !is_plain_relative("/etc")
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
