//! gpt-oss's chat format, harmony, without tools.
//!
//! A conversation is a sequence of messages, each
//! `<|start|>{role}<|message|>{text}<|end|>`:
//!
//! - **system**, always first:
//!
//!   ```text
//!   {identity}
//!   Knowledge cutoff: 2024-06
//!   Current date: {YYYY-MM-DD}
//!
//!   Reasoning: {low|medium|high}
//!
//!   # Valid channels: analysis, commentary, final. Channel must be included for every message.
//!   ```
//!
//! - **developer**, when there are instructions: `# Instructions\n\n{instructions}\n\n`;
//! - **user** turns;
//! - **earlier assistant** replies, final channel only:
//!   `<|start|>assistant<|channel|>final<|message|>{text}<|end|>`. Their reasoning is not
//!   shown again.
//!
//! The prompt ends `<|start|>assistant`. The model then writes its reasoning
//! (`<|channel|>analysis<|message|>…<|end|>`), then
//! `<|start|>assistant<|channel|>final<|message|>…` and `<|return|>`.
//!
//! This follows the chat template shipped with the model, from which these strings are
//! taken. Text goes through [`Tokenizer::encode`], so a message that spells a control
//! token cannot forge one.

use crate::tokenizer::Tokenizer;

pub const DEFAULT_IDENTITY: &str = "You are ChatGPT, a large language model trained by OpenAI.";
const KNOWLEDGE_CUTOFF: &str = "2024-06";
const CHANNELS: &str =
    "# Valid channels: analysis, commentary, final. Channel must be included for every message.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reasoning {
    Low,
    Medium,
    High,
}

impl Reasoning {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    User(String),
    /// An earlier reply: its final channel.
    Assistant(String),
}

#[derive(Clone, Debug)]
pub struct Conversation {
    pub identity: String,
    /// `YYYY-MM-DD`.
    pub date: String,
    pub reasoning: Reasoning,
    pub instructions: Option<String>,
    pub messages: Vec<Message>,
}

#[derive(Debug, thiserror::Error)]
#[error("the tokenizer has no control token {0}")]
pub struct MissingControl(&'static str);

/// The control tokens the format uses.
struct Controls {
    start: u32,
    message: u32,
    end: u32,
    channel: u32,
}

impl Controls {
    fn of(tokenizer: &Tokenizer) -> Result<Self, MissingControl> {
        let get = |name: &'static str| tokenizer.control(name).ok_or(MissingControl(name));
        Ok(Self {
            start: get("<|start|>")?,
            message: get("<|message|>")?,
            end: get("<|end|>")?,
            channel: get("<|channel|>")?,
        })
    }
}

impl Conversation {
    pub fn new(date: impl Into<String>) -> Self {
        Self {
            identity: DEFAULT_IDENTITY.to_owned(),
            date: date.into(),
            reasoning: Reasoning::Medium,
            instructions: None,
            messages: Vec::new(),
        }
    }

    /// The tokens up to and including `<|start|>assistant`, ready for a reply.
    pub fn prompt(&self, tokenizer: &Tokenizer) -> Result<Vec<u32>, MissingControl> {
        let c = Controls::of(tokenizer)?;
        let mut out = Vec::new();
        let message = |out: &mut Vec<u32>, role: &str, channel: Option<&str>, text: &str| {
            out.push(c.start);
            out.extend(tokenizer.encode(role));
            if let Some(channel) = channel {
                out.push(c.channel);
                out.extend(tokenizer.encode(channel));
            }
            out.push(c.message);
            out.extend(tokenizer.encode(text));
            out.push(c.end);
        };
        let system = format!(
            "{}\nKnowledge cutoff: {KNOWLEDGE_CUTOFF}\nCurrent date: {}\n\nReasoning: {}\n\n{CHANNELS}",
            self.identity,
            self.date,
            self.reasoning.name()
        );
        message(&mut out, "system", None, &system);
        if let Some(instructions) = &self.instructions {
            message(
                &mut out,
                "developer",
                None,
                &format!("# Instructions\n\n{instructions}\n\n"),
            );
        }
        for m in &self.messages {
            match m {
                Message::User(text) => message(&mut out, "user", None, text),
                Message::Assistant(text) => message(&mut out, "assistant", Some("final"), text),
            }
        }
        out.push(c.start);
        out.extend(tokenizer.encode("assistant"));
        Ok(out)
    }
}

/// A reply read back from the tokens the model wrote after the prompt.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reply {
    /// The analysis channel: the model's reasoning, not meant for the user.
    pub analysis: String,
    /// The final channel: the answer.
    pub answer: String,
    /// Whether the model finished (`<|return|>`) rather than running out of tokens.
    pub complete: bool,
}

/// Splits generated tokens into channels. Text before any channel header, and channels
/// other than analysis and final, are left out.
pub fn read_reply(tokenizer: &Tokenizer, tokens: &[u32]) -> Reply {
    let control = |name: &str| tokenizer.control(name);
    let (channel, message, ret) = (
        control("<|channel|>"),
        control("<|message|>"),
        control("<|return|>"),
    );
    let mut reply = Reply::default();
    let mut header: Vec<u32> = Vec::new();
    let mut body: Vec<u32> = Vec::new();
    let mut in_header = false;
    let mut current: Option<String> = None;
    let flush = |current: &Option<String>, body: &mut Vec<u32>, reply: &mut Reply| {
        let text = tokenizer.decode(body);
        match current.as_deref() {
            Some("analysis") => reply.analysis.push_str(&text),
            Some("final") => reply.answer.push_str(&text),
            _ => {}
        }
        body.clear();
    };
    for &t in tokens {
        if Some(t) == channel {
            flush(&current, &mut body, &mut reply);
            in_header = true;
            header.clear();
        } else if Some(t) == message && in_header {
            current = Some(tokenizer.decode(&header).trim().to_owned());
            in_header = false;
        } else if tokenizer.is_control(t) {
            flush(&current, &mut body, &mut reply);
            if Some(t) == ret {
                reply.complete = true;
                break;
            }
            current = None;
        } else if in_header {
            header.push(t);
        } else if current.is_some() {
            body.push(t);
        }
    }
    flush(&current, &mut body, &mut reply);
    reply
}
