//! Context flow: when a conversation nears the end of the model's context, the model
//! writes a handoff to itself and the context is rebuilt from it, and the turn goes on.
//!
//! Written from the behaviour Kimi's chat had since 2026-10-02 (kimi `docs/CHAT.md`,
//! "Context flow"), for every model on the loop:
//!
//! - past three quarters of the context, the model is asked (as a message from the chat,
//!   not from Jay) for a handoff in fixed sections, its reply begun for it with `TASK:`
//!   and limited to 1/32 of the context;
//! - the new context is the opening, a note holding the handoff and Jay's earlier
//!   messages word for word (the newest 2 KiB), Jay's message of this turn, and the
//!   newest tool rounds of this turn that fit an eighth of the context, results included;
//! - it happens before results that would cross the line, when a reply runs into the
//!   end of the context (the cut reply is dropped and written again), and before a new
//!   message that would cross it. Another waits until the context has grown by an
//!   eighth since the last, so a context that cannot be made smaller stops as before.
//!
//! What survives is what the program copies (Jay's words, the newest rounds) and what
//! the handoff says; everything else is gone.

use super::{Call, Exchange};

/// How the handoff reply is begun for the model: asked in a message alone, Kimi
/// answered with one more tool call (2026-10-02).
pub const HANDOFF_OPENING: &str = "TASK:";

/// Bytes of Jay's earlier messages kept word for word.
const EARLIER_BYTES: usize = 2048;

/// The newest calls of this turn listed in the rebuilt context, one line each.
pub const WORK_LINES: usize = 24;

/// The request for a handoff.
pub const REQUEST: &str = "The context is nearly full, so the conversation will be cleared \
and rebuilt from a handoff you write to yourself now. Write it in these sections, plainly and \
briefly, with no tool calls: TASK (what Jay asked), STANDING (instructions and constraints that \
still apply), DONE (what you did, with paths), FACTS (what you found, with paths and line \
numbers), FAILED (what did not work, and why), FILES CHANGED, NEXT (the next step). Only what \
you write here, Jay's messages and your newest tool results will remain.";

/// A tool round of the current turn: the reply that made the calls (as fed, without the
/// token that ended it), that token, and the results.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Round {
    pub reply: Vec<u32>,
    pub ended_by: Option<u32>,
    pub results: Vec<(Call, String)>,
}

/// Whether adding `adding` positions at `position` should first compact: past three
/// quarters of `capacity`, and grown by an eighth since the last compaction (which left
/// the context at `compacted`).
#[must_use]
pub fn due(position: usize, adding: usize, capacity: usize, compacted: usize) -> bool {
    position + adding > capacity / 4 * 3 && position >= compacted + capacity / 8
}

/// The handoff's length limit, in tokens.
#[must_use]
pub fn handoff_limit(capacity: usize) -> usize {
    (capacity / 32).max(64)
}

/// Jay's earlier messages, newest last, cut to the newest `EARLIER_BYTES` (the oldest
/// one kept may lose its start).
#[must_use]
pub fn earlier_messages(history: &[Exchange]) -> String {
    let mut kept: Vec<String> = Vec::new();
    let mut used = 0;
    for e in history.iter().rev() {
        if used + e.user.len() <= EARLIER_BYTES {
            used += e.user.len();
            kept.push(e.user.clone());
            continue;
        }
        let room = EARLIER_BYTES - used;
        let mut at = e.user.len() - room;
        while !e.user.is_char_boundary(at) {
            at += 1;
        }
        if at < e.user.len() {
            kept.push(format!("…{}", &e.user[at..]));
        }
        break;
    }
    kept.reverse();
    kept.iter()
        .map(|m| format!("- {m}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The note the rebuilt context opens the turn with. `work` lists this turn's calls,
/// one line each with the start of its result, so a model whose results did not fit can
/// see what it already did.
#[must_use]
pub fn note(handoff: &str, earlier: &str, work: &[String]) -> String {
    let mut note = format!(
        "The earlier conversation was cleared to free context. Before that you wrote this \
         handoff:\n\n{}\n",
        handoff.trim()
    );
    if !earlier.is_empty() {
        note.push_str(&format!(
            "\nJay's earlier messages in this conversation, word for word:\n{earlier}\n"
        ));
    }
    if !work.is_empty() {
        note.push_str(&format!(
            "\nCalls you made in this turn before the handoff (their full results are gone; \
             repeat one only if you need more than its line shows):\n{}\n",
            work.join("\n")
        ));
    }
    note.push_str(
        "\nGo on with Jay's message below from the handoff and the tool results after it; do \
         not redo finished steps.",
    );
    note
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compaction_is_due_past_three_quarters_after_growing_an_eighth() {
        assert!(!due(700, 50, 1000, 0));
        assert!(due(700, 51, 1000, 0));
        // Just compacted to 700: not again until 825.
        assert!(!due(800, 100, 1000, 700));
        assert!(due(825, 100, 1000, 700));
        assert_eq!(handoff_limit(32_768), 1024);
        assert_eq!(handoff_limit(100), 64);
    }

    #[test]
    fn earlier_messages_keep_the_newest_2_kib() {
        let history: Vec<Exchange> = (0..10)
            .map(|i| Exchange {
                user: format!("{i}{}", "x".repeat(499)),
                answer: String::new(),
            })
            .collect();
        let kept = earlier_messages(&history);
        // Four whole messages and the end of the fifth.
        assert_eq!(kept.lines().count(), 5);
        assert!(kept.starts_with("- …x") && kept.contains("- 6") && kept.contains("- 9"));
        let one = [Exchange {
            user: "é".repeat(3_000),
            answer: String::new(),
        }];
        assert!(earlier_messages(&one).len() <= EARLIER_BYTES + 10);
        let n = note("TASK: fix it", &kept, &["1. fs_read {} -> fn main".into()]);
        assert!(n.contains("TASK: fix it") && n.contains("word for word"));
        assert!(n.contains("1. fs_read {} -> fn main"));
        assert!(!note("TASK: x", "", &[]).contains("word for word"));
        assert!(!note("TASK: x", "", &[]).contains("Calls you made"));
    }
}
