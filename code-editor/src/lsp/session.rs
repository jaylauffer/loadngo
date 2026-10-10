//! One conversation with rust-analyzer, without the process: messages to
//! send come out of [`LspSession::take_outgoing`], messages received go into
//! [`LspSession::handle`], and what the editor should act on comes out of
//! [`LspSession::take_events`]. The app moves bytes between this and the
//! process ([`super::transport`]).
//!
//! Positions are lines and characters counted from zero, as LSP has them.
//! The session asks for UTF-32 positions, which are the editor's own
//! character indices; rust-analyzer agrees. A server that only speaks
//! UTF-16 differs from them only on characters outside the Basic
//! Multilingual Plane (emoji), which code rarely holds.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::uri;
use crate::cargo_check::{Diagnostic, Level};

/// A place in a document: zero-based line and character.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Position {
    pub line: usize,
    pub character: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub path: PathBuf,
    pub range: Range,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextEdit {
    pub range: Range,
    pub new_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionItem {
    pub label: String,
    pub detail: Option<String>,
    /// What to replace and with what; `None` means insert `insert_text` at
    /// the caret, replacing the word typed so far.
    pub edit: Option<TextEdit>,
    pub insert_text: String,
    /// Further edits, such as an added `use` line.
    pub additional_edits: Vec<TextEdit>,
    pub sort_text: String,
}

/// What the editor should act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LspEvent {
    Hover {
        request: u64,
        text: String,
    },
    Definition {
        request: u64,
        locations: Vec<Location>,
    },
    Completion {
        request: u64,
        items: Vec<CompletionItem>,
    },
    /// Edits per file, from a rename.
    Edit {
        request: u64,
        edits: Vec<(PathBuf, Vec<TextEdit>)>,
    },
    /// A request failed: what the server said.
    Failed {
        request: u64,
        message: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pending {
    Initialize,
    Hover,
    Definition,
    Completion,
    Rename,
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    Initializing,
    Running,
    ShuttingDown,
}

#[derive(Debug, Clone, Copy)]
struct OpenDocument {
    version: i64,
    revision: u64,
}

pub struct LspSession {
    root: PathBuf,
    state: State,
    next_id: u64,
    pending: HashMap<u64, Pending>,
    outgoing: Vec<Value>,
    events: Vec<LspEvent>,
    open: HashMap<PathBuf, OpenDocument>,
    /// Documents opened before the server was ready.
    queued: Vec<(PathBuf, String, u64)>,
    diagnostics: HashMap<PathBuf, Vec<Diagnostic>>,
    /// Bumped whenever `diagnostics` changes.
    diagnostics_revision: u64,
    /// rust-analyzer's own words on how it is doing (serverStatus).
    pub status: String,
    /// Whether it has finished loading and indexing.
    pub ready: bool,
    utf32: bool,
}

impl LspSession {
    /// A session for the workspace at `root`; the initialize request is the
    /// first outgoing message.
    pub fn new(root: &Path) -> Self {
        let mut session = Self {
            root: root.to_path_buf(),
            state: State::Initializing,
            next_id: 1,
            pending: HashMap::new(),
            outgoing: Vec::new(),
            events: Vec::new(),
            open: HashMap::new(),
            queued: Vec::new(),
            diagnostics: HashMap::new(),
            diagnostics_revision: 0,
            status: "starting".to_string(),
            ready: false,
            utf32: false,
        };
        let root_uri = uri::from_path(root);
        let name = root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        session.request(
            Pending::Initialize,
            "initialize",
            json!({
                "processId": std::process::id(),
                "clientInfo": {"name": "loadngo-code-editor"},
                "rootUri": root_uri,
                "workspaceFolders": [{"uri": root_uri, "name": name}],
                "capabilities": {
                    "general": {"positionEncodings": ["utf-32", "utf-16"]},
                    "textDocument": {
                        "synchronization": {"didSave": true},
                        "hover": {"contentFormat": ["plaintext", "markdown"]},
                        "definition": {"linkSupport": true},
                        "completion": {
                            "completionItem": {
                                "snippetSupport": false,
                                "documentationFormat": ["plaintext"]
                            }
                        },
                        "rename": {"prepareSupport": false},
                        "publishDiagnostics": {}
                    },
                    "workspace": {
                        "workspaceEdit": {"documentChanges": true},
                        "configuration": false
                    },
                    "window": {"workDoneProgress": false},
                    "experimental": {"serverStatusNotification": true}
                },
                // The editor runs cargo check itself on save (M3); two
                // checks would fight over the build lock.
                "initializationOptions": {"checkOnSave": false}
            }),
        );
        session
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn take_outgoing(&mut self) -> Vec<Value> {
        std::mem::take(&mut self.outgoing)
    }

    pub fn take_events(&mut self) -> Vec<LspEvent> {
        std::mem::take(&mut self.events)
    }

    /// Diagnostics for `path` from the server, against the text last sent.
    pub fn diagnostics(&self, path: &Path) -> &[Diagnostic] {
        self.diagnostics.get(path).map_or(&[], Vec::as_slice)
    }

    pub fn all_diagnostics(&self) -> impl Iterator<Item = &Diagnostic> {
        self.diagnostics.values().flatten()
    }

    pub fn diagnostics_revision(&self) -> u64 {
        self.diagnostics_revision
    }

    pub fn is_open(&self, path: &Path) -> bool {
        self.open.contains_key(path)
    }

    fn request(&mut self, kind: Pending, method: &str, params: Value) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.pending.insert(id, kind);
        self.outgoing
            .push(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        id
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.outgoing
            .push(json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    // ----- documents -----

    /// Opens `path` with `text` at the editor's `revision`.
    pub fn open_document(&mut self, path: &Path, text: &str, revision: u64) {
        if self.open.contains_key(path) {
            return;
        }
        if self.state != State::Running {
            if !self.queued.iter().any(|(queued, _, _)| queued == path) {
                self.queued
                    .push((path.to_path_buf(), text.to_string(), revision));
            }
            return;
        }
        self.open.insert(
            path.to_path_buf(),
            OpenDocument {
                version: 1,
                revision,
            },
        );
        self.notify(
            "textDocument/didOpen",
            json!({"textDocument": {
                "uri": uri::from_path(path),
                "languageId": "rust",
                "version": 1,
                "text": text
            }}),
        );
    }

    /// Whether the server already has `path` at `revision`.
    pub fn in_sync(&self, path: &Path, revision: u64) -> bool {
        self.open
            .get(path)
            .is_some_and(|document| document.revision == revision)
    }

    /// Sends `path`'s whole new text when its revision changed.
    pub fn change_document(&mut self, path: &Path, text: &str, revision: u64) {
        if let Some((_, queued_text, queued_revision)) =
            self.queued.iter_mut().find(|(queued, _, _)| queued == path)
        {
            *queued_text = text.to_string();
            *queued_revision = revision;
            return;
        }
        let Some(document) = self.open.get_mut(path) else {
            return;
        };
        if document.revision == revision {
            return;
        }
        document.revision = revision;
        document.version += 1;
        let version = document.version;
        self.notify(
            "textDocument/didChange",
            json!({
                "textDocument": {"uri": uri::from_path(path), "version": version},
                "contentChanges": [{"text": text}]
            }),
        );
    }

    pub fn save_document(&mut self, path: &Path) {
        if self.open.contains_key(path) {
            self.notify(
                "textDocument/didSave",
                json!({"textDocument": {"uri": uri::from_path(path)}}),
            );
        }
    }

    pub fn close_document(&mut self, path: &Path) {
        self.queued.retain(|(queued, _, _)| queued != path);
        if self.open.remove(path).is_some() {
            self.notify(
                "textDocument/didClose",
                json!({"textDocument": {"uri": uri::from_path(path)}}),
            );
        }
    }

    // ----- requests -----

    fn position_params(path: &Path, at: Position) -> Value {
        json!({
            "textDocument": {"uri": uri::from_path(path)},
            "position": {"line": at.line, "character": at.character}
        })
    }

    /// Asks what is at `at`; `None` before the server is running.
    pub fn hover(&mut self, path: &Path, at: Position) -> Option<u64> {
        (self.state == State::Running && self.open.contains_key(path)).then(|| {
            self.request(
                Pending::Hover,
                "textDocument/hover",
                Self::position_params(path, at),
            )
        })
    }

    pub fn definition(&mut self, path: &Path, at: Position) -> Option<u64> {
        (self.state == State::Running && self.open.contains_key(path)).then(|| {
            self.request(
                Pending::Definition,
                "textDocument/definition",
                Self::position_params(path, at),
            )
        })
    }

    pub fn completion(&mut self, path: &Path, at: Position) -> Option<u64> {
        (self.state == State::Running && self.open.contains_key(path)).then(|| {
            self.request(
                Pending::Completion,
                "textDocument/completion",
                Self::position_params(path, at),
            )
        })
    }

    pub fn rename(&mut self, path: &Path, at: Position, new_name: &str) -> Option<u64> {
        (self.state == State::Running && self.open.contains_key(path)).then(|| {
            let mut params = Self::position_params(path, at);
            params["newName"] = json!(new_name);
            self.request(Pending::Rename, "textDocument/rename", params)
        })
    }

    /// Asks the server to stop; the app ends the process afterwards.
    pub fn shutdown(&mut self) {
        if self.state == State::ShuttingDown {
            return;
        }
        self.state = State::ShuttingDown;
        self.request(Pending::Shutdown, "shutdown", Value::Null);
        self.notify("exit", Value::Null);
    }

    // ----- what arrives -----

    pub fn handle(&mut self, message: Value) {
        let method = message.get("method").and_then(Value::as_str);
        let id = message.get("id").cloned();
        match (method, id) {
            (Some(method), Some(id)) => self.answer_server_request(method, id, &message),
            (Some(method), None) => self.notification(method, message.get("params")),
            (None, Some(id)) => {
                if let Some(id) = id.as_u64() {
                    self.response(id, &message);
                }
            }
            (None, None) => {}
        }
    }

    /// The server asking something: rust-analyzer asks for little the
    /// client declared it could do; anything else gets an empty answer so
    /// the server is never left waiting.
    fn answer_server_request(&mut self, method: &str, id: Value, message: &Value) {
        let result = match method {
            "workspace/configuration" => {
                let count = message["params"]["items"].as_array().map_or(0, Vec::len);
                Value::Array(vec![Value::Null; count])
            }
            _ => Value::Null,
        };
        self.outgoing
            .push(json!({"jsonrpc": "2.0", "id": id, "result": result}));
    }

    fn notification(&mut self, method: &str, params: Option<&Value>) {
        let Some(params) = params else {
            return;
        };
        match method {
            "textDocument/publishDiagnostics" => {
                let Some(path) = params["uri"].as_str().and_then(uri::to_path) else {
                    return;
                };
                let diagnostics: Vec<Diagnostic> = params["diagnostics"]
                    .as_array()
                    .map(|list| {
                        list.iter()
                            .filter_map(|item| diagnostic(&path, item))
                            .collect()
                    })
                    .unwrap_or_default();
                if diagnostics.is_empty() {
                    self.diagnostics.remove(&path);
                } else {
                    self.diagnostics.insert(path, diagnostics);
                }
                self.diagnostics_revision += 1;
            }
            "experimental/serverStatus" => {
                self.ready = params["quiescent"].as_bool().unwrap_or(false);
                let health = params["health"].as_str().unwrap_or("ok");
                let message = params["message"].as_str().unwrap_or("");
                self.status = match (health, self.ready) {
                    ("ok", true) => "ready".to_string(),
                    ("ok", false) => "indexing…".to_string(),
                    (health, _) if message.is_empty() => health.to_string(),
                    (health, _) => format!("{health}: {}", message.lines().next().unwrap_or("")),
                };
            }
            _ => {}
        }
    }

    fn response(&mut self, id: u64, message: &Value) {
        let Some(kind) = self.pending.remove(&id) else {
            return;
        };
        if let Some(error) = message.get("error") {
            let text = error["message"]
                .as_str()
                .unwrap_or("request failed")
                .to_string();
            if kind == Pending::Initialize {
                self.status = format!("failed to start: {text}");
            } else {
                self.events.push(LspEvent::Failed {
                    request: id,
                    message: text,
                });
            }
            return;
        }
        let result = message.get("result").unwrap_or(&Value::Null);
        match kind {
            Pending::Initialize => {
                self.utf32 = result["capabilities"]["positionEncoding"].as_str() == Some("utf-32");
                self.state = State::Running;
                self.status = "loading the workspace…".to_string();
                self.notify("initialized", json!({}));
                for (path, text, revision) in std::mem::take(&mut self.queued) {
                    self.open_document(&path, &text, revision);
                }
            }
            Pending::Hover => {
                let text = hover_text(&result["contents"]);
                self.events.push(LspEvent::Hover { request: id, text });
            }
            Pending::Definition => {
                self.events.push(LspEvent::Definition {
                    request: id,
                    locations: locations(result),
                });
            }
            Pending::Completion => {
                let list = result
                    .get("items")
                    .unwrap_or(result)
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                let mut items: Vec<CompletionItem> =
                    list.iter().filter_map(completion_item).collect();
                items.sort_by(|a, b| a.sort_text.cmp(&b.sort_text));
                self.events
                    .push(LspEvent::Completion { request: id, items });
            }
            Pending::Rename => {
                self.events.push(LspEvent::Edit {
                    request: id,
                    edits: workspace_edit(result),
                });
            }
            Pending::Shutdown => {}
        }
    }

    /// Whether the server agreed to UTF-32 positions.
    pub fn utf32(&self) -> bool {
        self.utf32
    }
}

fn position(value: &Value) -> Option<Position> {
    Some(Position {
        line: value["line"].as_u64()? as usize,
        character: value["character"].as_u64()? as usize,
    })
}

fn range(value: &Value) -> Option<Range> {
    Some(Range {
        start: position(&value["start"])?,
        end: position(&value["end"])?,
    })
}

fn text_edit(value: &Value) -> Option<TextEdit> {
    Some(TextEdit {
        range: range(&value["range"])?,
        new_text: value["newText"].as_str()?.to_string(),
    })
}

fn diagnostic(path: &Path, value: &Value) -> Option<Diagnostic> {
    let level = match value["severity"].as_u64() {
        Some(1) => Level::Error,
        Some(2) => Level::Warning,
        _ => return None,
    };
    let range = range(&value["range"])?;
    let code = match &value["code"] {
        Value::String(code) => Some(code.clone()),
        Value::Number(code) => Some(code.to_string()),
        _ => None,
    };
    Some(Diagnostic {
        level,
        file: path.to_path_buf(),
        line: range.start.line + 1,
        column: range.start.character + 1,
        end_line: range.end.line + 1,
        end_column: range.end.character + 1,
        message: value["message"].as_str()?.to_string(),
        label: None,
        code,
    })
}

/// Hover contents as plain text: markdown code fences dropped, their code
/// kept.
fn hover_text(contents: &Value) -> String {
    let raw = match contents {
        Value::String(text) => text.clone(),
        Value::Object(object) => object
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        Value::Array(parts) => parts
            .iter()
            .map(hover_text)
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ => String::new(),
    };
    let mut lines = Vec::new();
    for line in raw.lines() {
        if line.trim_start().starts_with("```") {
            continue;
        }
        lines.push(line.trim_end());
    }
    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

fn location(value: &Value) -> Option<Location> {
    // Location, or LocationLink (targetUri and targetSelectionRange).
    let (uri_value, range_value) = if value.get("targetUri").is_some() {
        (&value["targetUri"], &value["targetSelectionRange"])
    } else {
        (&value["uri"], &value["range"])
    };
    Some(Location {
        path: uri::to_path(uri_value.as_str()?)?,
        range: range(range_value)?,
    })
}

fn locations(result: &Value) -> Vec<Location> {
    match result {
        Value::Array(list) => list.iter().filter_map(location).collect(),
        Value::Object(_) => location(result).into_iter().collect(),
        _ => Vec::new(),
    }
}

fn completion_item(value: &Value) -> Option<CompletionItem> {
    let label = value["label"].as_str()?.to_string();
    let edit = value.get("textEdit").and_then(|edit| {
        if edit.get("range").is_some() {
            text_edit(edit)
        } else {
            // InsertReplaceEdit: use the insert range.
            Some(TextEdit {
                range: range(&edit["insert"])?,
                new_text: edit["newText"].as_str()?.to_string(),
            })
        }
    });
    Some(CompletionItem {
        detail: value["detail"].as_str().map(str::to_string),
        insert_text: value["insertText"]
            .as_str()
            .map_or_else(|| label.clone(), str::to_string),
        additional_edits: value["additionalTextEdits"]
            .as_array()
            .map(|edits| edits.iter().filter_map(text_edit).collect())
            .unwrap_or_default(),
        sort_text: value["sortText"]
            .as_str()
            .map_or_else(|| label.clone(), str::to_string),
        edit,
        label,
    })
}

/// A WorkspaceEdit's edits per file, from `documentChanges` or `changes`.
fn workspace_edit(result: &Value) -> Vec<(PathBuf, Vec<TextEdit>)> {
    let mut edits: Vec<(PathBuf, Vec<TextEdit>)> = Vec::new();
    if let Some(changes) = result["documentChanges"].as_array() {
        for change in changes {
            let Some(path) = change["textDocument"]["uri"]
                .as_str()
                .and_then(uri::to_path)
            else {
                continue;
            };
            let list = change["edits"]
                .as_array()
                .map(|list| list.iter().filter_map(text_edit).collect())
                .unwrap_or_default();
            edits.push((path, list));
        }
    } else if let Some(changes) = result["changes"].as_object() {
        for (uri_text, list) in changes {
            let Some(path) = uri::to_path(uri_text) else {
                continue;
            };
            let list = list
                .as_array()
                .map(|list| list.iter().filter_map(text_edit).collect())
                .unwrap_or_default();
            edits.push((path, list));
        }
    }
    edits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn started() -> LspSession {
        let mut session = LspSession::new(Path::new("/w"));
        let out = session.take_outgoing();
        assert_eq!(out[0]["method"], "initialize");
        assert_eq!(out[0]["params"]["rootUri"], "file:///w");
        session.handle(json!({"jsonrpc": "2.0", "id": 1, "result": {
            "capabilities": {"positionEncoding": "utf-32"}
        }}));
        session
    }

    #[test]
    fn documents_opened_early_are_sent_after_initialization() {
        let mut session = LspSession::new(Path::new("/w"));
        session.take_outgoing();
        session.open_document(Path::new("/w/src/lib.rs"), "fn a() {}", 1);
        session.change_document(Path::new("/w/src/lib.rs"), "fn b() {}", 2);
        assert!(
            session.take_outgoing().is_empty(),
            "nothing before initialize"
        );
        session.handle(json!({"jsonrpc": "2.0", "id": 1, "result": {
            "capabilities": {"positionEncoding": "utf-32"}
        }}));
        assert!(session.utf32());
        let out = session.take_outgoing();
        assert_eq!(out[0]["method"], "initialized");
        assert_eq!(out[1]["method"], "textDocument/didOpen");
        assert_eq!(out[1]["params"]["textDocument"]["text"], "fn b() {}");
        assert!(session.in_sync(Path::new("/w/src/lib.rs"), 2));
    }

    #[test]
    fn changes_are_sent_once_per_revision_with_rising_versions() {
        let mut session = started();
        session.take_outgoing();
        let path = Path::new("/w/src/lib.rs");
        session.open_document(path, "a", 1);
        session.change_document(path, "ab", 2);
        session.change_document(path, "ab", 2);
        session.change_document(path, "abc", 3);
        let out = session.take_outgoing();
        let methods: Vec<_> = out.iter().map(|m| m["method"].clone()).collect();
        assert_eq!(
            methods,
            vec![
                "textDocument/didOpen",
                "textDocument/didChange",
                "textDocument/didChange"
            ]
        );
        assert_eq!(out[2]["params"]["textDocument"]["version"], 3);
        assert_eq!(out[2]["params"]["contentChanges"][0]["text"], "abc");
        session.close_document(path);
        assert_eq!(
            session.take_outgoing()[0]["method"],
            "textDocument/didClose"
        );
    }

    #[test]
    fn diagnostics_keep_errors_and_warnings_as_one_based_positions() {
        let mut session = started();
        session.handle(json!({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics",
            "params": {"uri": "file:///w/src/lib.rs", "diagnostics": [
                {"range": {"start": {"line": 1, "character": 4}, "end": {"line": 1, "character": 9}},
                 "severity": 1, "code": "E0425", "message": "cannot find value `x`"},
                {"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}},
                 "severity": 4, "message": "a hint"}
            ]}}));
        let found = session.diagnostics(Path::new("/w/src/lib.rs"));
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].line, found[0].column), (2, 5));
        assert_eq!(found[0].code.as_deref(), Some("E0425"));
        session.handle(
            json!({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics",
            "params": {"uri": "file:///w/src/lib.rs", "diagnostics": []}}),
        );
        assert!(session.diagnostics(Path::new("/w/src/lib.rs")).is_empty());
    }

    #[test]
    fn server_requests_always_get_an_answer() {
        let mut session = started();
        session.take_outgoing();
        session.handle(
            json!({"jsonrpc": "2.0", "id": 9, "method": "client/registerCapability", "params": {}}),
        );
        session.handle(
            json!({"jsonrpc": "2.0", "id": "c1", "method": "workspace/configuration",
            "params": {"items": [{}, {}]}}),
        );
        let out = session.take_outgoing();
        assert_eq!(out[0], json!({"jsonrpc": "2.0", "id": 9, "result": null}));
        assert_eq!(out[1]["result"], json!([null, null]));
    }

    #[test]
    fn hover_definition_completion_and_rename_results() {
        let mut session = started();
        let path = Path::new("/w/src/lib.rs");
        session.open_document(path, "", 1);
        let at = Position {
            line: 3,
            character: 7,
        };
        let hover = session.hover(path, at).unwrap();
        let definition = session.definition(path, at).unwrap();
        let completion = session.completion(path, at).unwrap();
        let rename = session.rename(path, at, "count").unwrap();
        session.handle(json!({"jsonrpc": "2.0", "id": hover, "result": {"contents": {
            "kind": "markdown", "value": "```rust\npub fn len(&self) -> usize\n```\n\nReturns the length."
        }}}));
        session.handle(json!({"jsonrpc": "2.0", "id": definition, "result": [{
            "targetUri": "file:///w/src/other.rs",
            "targetRange": {"start": {"line": 0, "character": 0}, "end": {"line": 9, "character": 1}},
            "targetSelectionRange": {"start": {"line": 2, "character": 7}, "end": {"line": 2, "character": 10}}
        }]}));
        session.handle(json!({"jsonrpc": "2.0", "id": completion, "result": {"isIncomplete": true, "items": [
            {"label": "zeta", "sortText": "b"},
            {"label": "alpha", "sortText": "a", "detail": "fn()",
             "textEdit": {"range": {"start": {"line": 3, "character": 4}, "end": {"line": 3, "character": 7}}, "newText": "alpha"},
             "additionalTextEdits": [{"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}}, "newText": "use x::alpha;\n"}]}
        ]}}));
        session.handle(json!({"jsonrpc": "2.0", "id": rename, "result": {"documentChanges": [{
            "textDocument": {"uri": "file:///w/src/lib.rs", "version": 1},
            "edits": [{"range": {"start": {"line": 3, "character": 4}, "end": {"line": 3, "character": 9}}, "newText": "count"}]
        }]}}));
        let events = session.take_events();
        assert_eq!(
            events[0],
            LspEvent::Hover {
                request: hover,
                text: "pub fn len(&self) -> usize\n\nReturns the length.".to_string()
            }
        );
        let LspEvent::Definition { locations, .. } = &events[1] else {
            panic!("{events:?}");
        };
        assert_eq!(locations[0].path, PathBuf::from("/w/src/other.rs"));
        assert_eq!(
            locations[0].range.start,
            Position {
                line: 2,
                character: 7
            }
        );
        let LspEvent::Completion { items, .. } = &events[2] else {
            panic!("{events:?}");
        };
        assert_eq!(items[0].label, "alpha", "sorted by sortText");
        assert_eq!(items[0].additional_edits[0].new_text, "use x::alpha;\n");
        assert_eq!(items[1].insert_text, "zeta");
        let LspEvent::Edit { edits, .. } = &events[3] else {
            panic!("{events:?}");
        };
        assert_eq!(edits[0].0, PathBuf::from("/w/src/lib.rs"));
        assert_eq!(edits[0].1[0].new_text, "count");
    }

    #[test]
    fn server_status_reads_as_ready_or_indexing() {
        let mut session = started();
        session.handle(
            json!({"jsonrpc": "2.0", "method": "experimental/serverStatus",
            "params": {"health": "ok", "quiescent": false}}),
        );
        assert_eq!(session.status, "indexing…");
        session.handle(
            json!({"jsonrpc": "2.0", "method": "experimental/serverStatus",
            "params": {"health": "ok", "quiescent": true}}),
        );
        assert!(session.ready);
        assert_eq!(session.status, "ready");
    }
}
