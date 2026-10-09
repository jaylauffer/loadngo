//! A decision model in the loadngo stack: Strands Decider (Apache-2.0,
//! `strands-labs/strands-decider`), written from its published architecture and checked
//! against its reference implementation (`scripts/decider_fixtures.py`).
//!
//! The model reads a state and typed questions (the System One request of
//! [`loadngo_inference::system_one`]) and gives a probability for every option of every
//! question from one forward pass, with nothing generated:
//!
//! - [`tokenizer`]: the checkpoint's byte-level BPE, with character offsets.
//! - [`prompt`]: the request rendered as the model was trained to read it, and fitted to
//!   its window.
//! - [`model`]: the Qwen3.5 torso (Gated DeltaNet and gated attention layers), with the
//!   decider's LoRA adapter merged into the weights.
//! - [`head`]: the pointer head, which scores each option by comparing the hidden state
//!   at `<answer>` with the hidden state at the end of that option's line.
//! - [`Decider`]: all of it together.

pub mod config;
pub mod head;
mod math;
pub mod model;
pub mod prompt;
pub mod tokenizer;

use std::path::Path;

use loadngo_inference::system_one::{Answer, Decide, Question, Request};

use crate::{
    config::DeciderConfig,
    head::Head,
    model::{Adapter, LoadError, Model},
    prompt::{fit, render_question, render_state, FitError},
    tokenizer::{Tokenizer, TokenizerError},
};

#[derive(Debug, thiserror::Error)]
pub enum DeciderError {
    #[error("{0}")]
    Load(#[from] LoadError),
    #[error("{0}")]
    Tokenizer(#[from] TokenizerError),
    #[error("{0}")]
    Config(#[from] config::ConfigError),
    #[error("{0}")]
    Fit(#[from] FitError),
}

/// A loaded decision model.
pub struct Decider {
    pub config: DeciderConfig,
    tokenizer: Tokenizer,
    model: Model,
    head: Head,
}

impl Decider {
    /// Loads a checkpoint (`strands_decider_config.json`, `tokenizer.json`, `head.safetensors`
    /// and the LoRA adapter in `lora/`) over its base model's directory.
    ///
    /// # Errors
    /// When a file is missing or describes something this crate does not implement.
    pub fn load(checkpoint: &Path, base: &Path) -> Result<Self, DeciderError> {
        let config = DeciderConfig::from_file(&checkpoint.join("strands_decider_config.json"))?;
        let tokenizer = Tokenizer::from_file(&checkpoint.join("tokenizer.json"))?;
        #[allow(clippy::cast_precision_loss)]
        let scale = config.lora_alpha / config.lora_r as f32;
        let model = Model::load(
            base,
            Some(Adapter {
                dir: &checkpoint.join("lora"),
                scale,
            }),
            config.bf16,
        )?;
        let head = Head::load(checkpoint, model.config.hidden, config.pointer_dim)?;
        Ok(Self {
            config,
            tokenizer,
            model,
            head,
        })
    }

    /// Answers every question of `request`: for each, a probability per option in
    /// [`Question::options`] order. The state is read once per run of questions; each
    /// question is read from a copy of that.
    ///
    /// # Errors
    /// When a question is too long to fit the window with its options.
    pub fn answer(&self, request: &Request) -> Result<Vec<(String, Answer)>, DeciderError> {
        let rendered: Vec<_> = request
            .questions
            .iter()
            .map(|(_, q)| render_question(q))
            .collect();
        let runs = fit(
            &self.tokenizer,
            &render_state(&request.state),
            &rendered,
            self.config.max_length,
        )?;
        let hidden = self.model.config.hidden;
        let mut answers = Vec::with_capacity(request.questions.len());
        for run in runs {
            let mut state = self.model.session();
            self.model.forward(&mut state, &run.state);
            for (k, index) in run.questions.clone().enumerate() {
                let states = self.model.forward(&mut state.clone(), &run.question_ids[k]);
                let row = |p: usize| &states[p * hidden..(p + 1) * hidden];
                let options: Vec<&[f32]> = run.options[k].iter().map(|&p| row(p)).collect();
                let last = run.question_ids[k].len() - 1;
                let mut probabilities = self.head.logits(row(last), &options);
                let temperature = self.config.temperature(rendered[index].kind.name());
                for p in &mut probabilities {
                    *p /= temperature;
                }
                math::softmax(&mut probabilities);
                let (id, question) = &request.questions[index];
                answers.push((id.clone(), to_answer(question, &probabilities)));
            }
        }
        Ok(answers)
    }
}

/// The rendered options' probabilities as an [`Answer`] in [`Question::options`] order: a
/// noul is rendered `false, true` and answered `true, false`.
fn to_answer(question: &Question, rendered: &[f32]) -> Answer {
    let probabilities = match question {
        Question::Noul { .. } => vec![
            ("true".to_owned(), rendered[1]),
            ("false".to_owned(), rendered[0]),
        ],
        Question::Choice { criteria, .. } | Question::Score { criteria, .. } => criteria
            .iter()
            .zip(rendered)
            .map(|((label, _), p)| (label.clone(), *p))
            .collect(),
    };
    Answer { probabilities }
}

impl Decide for Decider {
    fn decide(&mut self, request: &Request) -> Result<Vec<(String, Answer)>, String> {
        self.answer(request).map_err(|e| e.to_string())
    }
}
