//! gpt-oss's chat format, harmony, with function tools.
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
//! Tools ([`tool_namespace`]) are declared in the developer message as a TypeScript-like
//! `functions` namespace, and the system message adds that calls go to the commentary
//! channel. The model calls one with a header naming `to=functions.NAME`, then the JSON
//! arguments, then `<|call|>` ([`read_call`]). The result returns as
//! `<|start|>functions.NAME to=assistant<|channel|>commentary<|message|>"…"<|end|>`, the
//! text JSON-encoded ([`tool_result`]), and the reply goes on after `<|start|>assistant`.
//!
//! This follows the chat template shipped with the model, from which these strings are
//! taken. Text goes through [`Tokenizer::encode`], so a message that spells a control
//! token cannot forge one.

use loadngo_inference::agent::{Call, Prompt, Read, Rendered, Template};
use serde_json::Value;

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
    /// The tools, rendered by [`tool_namespace`].
    pub tools: Option<String>,
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
            tools: None,
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
        let mut system = format!(
            "{}\nKnowledge cutoff: {KNOWLEDGE_CUTOFF}\nCurrent date: {}\n\nReasoning: {}\n\n{CHANNELS}",
            self.identity,
            self.date,
            self.reasoning.name()
        );
        if self.tools.is_some() {
            system
                .push_str("\nCalls to these tools must go to the commentary channel: 'functions'.");
        }
        message(&mut out, "system", None, &system);
        if self.instructions.is_some() || self.tools.is_some() {
            let mut developer = String::new();
            if let Some(instructions) = &self.instructions {
                developer.push_str(&format!("# Instructions\n\n{instructions}\n\n"));
            }
            if let Some(tools) = &self.tools {
                developer.push_str("# Tools\n\n");
                developer.push_str(tools);
            }
            message(&mut out, "developer", None, &developer);
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

/// The TypeScript-like type the chat template gives a JSON Schema property. Covers what
/// loadngo's tools declare (strings, numbers, booleans, string enums and arrays of
/// those); anything else is `any`.
fn typescript(spec: &Value) -> String {
    match spec.get("type").and_then(Value::as_str) {
        Some("string") => match spec.get("enum").and_then(Value::as_array) {
            Some(values) => values
                .iter()
                .filter_map(Value::as_str)
                .map(|v| format!("\"{v}\""))
                .collect::<Vec<_>>()
                .join(" | "),
            None => "string".into(),
        },
        Some("number" | "integer") => "number".into(),
        Some("boolean") => "boolean".into(),
        Some("array") => match spec.pointer("/items/type").and_then(Value::as_str) {
            Some("string") => "string[]".into(),
            Some("number" | "integer") => "number[]".into(),
            Some("boolean") => "boolean[]".into(),
            _ => "any[]".into(),
        },
        _ => "any".into(),
    }
}

/// Renders tool declarations (a JSON array of `{"type": "function", "function": {name,
/// description, parameters}}`, as `loadngo_inference::tools::Toolbox::declaration` gives
/// them) as the chat template's `functions` namespace.
///
/// # Errors
/// When the declarations are not that shape.
pub fn tool_namespace(declarations: &str) -> Result<String, String> {
    let list: Value =
        serde_json::from_str(declarations).map_err(|e| format!("tool declarations: {e}"))?;
    let list = list
        .as_array()
        .ok_or("tool declarations must be a JSON array")?;
    let mut out = String::from("## functions\n\nnamespace functions {\n\n");
    for tool in list {
        let f = tool
            .get("function")
            .ok_or("a declaration has no `function`")?;
        let name = f
            .get("name")
            .and_then(Value::as_str)
            .ok_or("a tool has no name")?;
        let description = f.get("description").and_then(Value::as_str).unwrap_or("");
        out.push_str(&format!("// {description}\ntype {name} = "));
        let params = f.get("parameters");
        let properties = params
            .and_then(|p| p.get("properties"))
            .and_then(Value::as_object);
        match properties {
            Some(properties) if !properties.is_empty() => {
                let required: Vec<&str> = params
                    .and_then(|p| p.get("required"))
                    .and_then(Value::as_array)
                    .map(|r| r.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default();
                out.push_str("(_: {\n");
                for (param, spec) in properties {
                    if let Some(d) = spec.get("description").and_then(Value::as_str) {
                        out.push_str(&format!("// {d}\n"));
                    }
                    let optional = if required.contains(&param.as_str()) {
                        ""
                    } else {
                        "?"
                    };
                    out.push_str(&format!("{param}{optional}: {},\n", typescript(spec)));
                }
                out.push_str("}) => any;\n\n");
            }
            _ => out.push_str("() => any;\n\n"),
        }
    }
    out.push_str("} // namespace functions");
    Ok(out)
}

/// A tool call the model made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolCall {
    pub name: String,
    /// The JSON arguments, as written.
    pub arguments: String,
}

/// The tool call that ends `tokens` (generated tokens ending in `<|call|>`): the
/// recipient from the last message header (`to=functions.NAME`, before or after the
/// channel) and the text between that header's `<|message|>` and `<|call|>`.
pub fn read_call(tokenizer: &Tokenizer, tokens: &[u32]) -> Option<ToolCall> {
    let call = tokenizer.control("<|call|>")?;
    let message = tokenizer.control("<|message|>")?;
    let start = tokenizer.control("<|start|>");
    let end = tokens.iter().rposition(|&t| t == call)?;
    let body = tokens[..end].iter().rposition(|&t| t == message)?;
    let header_from = tokens[..body]
        .iter()
        .rposition(|&t| Some(t) == start)
        .map_or(0, |i| i + 1);
    let header = tokenizer.decode(&tokens[header_from..body]);
    let at = header.find("to=functions.")? + "to=functions.".len();
    let name: String = header[at..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    if name.is_empty() {
        return None;
    }
    Some(ToolCall {
        name,
        arguments: tokenizer.decode(&tokens[body + 1..end]),
    })
}

/// A tool's result as the model reads it, followed by `<|start|>assistant` for the reply
/// to go on: the text JSON-encoded, as the chat template's `tojson` does.
///
/// # Errors
/// When the tokenizer lacks a control token the format needs.
pub fn tool_result(
    tokenizer: &Tokenizer,
    name: &str,
    result: &str,
) -> Result<Vec<u32>, MissingControl> {
    let c = Controls::of(tokenizer)?;
    let mut out = tool_message(tokenizer, &c, name, result);
    out.push(c.start);
    out.extend(tokenizer.encode("assistant"));
    Ok(out)
}

/// A tool's result message, without the reply's opening after it.
fn tool_message(tokenizer: &Tokenizer, c: &Controls, name: &str, result: &str) -> Vec<u32> {
    let mut out = vec![c.start];
    out.extend(tokenizer.encode(&format!("functions.{name} to=assistant")));
    out.push(c.channel);
    out.extend(tokenizer.encode("commentary"));
    out.push(c.message);
    out.extend(tokenizer.encode(&Value::String(result.to_owned()).to_string()));
    out.push(c.end);
    out
}

/// The start of a typed question for System One (Jev): a system message, then `state`
/// opening a user message. Questions about one state share these tokens, so they are
/// read once ([`judge_question`] completes each).
///
/// # Errors
/// When the tokenizer lacks a control token the format needs.
pub fn judge_state(
    tokenizer: &Tokenizer,
    date: &str,
    state: &str,
) -> Result<Vec<u32>, MissingControl> {
    let c = Controls::of(tokenizer)?;
    let system = format!(
        "You judge text carefully and answer each question with the letter of one option.\n\
         Knowledge cutoff: {KNOWLEDGE_CUTOFF}\nCurrent date: {date}\n\nReasoning: low\n\n{CHANNELS}"
    );
    let mut out = vec![c.start];
    out.extend(tokenizer.encode("system"));
    out.push(c.message);
    out.extend(tokenizer.encode(&system));
    out.push(c.end);
    out.push(c.start);
    out.extend(tokenizer.encode("user"));
    out.push(c.message);
    out.extend(tokenizer.encode(&format!("{state}\n\n")));
    Ok(out)
}

/// The rest of a Jev question after [`judge_state`]: the question with its lettered
/// options, then the answer's start in the final channel, so the next token is the
/// letter.
///
/// # Errors
/// When the tokenizer lacks a control token the format needs.
pub fn judge_question(tokenizer: &Tokenizer, question: &str) -> Result<Vec<u32>, MissingControl> {
    let c = Controls::of(tokenizer)?;
    let mut out = tokenizer.encode(&format!(
        "{}\nAnswer with the letter only.",
        question.trim_end()
    ));
    out.push(c.end);
    out.push(c.start);
    out.extend(tokenizer.encode("assistant"));
    out.push(c.channel);
    out.extend(tokenizer.encode("final"));
    out.push(c.message);
    Ok(out)
}

/// Ends the model's answer as a finished message (`<|end|>`, as the history shows earlier
/// answers) and adds `text` as a new user message, then `<|start|>assistant` for the
/// reply: for a note the chat itself sends before an answer reaches the user.
///
/// # Errors
/// When the tokenizer lacks a control token the format needs.
pub fn follow_up(tokenizer: &Tokenizer, text: &str) -> Result<Vec<u32>, MissingControl> {
    let c = Controls::of(tokenizer)?;
    let mut out = vec![c.end, c.start];
    out.extend(tokenizer.encode("user"));
    out.push(c.message);
    out.extend(tokenizer.encode(text));
    out.push(c.end);
    out.push(c.start);
    out.extend(tokenizer.encode("assistant"));
    Ok(out)
}

/// Harmony as the shared chat loop's format (`loadngo_inference::agent`).
///
/// Each user message renders the whole conversation again: earlier replies keep only
/// their final channel, as the chat template writes them, and the developer message
/// opens with this turn's date and time, then the standing instructions, then the
/// turn's notes (facts about the chat, tool receipts).
pub struct Harmony<'t> {
    tokenizer: &'t Tokenizer,
    pub identity: String,
    pub reasoning: Reasoning,
    /// `<|return|>`, `<|call|>`.
    stops: [u32; 2],
}

impl<'t> Harmony<'t> {
    /// # Errors
    /// When the tokenizer lacks a control token the format needs.
    pub fn new(tokenizer: &'t Tokenizer, reasoning: Reasoning) -> Result<Self, MissingControl> {
        Controls::of(tokenizer)?;
        let get = |name: &'static str| tokenizer.control(name).ok_or(MissingControl(name));
        Ok(Self {
            tokenizer,
            identity: DEFAULT_IDENTITY.to_owned(),
            reasoning,
            stops: [get("<|return|>")?, get("<|call|>")?],
        })
    }

    /// The reasoning and answer of a reply.
    pub fn reply(&self, tokens: &[u32]) -> Reply {
        read_reply(self.tokenizer, tokens)
    }
}

impl Template for Harmony<'_> {
    fn render(&self, p: &Prompt<'_>) -> Result<Rendered, String> {
        let mut conversation = Conversation::new(p.now.date.clone());
        conversation.identity.clone_from(&self.identity);
        conversation.reasoning = self.reasoning;
        let mut developer = p.now.said.clone();
        for part in [p.instructions, p.notes] {
            if !part.is_empty() {
                developer.push_str("\n\n");
                developer.push_str(part);
            }
        }
        conversation.instructions = Some(developer);
        conversation.tools = p.tools.map(tool_namespace).transpose()?;
        for e in p.history {
            conversation.messages.push(Message::User(e.user.clone()));
            conversation
                .messages
                .push(Message::Assistant(e.answer.clone()));
        }
        conversation.messages.push(Message::User(p.user.to_owned()));
        Ok(Rendered::Full(
            conversation
                .prompt(self.tokenizer)
                .map_err(|e| e.to_string())?,
        ))
    }

    fn stops(&self) -> &[u32] {
        &self.stops
    }

    fn read(&self, reply: &[u32]) -> Read {
        let r = read_reply(self.tokenizer, reply);
        let calls = if reply.last() == Some(&self.stops[1]) {
            read_call(self.tokenizer, reply)
                .map(|c| Call {
                    id: None,
                    name: c.name,
                    arguments: c.arguments,
                })
                .into_iter()
                .collect()
        } else {
            Vec::new()
        };
        Read {
            calls,
            answer: r.answer,
            reasoning: r.analysis,
            complete: r.complete,
        }
    }

    fn results(
        &self,
        ended_by: Option<u32>,
        results: &[(Call, String)],
    ) -> Result<Vec<u32>, String> {
        let c = Controls::of(self.tokenizer).map_err(|e| e.to_string())?;
        let mut out: Vec<u32> = ended_by.into_iter().collect();
        for (call, text) in results {
            out.extend(tool_message(self.tokenizer, &c, &call.name, text));
        }
        out.push(c.start);
        out.extend(self.tokenizer.encode("assistant"));
        Ok(out)
    }

    /// The answer's `<|return|>` becomes `<|end|>`, as history writes a finished answer.
    fn note(&self, _ended_by: Option<u32>, text: &str) -> Result<Vec<u32>, String> {
        follow_up(self.tokenizer, text).map_err(|e| e.to_string())
    }

    fn answer_opening(&self, text: &str) -> Result<Vec<u32>, String> {
        let c = Controls::of(self.tokenizer).map_err(|e| e.to_string())?;
        let mut out = vec![c.channel];
        out.extend(self.tokenizer.encode("final"));
        out.push(c.message);
        out.extend(self.tokenizer.encode(text));
        Ok(out)
    }
}
