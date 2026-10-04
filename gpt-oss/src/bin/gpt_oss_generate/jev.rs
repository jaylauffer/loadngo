//! System One (Jev) on gpt-oss: typed questions about the work, answered with a
//! probability per option from the model's next-token scores (loadngo
//! `inference::system_one`), in the engine's side session.
//!
//! Two uses:
//!
//! - **Checkpoints.** Every few tool calls: is the turn in progress, waiting for Jay,
//!   complete, or stuck; and are the latest calls repeating earlier ones. The tool loop
//!   acts on confident answers (see `agent`).
//! - **The web gate.** Before a web search or fetch, when no local tool has been tried
//!   this turn: is the request about this machine, Jay's projects, files, archives or
//!   notes? When it probably is, the model is sent to the local tools first.

use std::collections::HashMap;
use std::time::Instant;

use loadngo_gpt_oss::{
    chat::{judge_question, judge_state},
    tokenizer::Tokenizer,
};
use loadngo_inference::system_one::{answer, Answer, Calibration, LabelModel, Question, Request};

use crate::engine::{Engine, Which};

/// The longest state Jev reads, in characters (about 1,500 tokens).
const MAX_STATE_CHARS: usize = 6_000;

/// gpt-oss as a [`LabelModel`]: the state is read once into the side session, then each
/// question is read after it and the side session goes back to the state's end.
pub struct Judge<'a> {
    engine: &'a mut Engine,
    tokenizer: &'a Tokenizer,
    date: String,
    /// The state read, and the side session's position after it.
    cached: Option<(String, usize)>,
    letters: HashMap<String, u32>,
}

impl<'a> Judge<'a> {
    pub fn new(engine: &'a mut Engine, tokenizer: &'a Tokenizer, date: &str) -> Self {
        Self {
            engine,
            tokenizer,
            date: date.to_owned(),
            cached: None,
            letters: HashMap::new(),
        }
    }

    fn letter(&mut self, label: &str) -> Result<u32, String> {
        if let Some(&id) = self.letters.get(label) {
            return Ok(id);
        }
        match self.tokenizer.encode(label)[..] {
            [id] => {
                self.letters.insert(label.to_owned(), id);
                Ok(id)
            }
            _ => Err(format!("option label {label:?} is not one token")),
        }
    }
}

impl LabelModel for Judge<'_> {
    fn label_logits(
        &mut self,
        state: &str,
        question: &str,
        labels: &[String],
    ) -> Result<Vec<f32>, String> {
        let ids = labels
            .iter()
            .map(|l| self.letter(l))
            .collect::<Result<Vec<_>, _>>()?;
        match &self.cached {
            Some((read, end)) if read == state => self.engine.truncate(Which::Side, *end)?,
            _ => {
                self.engine.reset(Which::Side)?;
                let prefix =
                    judge_state(self.tokenizer, &self.date, state).map_err(|e| e.to_string())?;
                self.engine.feed(Which::Side, &prefix)?;
                self.cached = Some((state.to_owned(), self.engine.position(Which::Side)));
            }
        }
        let suffix = judge_question(self.tokenizer, question).map_err(|e| e.to_string())?;
        let logits = self.engine.feed(Which::Side, &suffix)?;
        Ok(ids.iter().map(|&id| logits[id as usize]).collect())
    }
}

/// `text` cut to at most `n` characters, keeping its end (the newest part).
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
    pub fn p(&self, label: &str) -> f32 {
        self.state
            .probabilities
            .iter()
            .find(|(l, _)| l == label)
            .map_or(0.0, |(_, p)| *p)
    }

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
pub fn checkpoint(
    judge: &mut Judge<'_>,
    request: &str,
    work: &[String],
) -> Result<Checkpoint, String> {
    let started = Instant::now();
    let state = newest(
        &format!(
            "Jay's request to the assistant:\n{request}\n\nThe assistant's tool calls so far, oldest first:\n{}",
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
    let answers = answer(judge, &Request { state, questions }, Calibration::default())?;
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
pub fn local_first(judge: &mut Judge<'_>, request: &str, intent: &str) -> Result<f32, String> {
    let state = newest(
        &format!(
            "Jay's request to an assistant running on his Mac mini, which holds his workspace of \
             projects, his archives and the assistant's notes:\n{request}\n\nThe assistant now wants to {intent}."
        ),
        MAX_STATE_CHARS,
    );
    let questions = vec![(
        "local".to_owned(),
        Question::Noul {
            instructions: "The request is about Jay's own projects, files, code, archives, notes or this \
                           machine, or about something already stated above, so the answer is more likely \
                           on this machine than on the web."
                .into(),
        },
    )];
    let answers = answer(judge, &Request { state, questions }, Calibration::default())?;
    Ok(answers[0].1.probabilities.first().map_or(0.0, |(_, p)| *p))
}
