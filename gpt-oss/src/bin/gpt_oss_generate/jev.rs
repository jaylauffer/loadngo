//! System One (Jev) on gpt-oss: the model's next-token scores for option letters, read
//! in the engine's side session. The questions themselves, and what the chat does with
//! the answers, are the shared loop's (`loadngo_inference::agent::jev`).

use std::collections::HashMap;

use loadngo_gpt_oss::{
    chat::{judge_question, judge_state},
    tokenizer::Tokenizer,
};
use loadngo_inference::system_one::LabelModel;

use crate::engine::{Engine, Which};

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
