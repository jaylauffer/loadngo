//! A language server as a child process, speaking JSON-RPC over stdio with
//! `Content-Length` framing.
//!
//! The app's thread never blocks on the server: one thread writes queued
//! messages to its stdin, one reads its stdout into a queue and calls a
//! wake function after each message, one drains its stderr. The app takes
//! what arrived with [`LspProcess::drain`] on its next frame.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::Value;

/// Frames one message for the wire.
pub fn encode(message: &Value) -> Vec<u8> {
    let body = message.to_string();
    let mut bytes = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    bytes.extend_from_slice(body.as_bytes());
    bytes
}

/// Reads one framed message: `Ok(None)` at the end of the stream.
pub fn read_message(reader: &mut impl BufRead) -> std::io::Result<Option<Value>> {
    let mut length = None;
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let header = line.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some(value) = header.strip_prefix("Content-Length:") {
            length = value.trim().parse::<usize>().ok();
        }
    }
    let Some(length) = length else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "message without Content-Length",
        ));
    };
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body)?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

/// What the reader thread collected.
#[derive(Default)]
struct Inbox {
    messages: VecDeque<Value>,
    ended: Option<String>,
}

pub struct LspProcess {
    child: Child,
    outgoing: Sender<Vec<u8>>,
    inbox: Arc<Mutex<Inbox>>,
    stderr_tail: Arc<Mutex<String>>,
}

/// `program` from `PATH`, else `~/.cargo/bin` (a Finder launch has no shell
/// `PATH`).
pub fn find_program(program: &str) -> PathBuf {
    let on_path = std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(program).is_file()));
    if on_path {
        return PathBuf::from(program);
    }
    std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(".cargo/bin").join(program))
        .filter(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from(program))
}

impl LspProcess {
    /// Starts `program` in `dir`. `wake` is called (from the reader thread)
    /// after each message arrives and when the server's output ends.
    pub fn start(
        program: &Path,
        dir: &Path,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> std::io::Result<Self> {
        let mut child = Command::new(program)
            .current_dir(dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut stdin = child.stdin.take().expect("piped");
        let stdout = child.stdout.take().expect("piped");
        let mut stderr = child.stderr.take().expect("piped");

        let (outgoing, queued) = mpsc::channel::<Vec<u8>>();
        std::thread::Builder::new()
            .name("lsp-writer".to_string())
            .spawn(move || {
                for bytes in queued {
                    if stdin
                        .write_all(&bytes)
                        .and_then(|()| stdin.flush())
                        .is_err()
                    {
                        break;
                    }
                }
            })?;

        let inbox = Arc::new(Mutex::new(Inbox::default()));
        let filled = Arc::clone(&inbox);
        std::thread::Builder::new()
            .name("lsp-reader".to_string())
            .spawn(move || {
                let mut reader = BufReader::new(stdout);
                let ended = loop {
                    match read_message(&mut reader) {
                        Ok(Some(message)) => {
                            filled
                                .lock()
                                .unwrap_or_else(PoisonError::into_inner)
                                .messages
                                .push_back(message);
                            wake();
                        }
                        Ok(None) => break "the server closed its output".to_string(),
                        Err(error) => break format!("unreadable message from the server: {error}"),
                    }
                };
                filled.lock().unwrap_or_else(PoisonError::into_inner).ended = Some(ended);
                wake();
            })?;

        let stderr_tail = Arc::new(Mutex::new(String::new()));
        let tail = Arc::clone(&stderr_tail);
        std::thread::Builder::new()
            .name("lsp-stderr".to_string())
            .spawn(move || {
                let mut chunk = [0u8; 4096];
                while let Ok(n) = stderr.read(&mut chunk) {
                    if n == 0 {
                        break;
                    }
                    let mut tail = tail.lock().unwrap_or_else(PoisonError::into_inner);
                    tail.push_str(&String::from_utf8_lossy(&chunk[..n]));
                    if tail.len() > 8192 {
                        let cut = tail.len() - 4096;
                        let cut = (cut..tail.len())
                            .find(|&i| tail.is_char_boundary(i))
                            .unwrap_or(tail.len());
                        tail.drain(..cut);
                    }
                }
            })?;

        Ok(Self {
            child,
            outgoing,
            inbox,
            stderr_tail,
        })
    }

    pub fn send(&self, message: &Value) {
        let _ = self.outgoing.send(encode(message));
    }

    /// The messages that arrived since the last call.
    pub fn drain(&self) -> Vec<Value> {
        self.inbox
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .messages
            .drain(..)
            .collect()
    }

    /// Why the server's output ended, once it has, with the end of what it
    /// wrote to stderr.
    pub fn ended(&self) -> Option<String> {
        let ended = self
            .inbox
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .ended
            .clone()?;
        let tail = self
            .stderr_tail
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .trim()
            .to_string();
        Some(if tail.is_empty() {
            ended
        } else {
            format!("{ended}: {}", tail.lines().last().unwrap_or_default())
        })
    }
}

impl Drop for LspProcess {
    fn drop(&mut self) {
        // The polite shutdown (shutdown request, exit notification) is the
        // session's; whatever is left is stopped here so no server outlives
        // the editor.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn messages_round_trip_through_the_framing() {
        let first =
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"é": "ü"}});
        let second = json!({"jsonrpc": "2.0", "method": "initialized", "params": {}});
        let mut wire = encode(&first);
        wire.extend(encode(&second));
        let mut reader = BufReader::new(wire.as_slice());
        assert_eq!(read_message(&mut reader).unwrap(), Some(first));
        assert_eq!(read_message(&mut reader).unwrap(), Some(second));
        assert_eq!(read_message(&mut reader).unwrap(), None);
    }

    #[test]
    fn extra_headers_are_skipped_and_a_missing_length_is_an_error() {
        let body = r#"{"id":2}"#;
        let wire = format!(
            "Content-Type: application/vscode-jsonrpc; charset=utf-8\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let mut reader = BufReader::new(wire.as_bytes());
        assert_eq!(read_message(&mut reader).unwrap(), Some(json!({"id": 2})));
        let mut bad = BufReader::new("X: 1\r\n\r\n{}".as_bytes());
        assert!(read_message(&mut bad).is_err());
    }
}
