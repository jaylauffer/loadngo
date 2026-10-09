//! Saved chats: one JSON event per line, written as the chat goes, so a chat can be
//! read afterwards (what Jay asked, every reply, call and result, how each turn ended).
//! The event names are the ones Kimi's chat has written since 2026-09-30 (`start`,
//! `user`, `reply`, `tool_call`, `tool_result`, `turn_end`, `exit`), so older and newer
//! transcripts read the same way. Beside each log, `<time>.state.json` holds what is
//! needed to resume the chat (the agent's [`super::Agent::state`]), rewritten through a
//! temporary file after every round; [`Transcript::resume`] reopens both.
//!
//! Each event is appended and flushed as it happens, so a crash or power cut loses at
//! most the event being written. They are small (a result is cut at 64 KiB).

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use chrono::Local;
use serde_json::{json, Value};

use super::{Ended, Event, Observer};

/// Longest result text kept, in bytes.
const RESULT_BYTES: usize = 64 * 1024;

pub struct Transcript {
    path: PathBuf,
    format: String,
    file: Option<File>,
}

/// `x.jsonl` -> `x.state.json`.
fn state_path(log: &Path) -> PathBuf {
    let stem = log.file_stem().and_then(|s| s.to_str()).unwrap_or("chat");
    log.with_file_name(format!("{stem}.state.json"))
}

impl Transcript {
    /// A new transcript in `dir` (made if missing), named by the local time, opening
    /// with a `start` event naming the chat `format` and the `model`.
    ///
    /// # Errors
    /// When the directory or file cannot be made.
    pub fn create(dir: &Path, format: &str, model: &str) -> Result<Self, String> {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let stamp = Local::now().format("%Y-%m-%d-%H%M%S");
        let mut path = dir.join(format!("{stamp}.jsonl"));
        let mut n = 1;
        while path.exists() {
            n += 1;
            path = dir.join(format!("{stamp}-{n}.jsonl"));
        }
        let file = OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(&path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let mut transcript = Self {
            path,
            format: format.to_owned(),
            file: Some(file),
        };
        transcript.write(json!({"event": "start", "format": format, "model": model}));
        Ok(transcript)
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reopens a saved chat in `dir`: `latest` (the newest snapshot of this `format`), or
    /// the path of a `.jsonl` log or its `.state.json`. The log goes on from where it
    /// ended. Returns the transcript and the saved state.
    ///
    /// # Errors
    /// When there is no snapshot, it cannot be read, or another chat format saved it.
    pub fn resume(dir: &Path, which: &str, format: &str) -> Result<(Self, Value), String> {
        let state = if which == "latest" {
            let mut found: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(dir)
                .map_err(|e| format!("{}: {e}", dir.display()))?
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.to_string_lossy().ends_with(".state.json"))
                .filter_map(|p| Some((p.metadata().ok()?.modified().ok()?, p)))
                .collect();
            found.sort();
            found
                .into_iter()
                .rev()
                .map(|(_, p)| p)
                .find(|p| {
                    std::fs::read(p)
                        .ok()
                        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
                        .is_some_and(|v| v["format"] == format)
                })
                .ok_or_else(|| format!("no saved {format} chat in {}", dir.display()))?
        } else if which.ends_with(".jsonl") {
            state_path(Path::new(which))
        } else {
            PathBuf::from(which)
        };
        let bytes = std::fs::read(&state).map_err(|e| format!("{}: {e}", state.display()))?;
        let saved: Value =
            serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", state.display()))?;
        if saved["format"] != format {
            return Err(format!(
                "{} was saved by the {} chat, not {format}",
                state.display(),
                saved["format"]
            ));
        }
        let stem = state
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".state.json"))
            .ok_or("not a .state.json path")?;
        let path = state.with_file_name(format!("{stem}.jsonl"));
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let mut transcript = Self {
            path,
            format: format.to_owned(),
            file: Some(file),
        };
        transcript.write(json!({"event": "resume", "format": format}));
        Ok((transcript, saved))
    }

    /// Rewrites the snapshot beside the log, through a temporary file.
    fn save_state(&self, state: &Value) {
        let mut state = state.clone();
        state["format"] = Value::String(self.format.clone());
        let path = state_path(&self.path);
        let tmp = path.with_extension("json.tmp");
        let written =
            std::fs::write(&tmp, state.to_string()).and_then(|()| std::fs::rename(&tmp, &path));
        if let Err(e) = written {
            eprintln!(
                "transcript {}: {e}; the chat cannot be resumed",
                path.display()
            );
        }
    }

    /// Writes one event with its local time. A failed write is reported once and the
    /// transcript stops; the chat goes on.
    pub fn write(&mut self, mut event: Value) {
        let Some(file) = &mut self.file else { return };
        event["at"] = Value::String(Local::now().to_rfc3339());
        let mut line = event.to_string();
        line.push('\n');
        if let Err(e) = file.write_all(line.as_bytes()).and_then(|()| file.flush()) {
            eprintln!("transcript {}: {e}; no longer saved", self.path.display());
            self.file = None;
        }
    }
}

fn ended(e: Ended) -> &'static str {
    match e {
        Ended::Stop => "EndToken",
        Ended::Limit => "TokenLimit",
        Ended::Context => "ContextLimit",
        Ended::Cancelled => "Cancelled",
        Ended::Halted => "Halted",
    }
}

/// `text` cut at `limit` bytes on a character boundary.
fn cut(text: &str, limit: usize) -> (&str, bool) {
    if text.len() <= limit {
        return (text, false);
    }
    let at = (0..=limit)
        .rev()
        .find(|&i| text.is_char_boundary(i))
        .unwrap_or(0);
    (&text[..at], true)
}

impl Observer for Transcript {
    fn event(&mut self, event: Event<'_>) {
        let value = match event {
            Event::User(text) => json!({"event": "user", "text": text}),
            Event::Prompt { .. } | Event::Token(_) => return,
            Event::Reply {
                read,
                tokens,
                seconds,
                ended: e,
            } => {
                let calls: Vec<Value> = read
                    .calls
                    .iter()
                    .map(|c| json!({"name": c.name, "arguments": c.arguments}))
                    .collect();
                json!({"event": "reply", "text": read.answer, "reasoning": read.reasoning,
                       "calls": calls, "tokens": tokens, "stop": ended(e), "seconds": seconds})
            }
            Event::Call { name, arguments } => {
                json!({"event": "tool_call", "name": name, "arguments": arguments})
            }
            Event::Result { name, text, ok } => {
                let (text, was_cut) = cut(text, RESULT_BYTES);
                json!({"event": "tool_result", "name": name, "text": text, "ok": ok, "cut": was_cut})
            }
            Event::Note(text) => json!({"event": "note", "text": text}),
            Event::State(state) => {
                self.save_state(state);
                return;
            }
            Event::TurnEnd {
                stopped,
                replies,
                tokens,
                seconds,
            } => json!({"event": "turn_end", "stop": stopped.unwrap_or("answered"),
                         "replies": replies, "tokens": tokens, "seconds": seconds}),
        };
        self.write(value);
    }
}

impl Drop for Transcript {
    fn drop(&mut self) {
        self.write(json!({"event": "exit"}));
    }
}

/// Several observers, each seeing every event in turn.
pub struct Tee<'o>(pub Vec<Box<dyn Observer + 'o>>);

impl Observer for Tee<'_> {
    fn event(&mut self, event: Event<'_>) {
        // Events borrow; rebuild each for the next observer.
        for observer in &mut self.0 {
            observer.event(match &event {
                Event::User(t) => Event::User(t),
                Event::Prompt { tokens, seconds } => Event::Prompt {
                    tokens: *tokens,
                    seconds: *seconds,
                },
                Event::Token(t) => Event::Token(*t),
                Event::Reply {
                    read,
                    tokens,
                    seconds,
                    ended,
                } => Event::Reply {
                    read,
                    tokens: *tokens,
                    seconds: *seconds,
                    ended: *ended,
                },
                Event::Call { name, arguments } => Event::Call { name, arguments },
                Event::Result { name, text, ok } => Event::Result {
                    name,
                    text,
                    ok: *ok,
                },
                Event::Note(t) => Event::Note(t),
                Event::State(v) => Event::State(v),
                Event::TurnEnd {
                    stopped,
                    replies,
                    tokens,
                    seconds,
                } => Event::TurnEnd {
                    stopped: *stopped,
                    replies: *replies,
                    tokens: *tokens,
                    seconds: *seconds,
                },
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{Call, Read};

    #[test]
    fn a_transcript_holds_each_event_as_a_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = {
            let mut t = Transcript::create(dir.path(), "toy", "model-x").unwrap();
            t.event(Event::User("read a.rs"));
            t.event(Event::Token(5));
            let read = Read {
                calls: vec![Call {
                    id: None,
                    name: "fs_read".into(),
                    arguments: "{}".into(),
                }],
                ..Read::default()
            };
            t.event(Event::Reply {
                read: &read,
                tokens: 7,
                seconds: 0.5,
                ended: Ended::Stop,
            });
            t.event(Event::Result {
                name: "fs_read",
                text: &"é".repeat(40_000),
                ok: true,
            });
            t.event(Event::TurnEnd {
                stopped: None,
                replies: 2,
                tokens: 9,
                seconds: 1.0,
            });
            t.path().to_owned()
        };
        let lines: Vec<Value> = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let events: Vec<&str> = lines.iter().map(|l| l["event"].as_str().unwrap()).collect();
        assert_eq!(
            events,
            ["start", "user", "reply", "tool_result", "turn_end", "exit"]
        );
        assert_eq!(lines[0]["model"], "model-x");
        assert_eq!(lines[2]["calls"][0]["name"], "fs_read");
        assert_eq!(lines[3]["cut"], true);
        assert!(lines[3]["text"].as_str().unwrap().len() <= RESULT_BYTES);
        assert_eq!(lines[4]["stop"], "answered");
        assert!(lines.iter().all(|l| l["at"].is_string()));
        // A snapshot is written beside the log and found again.
        {
            let mut t = Transcript::create(dir.path(), "toy", "m").unwrap();
            t.event(Event::State(&json!({"version": 2, "tokens": [1, 2]})));
        }
        let (resumed, saved) = Transcript::resume(dir.path(), "latest", "toy").unwrap();
        assert_eq!(saved["tokens"], json!([1, 2]));
        assert!(resumed.path().to_string_lossy().ends_with(".jsonl"));
        assert!(Transcript::resume(dir.path(), "latest", "other").is_err());
        drop(resumed);
        // A second transcript in the same second gets its own file.
        let a = Transcript::create(dir.path(), "toy", "m").unwrap();
        let b = Transcript::create(dir.path(), "toy", "m").unwrap();
        assert_ne!(a.path(), b.path());
    }
}
