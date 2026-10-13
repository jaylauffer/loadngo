//! Calls to a model host on this machine: one JSON object per line on a Unix socket in a
//! directory only the user can open (`~/.loadngo/node/`, mode 0700), with no token, as
//! `docs/LOADNGO_NODE.md` plans for same-machine calls. `gpt_oss_serve` answers them.
//!
//! A call, on one line:
//!
//! ```json
//! {"call_id": 7, "capability": "chat", "deadline_ms": 30000,
//!  "body": {"instructions": "You are the talking head ...", "message": "How do I save them?"}}
//! ```
//!
//! and its reply, on one line: `{"call_id": 7, "status": "ok", "body": {"text": "..."}}`.
//! `status` is `ok`, `expired` (the deadline passed before the answer was finished),
//! `busy` (the queue was full) or `error` (`body.error` says why). `deadline_ms` counts
//! from when the host read the call.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// The capability that answers a message, optionally under instructions.
pub const CHAT: &str = "chat";
/// Bytes a call may take before its newline.
pub const MAX_CALL_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct Call {
    pub call_id: u64,
    pub capability: String,
    pub deadline_ms: u64,
    #[serde(default)]
    pub body: Value,
}

/// A `chat` call's body.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct Chat {
    #[serde(default)]
    pub instructions: Option<String>,
    pub message: String,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Ok,
    Expired,
    Busy,
    Error,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Reply {
    pub call_id: u64,
    pub status: Status,
    pub body: Value,
}

impl Reply {
    #[must_use]
    pub fn text(call_id: u64, text: &str) -> Self {
        Self {
            call_id,
            status: Status::Ok,
            body: json!({ "text": text }),
        }
    }

    #[must_use]
    pub fn failed(call_id: u64, status: Status, why: &str) -> Self {
        Self {
            call_id,
            status,
            body: json!({ "error": why }),
        }
    }

    /// The reply as one line, newline included.
    #[must_use]
    pub fn line(&self) -> Vec<u8> {
        let mut bytes = serde_json::to_vec(self).expect("a reply serializes");
        bytes.push(b'\n');
        bytes
    }
}

/// Reads one call line: a `chat` call and its body, or the reply refusing it (`error`;
/// `call_id` 0 when the line has none).
///
/// # Errors
/// The refusal, as the reply to send.
pub fn read(line: &[u8]) -> Result<(Call, Chat), Reply> {
    let call: Call = serde_json::from_slice(line).map_err(|e| {
        let call_id = serde_json::from_slice::<Value>(line)
            .ok()
            .and_then(|v| v.get("call_id").and_then(Value::as_u64))
            .unwrap_or(0);
        Reply::failed(call_id, Status::Error, &format!("not a call: {e}"))
    })?;
    if call.capability != CHAT {
        return Err(Reply::failed(
            call.call_id,
            Status::Error,
            &format!(
                "no capability {:?} here; this host answers {CHAT:?}",
                call.capability
            ),
        ));
    }
    let chat: Chat = serde_json::from_value(call.body.clone()).map_err(|e| {
        Reply::failed(
            call.call_id,
            Status::Error,
            &format!("not a chat body: {e}"),
        )
    })?;
    Ok((call, chat))
}

#[cfg(test)]
mod tests {
    use super::{read, Reply, Status};
    use serde_json::json;

    #[test]
    fn a_chat_call_is_read_and_anything_else_is_refused_with_its_call_id() {
        let (call, chat) = read(
            br#"{"call_id": 7, "capability": "chat", "deadline_ms": 30000,
                 "body": {"instructions": "Be brief.", "message": "Hello?"}}"#,
        )
        .unwrap();
        assert_eq!((call.call_id, call.deadline_ms), (7, 30000));
        assert_eq!(chat.instructions.as_deref(), Some("Be brief."));
        assert_eq!(chat.message, "Hello?");

        let no_instructions = read(
            br#"{"call_id": 8, "capability": "chat", "deadline_ms": 1,
                                        "body": {"message": "Hi"}}"#,
        );
        assert_eq!(no_instructions.unwrap().1.instructions, None);

        let refused = |line: &[u8]| read(line).unwrap_err();
        let paint = refused(br#"{"call_id": 9, "capability": "paint", "deadline_ms": 1}"#);
        assert_eq!((paint.call_id, paint.status), (9, Status::Error));
        let no_message = refused(br#"{"call_id": 10, "capability": "chat", "deadline_ms": 1}"#);
        assert_eq!((no_message.call_id, no_message.status), (10, Status::Error));
        let no_deadline = refused(br#"{"call_id": 11, "capability": "chat"}"#);
        assert_eq!(
            (no_deadline.call_id, no_deadline.status),
            (11, Status::Error)
        );
        let garbage = refused(b"this is not json");
        assert_eq!((garbage.call_id, garbage.status), (0, Status::Error));
    }

    #[test]
    fn a_reply_is_one_json_line() {
        let line = Reply::text(7, "Fly through the gate.").line();
        assert_eq!(line.last(), Some(&b'\n'));
        let value: serde_json::Value = serde_json::from_slice(&line).unwrap();
        assert_eq!(
            value,
            json!({"call_id": 7, "status": "ok", "body": {"text": "Fly through the gate."}})
        );
        let busy: serde_json::Value =
            serde_json::from_slice(&Reply::failed(8, Status::Busy, "two calls wait").line())
                .unwrap();
        assert_eq!(busy["status"], "busy");
        assert_eq!(busy["body"]["error"], "two calls wait");
    }
}
