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
//!   decider's LoRA adapter merged into the weights, on the CPU in `f32`: the reference.
//! - `gpu` (macOS): the same torso on Apple GPUs, in bfloat16 with the adapter kept apart.
//! - [`head`]: the pointer head, which scores each option by comparing the hidden state
//!   at `<answer>` with the hidden state at the end of that option's line.
//! - [`Decider`]: all of it together.

pub mod config;
#[cfg(target_os = "macos")]
pub mod gpu;
pub mod head;
mod math;
pub mod model;
pub mod prompt;
pub mod tokenizer;

use std::path::Path;
#[cfg(target_os = "macos")]
use std::sync::{Mutex, PoisonError};

use loadngo_inference::system_one::{Answer, Decide, Question, Request};

use crate::{
    config::DeciderConfig,
    head::Head,
    model::{Adapter, LoadError, Model, Session},
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
    #[error("GPU: {0}")]
    Gpu(String),
}

#[cfg(target_os = "macos")]
impl From<gpu::GpuError> for DeciderError {
    fn from(e: gpu::GpuError) -> Self {
        Self::Gpu(e.to_string())
    }
}

/// Where the torso runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Device {
    /// [`model::Model`], in `f32`: the reference, on every platform.
    Cpu,
    /// `gpu::GpuModel` on Apple GPUs (macOS only).
    Gpu,
}

/// Positions the GPU reads per pass.
#[cfg(target_os = "macos")]
const GPU_CHUNK: usize = 512;

enum Torso {
    Cpu(Model),
    #[cfg(target_os = "macos")]
    Gpu {
        model: Box<gpu::GpuModel>,
        /// A request's state session and the copy each question is read from, kept
        /// between requests (about 120 MB each at the decider's window).
        sessions: Mutex<Option<(gpu::GpuSession, gpu::GpuSession)>>,
    },
}

/// A loaded decision model.
pub struct Decider {
    pub config: DeciderConfig,
    tokenizer: Tokenizer,
    torso: Torso,
    head: Head,
}

/// What [`Decider::answer`] needs from a torso: two sessions, one to read a request's
/// state into and one to copy it to for each question, and reads that return the final
/// hidden states of chosen positions.
trait Reader {
    type Session;
    fn begin(&self) -> Result<(Self::Session, Self::Session), DeciderError>;
    fn end(&self, sessions: (Self::Session, Self::Session));
    /// Makes `session` one that has read nothing.
    fn reset(&self, session: &mut Self::Session) -> Result<(), DeciderError>;
    fn copy(&self, from: &Self::Session, into: &mut Self::Session) -> Result<(), DeciderError>;
    /// The final hidden states of `wanted` (indices into `ids`), one after another.
    fn read(
        &self,
        session: &mut Self::Session,
        ids: &[u32],
        wanted: &[usize],
    ) -> Result<Vec<f32>, DeciderError>;
}

impl Reader for Model {
    type Session = Session;

    fn begin(&self) -> Result<(Session, Session), DeciderError> {
        Ok((self.session(), self.session()))
    }

    fn end(&self, _: (Session, Session)) {}

    fn reset(&self, session: &mut Session) -> Result<(), DeciderError> {
        *session = self.session();
        Ok(())
    }

    fn copy(&self, from: &Session, into: &mut Session) -> Result<(), DeciderError> {
        into.clone_from(from);
        Ok(())
    }

    fn read(
        &self,
        session: &mut Session,
        ids: &[u32],
        wanted: &[usize],
    ) -> Result<Vec<f32>, DeciderError> {
        let h = self.config.hidden;
        let states = self.forward(session, ids);
        Ok(wanted
            .iter()
            .flat_map(|&p| states[p * h..(p + 1) * h].iter().copied())
            .collect())
    }
}

#[cfg(target_os = "macos")]
struct GpuReader<'a> {
    model: &'a gpu::GpuModel,
    sessions: &'a Mutex<Option<(gpu::GpuSession, gpu::GpuSession)>>,
}

#[cfg(target_os = "macos")]
impl Reader for GpuReader<'_> {
    type Session = gpu::GpuSession;

    fn begin(&self) -> Result<(gpu::GpuSession, gpu::GpuSession), DeciderError> {
        let kept = self
            .sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        match kept {
            Some(pair) => Ok(pair),
            None => Ok((self.model.session()?, self.model.session()?)),
        }
    }

    fn end(&self, sessions: (gpu::GpuSession, gpu::GpuSession)) {
        *self.sessions.lock().unwrap_or_else(PoisonError::into_inner) = Some(sessions);
    }

    fn reset(&self, session: &mut gpu::GpuSession) -> Result<(), DeciderError> {
        Ok(self.model.reset(session)?)
    }

    fn copy(&self, from: &gpu::GpuSession, into: &mut gpu::GpuSession) -> Result<(), DeciderError> {
        Ok(self.model.copy_session(from, into)?)
    }

    fn read(
        &self,
        session: &mut gpu::GpuSession,
        ids: &[u32],
        wanted: &[usize],
    ) -> Result<Vec<f32>, DeciderError> {
        Ok(self.model.read(session, ids, wanted)?)
    }
}

impl Decider {
    /// Loads a checkpoint (`strands_decider_config.json`, `tokenizer.json`, `head.safetensors`
    /// and the LoRA adapter in `lora/`) over its base model's directory.
    ///
    /// # Errors
    /// When a file is missing or describes something this crate does not implement.
    pub fn load(checkpoint: &Path, base: &Path) -> Result<Self, DeciderError> {
        Self::load_on(checkpoint, base, Device::Cpu)
    }

    /// [`Decider::load`] with the torso on `device`.
    ///
    /// # Errors
    /// As [`Decider::load`]; and for [`Device::Gpu`], when this is not macOS or the GPU
    /// cannot hold the model.
    pub fn load_on(checkpoint: &Path, base: &Path, device: Device) -> Result<Self, DeciderError> {
        let config = DeciderConfig::from_file(&checkpoint.join("strands_decider_config.json"))?;
        let tokenizer = Tokenizer::from_file(&checkpoint.join("tokenizer.json"))?;
        #[allow(clippy::cast_precision_loss)]
        let scale = config.lora_alpha / config.lora_r as f32;
        let model = Model::load(
            base,
            Some(Adapter {
                dir: &checkpoint.join("lora"),
                scale,
                merge: device == Device::Cpu,
            }),
            config.bf16,
        )?;
        let head = Head::load(checkpoint, model.config.hidden, config.pointer_dim)?;
        let torso = match device {
            Device::Cpu => Torso::Cpu(model),
            #[cfg(target_os = "macos")]
            Device::Gpu => Torso::Gpu {
                model: Box::new(gpu::GpuModel::new(model, GPU_CHUNK, config.max_length)?),
                sessions: Mutex::new(None),
            },
            #[cfg(not(target_os = "macos"))]
            Device::Gpu => return Err(DeciderError::Gpu("the GPU path runs on macOS only".into())),
        };
        Ok(Self {
            config,
            tokenizer,
            torso,
            head,
        })
    }

    /// Where the torso runs: "CPU", or the GPU's name.
    #[must_use]
    pub fn device(&self) -> String {
        match &self.torso {
            Torso::Cpu(_) => "CPU".to_owned(),
            #[cfg(target_os = "macos")]
            Torso::Gpu { model, .. } => model.device(),
        }
    }

    /// The GPU's pass time since the last call (none on the CPU).
    #[cfg(target_os = "macos")]
    #[must_use]
    pub fn take_gpu_time(&self) -> Option<gpu::PassTime> {
        match &self.torso {
            Torso::Gpu { model, .. } => Some(model.take_time()),
            Torso::Cpu(_) => None,
        }
    }

    /// Answers every question of `request`: for each, a probability per option in
    /// [`Question::options`] order. The state is read once per run of questions; each
    /// question is read from a copy of that.
    ///
    /// # Errors
    /// When a question is too long to fit the window with its options.
    pub fn answer(&self, request: &Request) -> Result<Vec<(String, Answer)>, DeciderError> {
        match &self.torso {
            Torso::Cpu(model) => self.answer_with(model, request),
            #[cfg(target_os = "macos")]
            Torso::Gpu { model, sessions } => {
                self.answer_with(&GpuReader { model, sessions }, request)
            }
        }
    }

    fn answer_with<R: Reader>(
        &self,
        torso: &R,
        request: &Request,
    ) -> Result<Vec<(String, Answer)>, DeciderError> {
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
        let mut answers = Vec::with_capacity(request.questions.len());
        let (mut state, mut work) = torso.begin()?;
        let result = self.read_runs(
            torso,
            &runs,
            &rendered,
            request,
            (&mut state, &mut work),
            &mut answers,
        );
        torso.end((state, work));
        result?;
        Ok(answers)
    }

    /// Reads each run's state into `state`, then each of its questions from a copy in
    /// `work`, and scores the options.
    fn read_runs<R: Reader>(
        &self,
        torso: &R,
        runs: &[prompt::Fitted],
        rendered: &[prompt::Rendered],
        request: &Request,
        (state, work): (&mut R::Session, &mut R::Session),
        answers: &mut Vec<(String, Answer)>,
    ) -> Result<(), DeciderError> {
        let hidden = self.head.hidden();
        for run in runs {
            torso.reset(state)?;
            torso.read(state, &run.state, &[])?;
            for (k, index) in run.questions.clone().enumerate() {
                torso.copy(state, work)?;
                let last = run.question_ids[k].len() - 1;
                let mut wanted = run.options[k].clone();
                wanted.push(last);
                let states = torso.read(work, &run.question_ids[k], &wanted)?;
                let row = |i: usize| &states[i * hidden..(i + 1) * hidden];
                let options: Vec<&[f32]> = (0..run.options[k].len()).map(row).collect();
                let mut probabilities = self.head.logits(row(run.options[k].len()), &options);
                let temperature = self.config.temperature(rendered[index].kind.name());
                for p in &mut probabilities {
                    *p /= temperature;
                }
                math::softmax(&mut probabilities);
                let (id, question) = &request.questions[index];
                answers.push((id.clone(), to_answer(question, &probabilities)));
            }
        }
        Ok(())
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
