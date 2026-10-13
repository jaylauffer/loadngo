//! The CPU reference or the GPU path behind one interface, with two sessions: the
//! conversation, and a short side session for Jev's questions, so judging never
//! disturbs the conversation's cache.

use std::time::Instant;

use crate::chat::{read_reply, Conversation, Reply};
use crate::model::{Model, Session};
use crate::tokenizer::Tokenizer;

/// Positions a GPU side session holds: a Jev state and its questions.
#[cfg(target_os = "macos")]
const SIDE_CAPACITY: usize = 4096;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Which {
    Main,
    Side,
}

/// What [`Engine::answer`] produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answered {
    pub reply: Reply,
    /// Tokens generated.
    pub tokens: usize,
    /// The deadline passed before the reply was finished.
    pub expired: bool,
}

pub enum Engine {
    Cpu {
        model: Box<Model>,
        main: Session,
        side: Option<Session>,
    },
    #[cfg(target_os = "macos")]
    Gpu {
        gpu: Box<crate::gpu::GpuModel>,
        main: crate::gpu::GpuSession,
        side: Option<crate::gpu::GpuSession>,
    },
}

impl Engine {
    pub fn new(model: Model, on_gpu: bool, context: usize) -> Result<Self, String> {
        if on_gpu {
            #[cfg(target_os = "macos")]
            {
                let gpu =
                    crate::gpu::GpuModel::new(model, 512, context).map_err(|e| e.to_string())?;
                let main = gpu.session().map_err(|e| e.to_string())?;
                return Ok(Self::Gpu {
                    gpu: Box::new(gpu),
                    main,
                    side: None,
                });
            }
            #[cfg(not(target_os = "macos"))]
            {
                let _ = context;
                return Err("--gpu needs macOS".into());
            }
        }
        let main = model.session();
        Ok(Self::Cpu {
            model: Box::new(model),
            main,
            side: None,
        })
    }

    pub fn name(&self) -> String {
        match self {
            Self::Cpu { .. } => "CPU".into(),
            #[cfg(target_os = "macos")]
            Self::Gpu { gpu, .. } => format!("GPU: {}", gpu.device()),
        }
    }

    /// The positions the conversation can reach.
    pub fn context(&self) -> usize {
        match self {
            Self::Cpu { model, .. } => model.config.context_length,
            #[cfg(target_os = "macos")]
            Self::Gpu { main, .. } => main.capacity(),
        }
    }

    /// Positions fed so far.
    pub fn position(&self, which: Which) -> usize {
        match (self, which) {
            (Self::Cpu { main, .. }, Which::Main) => main.len(),
            (Self::Cpu { side, .. }, Which::Side) => side.as_ref().map_or(0, Session::len),
            #[cfg(target_os = "macos")]
            (Self::Gpu { main, .. }, Which::Main) => main.len(),
            #[cfg(target_os = "macos")]
            (Self::Gpu { side, .. }, Which::Side) => side.as_ref().map_or(0, |s| s.len()),
        }
    }

    /// Starts a session over (the side session is made on first use).
    pub fn reset(&mut self, which: Which) -> Result<(), String> {
        match (self, which) {
            (Self::Cpu { model, main, .. }, Which::Main) => *main = model.session(),
            (Self::Cpu { model, side, .. }, Which::Side) => *side = Some(model.session()),
            #[cfg(target_os = "macos")]
            (Self::Gpu { main, .. }, Which::Main) => main.reset(),
            #[cfg(target_os = "macos")]
            (Self::Gpu { gpu, side, .. }, Which::Side) => match side {
                Some(side) => side.reset(),
                None => {
                    *side = Some(
                        gpu.session_holding(SIDE_CAPACITY)
                            .map_err(|e| e.to_string())?,
                    )
                }
            },
        }
        Ok(())
    }

    /// Goes back to position `len` in a session.
    pub fn truncate(&mut self, which: Which, len: usize) -> Result<(), String> {
        match (self, which) {
            (Self::Cpu { main, .. }, Which::Main) => main.truncate(len),
            (Self::Cpu { side, .. }, Which::Side) => {
                side.as_mut().ok_or("no side session")?.truncate(len)
            }
            #[cfg(target_os = "macos")]
            (Self::Gpu { main, .. }, Which::Main) => {
                main.truncate(len).map_err(|e| e.to_string())?
            }
            #[cfg(target_os = "macos")]
            (Self::Gpu { side, .. }, Which::Side) => {
                side.as_mut()
                    .ok_or("no side session")?
                    .truncate(len)
                    .map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }

    /// The GPU profile since the last call (nothing on the CPU path).
    pub fn profile(&self) -> String {
        match self {
            Self::Cpu { .. } => "no profile on the CPU path".into(),
            #[cfg(target_os = "macos")]
            Self::Gpu { gpu, .. } => gpu.take_profile().to_string(),
        }
    }

    /// Feeds tokens to a session and returns the logits after the last.
    pub fn feed(&mut self, which: Which, tokens: &[u32]) -> Result<Vec<f32>, String> {
        if which == Which::Side && self.position(Which::Side) == 0 && !self.has_side() {
            self.reset(Which::Side)?;
        }
        match self {
            Self::Cpu { model, main, side } => {
                let session = match which {
                    Which::Main => main,
                    Which::Side => side.as_mut().expect("made above"),
                };
                let mut logits = Vec::new();
                for &t in tokens {
                    logits = model.step(session, t);
                }
                Ok(logits)
            }
            #[cfg(target_os = "macos")]
            Self::Gpu { gpu, main, side } => {
                let session = match which {
                    Which::Main => main,
                    Which::Side => side.as_mut().expect("made above"),
                };
                gpu.feed(session, tokens, crate::gpu::Logits::Last)
                    .map_err(|e| e.to_string())?
                    .pop()
                    .ok_or_else(|| "no logits".to_owned())
            }
        }
    }

    /// Answers `conversation` from a fresh conversation session: greedy decoding until
    /// the format ends the reply (`<|return|>`, or `<|call|>`), `limit` tokens, the end
    /// of the context, or `deadline`, which is checked before each token.
    ///
    /// # Errors
    /// When the prompt does not fit the context or the engine fails.
    pub fn answer(
        &mut self,
        tokenizer: &Tokenizer,
        conversation: &Conversation,
        limit: usize,
        deadline: Instant,
    ) -> Result<Answered, String> {
        let prompt = conversation.prompt(tokenizer).map_err(|e| e.to_string())?;
        if prompt.len() + 1 >= self.context() {
            return Err(format!(
                "the prompt is {} tokens; the context holds {}",
                prompt.len(),
                self.context()
            ));
        }
        let stops: Vec<u32> = ["<|return|>", "<|call|>"]
            .iter()
            .filter_map(|name| tokenizer.control(name))
            .collect();
        self.reset(Which::Main)?;
        let mut logits = self.feed(Which::Main, &prompt)?;
        let mut out = Vec::new();
        let mut expired = false;
        while out.len() < limit && self.position(Which::Main) + 1 < self.context() {
            if Instant::now() >= deadline {
                expired = true;
                break;
            }
            let next = logits
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.total_cmp(b.1))
                .map(|(i, _)| i as u32)
                .ok_or("no logits")?;
            out.push(next);
            if stops.contains(&next) {
                break;
            }
            logits = self.feed(Which::Main, &[next])?;
        }
        Ok(Answered {
            reply: read_reply(tokenizer, &out),
            tokens: out.len(),
            expired,
        })
    }

    fn has_side(&self) -> bool {
        match self {
            Self::Cpu { side, .. } => side.is_some(),
            #[cfg(target_os = "macos")]
            Self::Gpu { side, .. } => side.is_some(),
        }
    }
}
