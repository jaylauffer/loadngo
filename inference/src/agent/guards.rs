//! Mechanical checks on a model's work that need no judging: a reply looping on one
//! block, the same call made again, an edit that puts a file back as it was.

use std::collections::HashMap;

use serde_json::Value;

/// Copies of one block, at least, before a reply counts as looping.
const LOOP_COPIES: usize = 4;
/// Tokens the copies must cover together, at least.
const LOOP_TOKENS: usize = 64;
/// Longest block looked for.
const LOOP_PERIOD: usize = 200;

/// The length of the block the end of `reply` repeats, when it has repeated it at least
/// four times back to back over at least 64 tokens: a model that will not stop on its
/// own. Ordinary repetition (a list's bullets, a table's rows) differs somewhere within
/// each copy and is not caught.
#[must_use]
pub fn looping(reply: &[u32]) -> Option<usize> {
    let n = reply.len();
    (1..=LOOP_PERIOD.min(n / LOOP_COPIES)).find(|&period| {
        let span = (period * LOOP_COPIES).max(LOOP_TOKENS);
        n >= span && (n - span..n - period).all(|i| reply[i] == reply[i + period])
    })
}

/// A call's identity for the repeat guard: its name and its arguments as JSON, so key
/// order and spacing do not make two calls different.
#[must_use]
pub fn call_key(name: &str, arguments: &str) -> (String, Value) {
    let value =
        serde_json::from_str(arguments).unwrap_or_else(|_| Value::String(arguments.trim().into()));
    (name.to_owned(), value)
}

/// Tools whose results change from call to call even with the same arguments, so a
/// repeat is not refused: builds and tests, repository state, terminal sessions.
#[must_use]
pub fn repeatable(name: &str) -> bool {
    matches!(name, "cargo" | "git") || name.starts_with("terminal_")
}

/// Tools that change files or stored state: once one succeeds, earlier reads may read
/// differently, so they may be made again.
#[must_use]
pub fn mutates(name: &str) -> bool {
    matches!(
        name,
        "text_edit" | "text_write" | "text_format" | "memory_save" | "memory_forget"
    ) || name.starts_with("board_add")
        || name.starts_with("terminal_")
}

/// Tools that write files, for the write-failure limit and the verification gate.
#[must_use]
pub fn writes(name: &str) -> bool {
    matches!(name, "text_edit" | "text_write" | "text_format")
}

/// A note when a successful text tool call returns a file to a revision it had earlier in
/// the turn: going back and forth between two versions does not fix what is wrong.
/// Records the revisions `result` shows (gpt-oss run 4, `docs/GPT_OSS.md`, flipped one
/// brace in and out three times).
pub fn undone(
    seen: &mut HashMap<String, Vec<String>>,
    arguments: &str,
    result: &str,
) -> Option<String> {
    let path = serde_json::from_str::<Value>(arguments)
        .ok()?
        .get("path")?
        .as_str()?
        .to_owned();
    let revision = result
        .split(|c: char| !c.is_ascii_hexdigit())
        .find(|w| w.len() == 16)?
        .to_owned();
    let earlier = seen.entry(path.clone()).or_default();
    let edit = ["edited", "replaced", "formatted"]
        .iter()
        .any(|w| result.starts_with(w));
    let back = edit && earlier.len() >= 2 && earlier[..earlier.len() - 1].contains(&revision);
    earlier.push(revision);
    back.then(|| {
        format!(
            "This change puts {path} back as it was earlier in this turn: you are undoing your \
             own edit. Going back and forth will not fix it. Read the error again: the line \
             numbers it names, and the bracket or item it points to, are where the problem is. \
             text_read those lines, then make one change there."
        )
    })
}

/// One line about a tool call, for Jev's checkpoints.
#[must_use]
pub fn work_line(n: usize, name: &str, arguments: &str, result: &str) -> String {
    let squash = |text: &str, max: usize| -> String {
        let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if flat.chars().count() > max {
            format!("{}…", flat.chars().take(max).collect::<String>())
        } else {
            flat
        }
    };
    format!(
        "{n}. {name} {} -> {}",
        squash(arguments, 160),
        squash(result, 200)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_looping_reply_is_found_and_ordinary_repetition_is_not() {
        let block: Vec<u32> = (100..120).collect();
        let mut reply: Vec<u32> = (1..10).collect();
        for _ in 0..3 {
            reply.extend(&block);
        }
        assert_eq!(looping(&reply), None, "three copies are not yet a loop");
        reply.extend(&block);
        assert_eq!(looping(&reply), Some(20));
        // A short block must cover 64 tokens before it counts.
        let mut short = vec![7_u32; 63];
        assert_eq!(looping(&short), None);
        short.push(7);
        assert_eq!(looping(&short), Some(1));
        // List items that differ in one token are not a loop.
        let mut list = Vec::new();
        for item in 0..10 {
            list.extend([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, item]);
        }
        assert_eq!(looping(&list), None);
    }

    #[test]
    fn a_call_is_the_same_whatever_its_json_spacing_or_key_order() {
        assert_eq!(
            call_key("fs_read", r#"{"path":"a.rs","line_start":3}"#),
            call_key("fs_read", r#"{ "line_start": 3,  "path": "a.rs" }"#)
        );
        assert_ne!(
            call_key("fs_read", r#"{"path":"a.rs"}"#),
            call_key("fs_read", r#"{"path":"b.rs"}"#)
        );
        assert_eq!(call_key("x", " not json "), call_key("x", "not json"));
    }

    #[test]
    fn an_edit_back_to_an_earlier_revision_is_noted() {
        let mut seen = HashMap::new();
        let path = r#"{"path":"a/lib.rs"}"#;
        let read = "a/lib.rs revision 1111111111111111, 9 lines; lines 1-9:";
        assert_eq!(undone(&mut seen, path, read), None);
        let edit = |r: &str| format!("edited a/lib.rs: revision {r}; lines");
        assert_eq!(undone(&mut seen, path, &edit("2222222222222222")), None);
        assert_eq!(undone(&mut seen, path, &edit("3333333333333333")), None);
        let back = undone(&mut seen, path, &edit("2222222222222222"));
        assert!(back.is_some_and(|n| n.contains("undoing your own edit")));
        // Reading a file again at its current revision is no undo.
        assert_eq!(
            undone(
                &mut seen,
                path,
                "a/lib.rs revision 2222222222222222, 9 lines"
            ),
            None
        );
        assert_eq!(
            undone(
                &mut seen,
                r#"{"path":"b.rs"}"#,
                "edited b.rs: revision 2222222222222222; x"
            ),
            None
        );
    }
}
