//! `cas_list`, `cas_find`, `cas_read`, `cas_grep`: the [`crate::tools`] interface over a
//! signed, verified Archive CAS snapshot ([`data::archive_view::ArchiveView`]).
//!
//! Every result begins with the snapshot's identity (archive, signed root, signer) and
//! names the object hash of each file it quotes, so a model's claims about a file can be
//! checked against the same bytes by anyone holding the public key.

use std::fmt::Write as _;
use std::rc::Rc;

use data::archive_view::ArchiveView;
use serde_json::{json, Value};

use crate::tools::{
    as_text, glob_match, numbered_lines, Tool, MAX_ENTRIES, MAX_GREP_FILE_BYTES, MAX_MATCHES,
    MAX_SCAN_BYTES,
};

fn header(view: &ArchiveView) -> String {
    format!(
        "snapshot {} root {} signed by {}\n",
        view.archive_id(),
        view.root().to_hex(),
        view.signer()
    )
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

/// The four tools over one opened view.
pub fn cas_tools(view: ArchiveView) -> Vec<Box<dyn Tool>> {
    let view = Rc::new(view);
    vec![
        Box::new(CasList(view.clone())),
        Box::new(CasFind(view.clone())),
        Box::new(CasRead(view.clone())),
        Box::new(CasGrep(view)),
    ]
}

struct CasList(Rc<ArchiveView>);
struct CasFind(Rc<ArchiveView>);
struct CasRead(Rc<ArchiveView>);
struct CasGrep(Rc<ArchiveView>);

impl Tool for CasList {
    fn name(&self) -> &'static str {
        "cas_list"
    }
    fn description(&self) -> &'static str {
        "List a directory in the signed loadngo CAS snapshot of the pudding workspace (verified, read-only): kinds and sizes; cas_read gives each file's object hash."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {"path": {"type": "string",
            "description": "directory inside the snapshot; \"\" is its root"}}, "required": ["path"]})
    }
    fn call(&self, args: &Value) -> Result<String, String> {
        let path = str_arg(args, "path")?;
        let children = self.0.list(path).map_err(|e| format!("{e:#}"))?;
        let mut out = header(&self.0);
        // Kinds and sizes only: the signed root above identifies every entry, and
        // per-entry hashes would triple the prompt tokens a listing costs the model.
        for child in children.iter().take(MAX_ENTRIES) {
            match child.object {
                Some(o) => {
                    let _ = writeln!(out, "{} {:>10}  {}", child.kind, o.size, child.name);
                }
                None => {
                    let _ = writeln!(out, "{}  {}", child.kind, child.name);
                }
            }
        }
        if children.len() > MAX_ENTRIES {
            let _ = writeln!(
                out,
                "[{} more entries not shown]",
                children.len() - MAX_ENTRIES
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
        "Find files in the signed CAS snapshot whose path matches a glob (* within a directory, ** across)."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {"pattern": {"type": "string",
            "description": "e.g. loadngo/**/*.rs"}}, "required": ["pattern"]})
    }
    fn call(&self, args: &Value) -> Result<String, String> {
        let pattern = str_arg(args, "pattern")?;
        let mut out = header(&self.0);
        let mut count = 0;
        for (path, object) in self.0.files() {
            if glob_match(pattern, path) {
                let _ = writeln!(
                    out,
                    "{path}  {} bytes  object {}",
                    object.size,
                    object.hash.to_hex()
                );
                count += 1;
                if count >= MAX_MATCHES {
                    out.push_str("[stopped at the match limit; narrow the pattern]\n");
                    break;
                }
            }
        }
        let _ = writeln!(out, "{count} matches");
        Ok(out)
    }
}

impl Tool for CasRead {
    fn name(&self) -> &'static str {
        "cas_read"
    }
    fn description(&self) -> &'static str {
        "Read a text file from the signed CAS snapshot, BLAKE3-verified, with line numbers. At most 16 KiB per call."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "path": {"type": "string"},
            "line_start": {"type": "integer", "description": "first line, 1-based (default 1)"},
            "line_count": {"type": "integer", "description": "number of lines (default 400)"}},
            "required": ["path"]})
    }
    fn call(&self, args: &Value) -> Result<String, String> {
        let path = str_arg(args, "path")?;
        let (bytes, object) = self.0.read_file(path).map_err(|e| format!("{e:#}"))?;
        let text = as_text(&bytes).ok_or_else(|| {
            format!(
                "{path} is binary ({} bytes, object {})",
                object.size,
                object.hash.to_hex()
            )
        })?;
        Ok(format!(
            "{}{path}  {} bytes  object {} (verified)\n{}",
            header(&self.0),
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
        "Search text files in the signed CAS snapshot for a literal string; returns path:line: text. Bounded to 32 MiB scanned and 100 matches."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "pattern": {"type": "string"},
            "glob": {"type": "string", "description": "only paths matching, e.g. loadngo/**/*.rs"}},
            "required": ["pattern"]})
    }
    fn call(&self, args: &Value) -> Result<String, String> {
        let pattern = str_arg(args, "pattern")?;
        if pattern.is_empty() {
            return Err("empty pattern".into());
        }
        let filter = args.get("glob").and_then(Value::as_str);
        let mut out = header(&self.0);
        let (mut matches, mut scanned) = (0, 0_u64);
        'files: for (path, object) in self.0.files() {
            if object.size > MAX_GREP_FILE_BYTES || filter.is_some_and(|g| !glob_match(g, path)) {
                continue;
            }
            let Ok(bytes) = self.0.read_object(object) else {
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
    use std::path::Path;

    /// `cargo test --release -p loadngo-inference --features cas -- --ignored --nocapture`
    #[test]
    #[ignore = "needs the signed pudding snapshot on Zhoenus II and its public key"]
    fn real_signed_pudding_snapshot_opens_and_reads_verified() {
        let root = Path::new("/Volumes/Zhoenus II/pudding-cas");
        let key = root.join("keys/jay-macmini.dilithium2.pub");
        let start = std::time::Instant::now();
        let view = ArchiveView::open_newest_verified(root, &key).unwrap();
        eprintln!(
            "opened {} ({} files) root {} in {:?}",
            view.archive_id(),
            view.file_count(),
            view.root().to_hex(),
            start.elapsed()
        );
        let mut tools = Toolbox::default();
        for tool in cas_tools(view) {
            tools.push(tool);
        }
        let found = tools
            .call("cas_find", r#"{"pattern": "loadngo/proactor/src/*.rs"}"#)
            .unwrap();
        assert!(found.contains("loadngo/proactor/src/lib.rs"), "{found}");
        let read = tools
            .call(
                "cas_read",
                r#"{"path": "loadngo/proactor/src/lib.rs", "line_count": 5}"#,
            )
            .unwrap();
        assert!(
            read.contains("(verified)") && read.contains("    1  "),
            "{read}"
        );
        eprintln!("{}", read.lines().take(4).collect::<Vec<_>>().join("\n"));
    }
}
