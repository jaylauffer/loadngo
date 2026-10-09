//! System One (Jev) in the tool loop: typed questions about the work, answered with a
//! probability per option by a [`Decide`]: the chat's own model read through its option
//! letters in a side session ([`crate::system_one::LetterReadout`]), so judging never
//! disturbs the conversation, or a decision model.
//!
//! - **Checkpoints.** Every few tool calls: is the turn in progress, waiting for Jay,
//!   complete, or stuck; and are the latest calls repeating earlier ones. The agent acts
//!   on confident answers (see [`super::Agent`]).
//! - **The web gate.** Before the first approved web call in a turn: is useful local
//!   lookup still missing, given the actual calls? A denied call is judged again after
//!   more work; one unrelated local call does not bypass the gate.

use std::time::Instant;

use crate::system_one::{Answer, Decide, Question, Request};

/// The longest state Jev reads, in characters (about 1,500 tokens).
const MAX_STATE_CHARS: usize = 6_000;

/// `text` cut to at most `n` characters, keeping its end (the newest part).
#[must_use]
pub fn newest(text: &str, n: usize) -> String {
    let count = text.chars().count();
    if count <= n {
        return text.to_owned();
    }
    format!("[…]{}", text.chars().skip(count - n).collect::<String>())
}

/// Where a turn stands, as Jev judges it.
pub struct Checkpoint {
    /// `in-progress`, `needs-input`, `complete` or `stuck`, with probabilities.
    pub state: Answer,
    /// Probability that the latest tool calls repeat earlier ones.
    pub repeating: f32,
    pub seconds: f64,
}

impl Checkpoint {
    #[must_use]
    pub fn p(&self, label: &str) -> f32 {
        self.state
            .probabilities
            .iter()
            .find(|(l, _)| l == label)
            .map_or(0.0, |(_, p)| *p)
    }

    #[must_use]
    pub fn summary(&self) -> String {
        let (best, p) = self.state.best();
        format!(
            "state {best} {p:.2} (stuck {:.2}), repeating {:.2}, {:.1}s",
            self.p("stuck"),
            self.repeating,
            self.seconds
        )
    }
}

/// Asks where the turn stands. `request` is Jay's message; `work` the turn's tool calls
/// so far, oldest first, each one line.
///
/// # Errors
/// When the label model fails.
pub fn checkpoint(
    judge: &mut dyn Decide,
    request: &str,
    work: &[String],
) -> Result<Checkpoint, String> {
    let started = Instant::now();
    let state = newest(
        &format!(
            "Jay's request to the assistant:\n{request}\n\nThe assistant's tool calls so far, \
             oldest first:\n{}",
            work.join("\n")
        ),
        MAX_STATE_CHARS,
    );
    let questions = vec![
        (
            "state".to_owned(),
            Question::Choice {
                instructions: Some(
                    "Where does the assistant's work on Jay's request stand?".into(),
                ),
                criteria: vec![
                    (
                        "in-progress".into(),
                        "in progress: each step is getting closer to what Jay asked for".into(),
                    ),
                    (
                        "needs-input".into(),
                        "it needs Jay to decide something or supply something before it can go on"
                            .into(),
                    ),
                    (
                        "complete".into(),
                        "the request is done; all that is left is to answer".into(),
                    ),
                    (
                        "stuck".into(),
                        "stuck: it is going in circles or blocked, not getting closer".into(),
                    ),
                ],
            },
        ),
        (
            "repeating".to_owned(),
            Question::Noul {
                instructions:
                    "The latest tool calls repeat earlier calls without finding anything new."
                        .into(),
            },
        ),
    ];
    let answers = judge.decide(&Request { state, questions })?;
    let state = answers[0].1.clone();
    let repeating = answers[1].1.probabilities.first().map_or(0.0, |(_, p)| *p);
    Ok(Checkpoint {
        state,
        repeating,
        seconds: started.elapsed().as_secs_f64(),
    })
}

/// The probability that `request` can be answered from this machine, asked before the
/// model goes to the web to `intent` (for example `search the web for "…"`).
///
/// # Errors
/// When the label model fails.
pub fn local_first(
    judge: &mut dyn Decide,
    request: &str,
    intent: &str,
    evidence: &str,
) -> Result<f32, String> {
    let state = format!(
        "Jay's request to an assistant running on his Mac mini, which holds his workspace of \
         projects, his archives and the assistant's notes:\n{request}\n\nThe assistant now \
         wants to {intent}.\n\nActual tool receipts (including earlier turns):\n{}",
        newest(evidence, 4_000),
        request = newest(request, 1_000),
        intent = newest(intent, 500),
    );
    let questions = vec![(
        "local".to_owned(),
        Question::Noul {
            instructions: "Useful local lookup is still missing before this web call. For an \
                           unfamiliar person or name, check workspace contents, archive contents \
                           and notes first. One local call, an error, or a cas_find filename \
                           search does not cover those sources. cas_grep searches archive \
                           contents. If relevant local sources were already checked, the request \
                           is clearly external, or Jay explicitly asked for the web, answer no. \
                           Otherwise answer yes."
                .into(),
        },
    )];
    let answers = judge.decide(&Request { state, questions })?;
    Ok(answers[0].1.probabilities.first().map_or(0.0, |(_, p)| *p))
}
