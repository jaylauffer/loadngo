//! The chat: gpt-oss on loadngo's shared chat loop (`loadngo_inference::agent`). This
//! file only connects the engine (as the loop's backend), the harmony format (its
//! template) and the terminal; the turn loop, the tools, the guards and Jev's questions
//! are the loop's, the same for every local model.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use loadngo_gpt_oss::{chat::Harmony, tokenizer::Tokenizer};
use loadngo_inference::agent::{
    self,
    workspace::{Workspace, WorkspaceOptions},
    Agent, Backend, Ended, Event, Observer, TurnEnd,
};
use loadngo_inference::system_one::LabelModel;

use crate::engine::{Engine, Which};
use crate::jev::Judge;
use crate::{fail, Options};

/// The name this chat claims work under on the agent board.
pub const AGENT: &str = "gpt-oss";

/// The engine as the loop's backend: greedy decoding of the conversation session, Jev in
/// the side session.
pub struct GptOss<'t> {
    pub engine: Engine,
    tokenizer: &'t Tokenizer,
    /// The scores after the last token fed.
    logits: Vec<f32>,
    profile: bool,
    /// The last reply's speed, in tokens per second.
    pub rate: Option<f64>,
}

impl<'t> GptOss<'t> {
    pub fn new(engine: Engine, tokenizer: &'t Tokenizer, profile: bool) -> Self {
        Self {
            engine,
            tokenizer,
            logits: Vec::new(),
            profile,
            rate: None,
        }
    }
}

impl Backend for GptOss<'_> {
    fn load(&mut self, tokens: &[u32]) -> Result<(), String> {
        self.engine.reset(Which::Main)?;
        self.logits = self.engine.feed(Which::Main, tokens)?;
        if self.profile {
            eprintln!("[prompt] {}", self.engine.profile());
        }
        Ok(())
    }

    fn feed(&mut self, tokens: &[u32]) -> Result<(), String> {
        if !tokens.is_empty() {
            self.logits = self.engine.feed(Which::Main, tokens)?;
        }
        Ok(())
    }

    fn generate(
        &mut self,
        limit: usize,
        stops: &[u32],
        cancel: &AtomicBool,
        emit: &mut dyn FnMut(u32) -> bool,
    ) -> Result<(Vec<u32>, Ended), String> {
        let started = Instant::now();
        let mut out = Vec::new();
        let ended = loop {
            if cancel.load(Ordering::Relaxed) {
                break Ended::Cancelled;
            }
            if out.len() == limit {
                break Ended::Limit;
            }
            if self.engine.position(Which::Main) + 1 >= self.engine.context() {
                break Ended::Context;
            }
            let next = self
                .logits
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.total_cmp(b.1))
                .map(|(i, _)| i as u32)
                .ok_or("no logits")?;
            out.push(next);
            if stops.contains(&next) {
                break Ended::Stop;
            }
            self.logits = self.engine.feed(Which::Main, &[next])?;
            if !emit(next) {
                break Ended::Halted;
            }
        };
        let seconds = started.elapsed().as_secs_f64();
        if out.len() > 1 {
            self.rate = Some(out.len() as f64 / seconds);
        }
        if self.profile {
            eprintln!("[reply] {}", self.engine.profile());
        }
        Ok((out, ended))
    }

    fn position(&self) -> usize {
        self.engine.position(Which::Main)
    }

    fn capacity(&self) -> usize {
        self.engine.context()
    }

    fn judge(&mut self, date: &str) -> Option<Box<dyn LabelModel + '_>> {
        Some(Box::new(Judge::new(&mut self.engine, self.tokenizer, date)))
    }
}

/// What the chat reports, on stderr.
#[derive(Default)]
struct Terminal {
    /// The call being run, for its result line.
    call: Option<(String, String)>,
}

impl Observer for Terminal {
    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Prompt { tokens, seconds } => eprintln!(
                "{tokens} prompt tokens in {seconds:.2}s ({:.0} tokens/s)",
                tokens as f64 / seconds.max(1e-9)
            ),
            Event::Reply {
                tokens, seconds, ..
            } => eprintln!(
                "{tokens} tokens in {seconds:.2}s ({:.1} tokens/s)",
                tokens as f64 / seconds.max(1e-9)
            ),
            Event::Call { name, arguments } => {
                self.call = Some((name.to_owned(), arguments.trim().to_owned()));
            }
            Event::Result { name, chars, error } => {
                let arguments = self
                    .call
                    .take()
                    .filter(|(n, _)| n == name)
                    .map_or_else(String::new, |(_, a)| a);
                eprintln!("[tool] {name} {arguments} -> {chars} characters");
                if let Some(error) = error {
                    let first: String = error.chars().take(220).collect();
                    eprintln!("        {}", first.replace('\n', " "));
                }
            }
            Event::Note(note) => eprintln!("{note}"),
        }
    }
}

/// The chat for these options: tools from the shared workspace setup, or none.
pub fn chat<'t>(tokenizer: &'t Tokenizer, o: &Options) -> Agent<Harmony<'t>> {
    let template = Harmony::new(tokenizer, o.reasoning).unwrap_or_else(|e| fail(&e.to_string()));
    let workspace = if o.tools {
        let base = o
            .base
            .clone()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| ".".into());
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let memory = o.notes.then(|| {
            o.memory.clone().unwrap_or_else(|| {
                home.unwrap_or_default()
                    .join(".loadngo/gpt-oss/memory.jsonl")
            })
        });
        let workspace = Workspace::new(&WorkspaceOptions {
            base,
            agent: AGENT.into(),
            today: agent::clock::now().date,
            edit: o.edit,
            memory,
            web: o.web,
            cas_roots: o.cas_roots.clone(),
            cas_key: o.cas_key.clone(),
        })
        .unwrap_or_else(|e| fail(&e));
        for line in &workspace.described {
            eprintln!("{line}");
        }
        Some(workspace)
    } else {
        eprintln!("tools: off (--no-tools)");
        None
    };
    eprintln!(
        "jev: {}",
        if o.jev {
            "checkpoints every 6 tool calls; web gate"
        } else {
            "off (--no-jev)"
        }
    );
    Agent::new(template, workspace, o.jev, Box::new(Terminal::default()))
}

/// What the model is told about the chat it runs in.
fn about(backend: &GptOss<'_>) -> String {
    let mut about = format!(
        "About this chat: you are OpenAI's gpt-oss-20b (open weights), running locally on \
         Jay's Mac mini on loadngo's own Rust engine ({}), with a context of {} tokens. \
         Nothing you read leaves this machine except web_search and web_fetch.",
        backend.engine.name(),
        backend.capacity()
    );
    if let Some(rate) = backend.rate {
        about.push_str(&format!(
            " Your last reply was generated at {rate:.0} tokens/s."
        ));
    }
    about
}

/// One message: the answer to stdout, the rest to stderr.
pub fn turn(
    chat: &mut Agent<Harmony<'_>>,
    backend: &mut GptOss<'_>,
    text: &str,
    limit: usize,
    show_reasoning: bool,
) {
    chat.set_about(about(backend));
    let cancel = AtomicBool::new(false);
    let TurnEnd { read, stopped } =
        agent::turn(chat, backend, text, limit, &cancel).unwrap_or_else(|e| fail(&e));
    if show_reasoning && !read.reasoning.trim().is_empty() {
        eprintln!("[reasoning] {}", read.reasoning.trim());
    }
    if stopped.is_some() && read.answer.trim().is_empty() {
        eprintln!("[no answer]");
    } else {
        println!("{}", read.answer.trim());
    }
}

pub fn interactive(
    chat: &mut Agent<Harmony<'_>>,
    backend: &mut GptOss<'_>,
    limit: usize,
    o: &Options,
) {
    eprintln!(
        "chat: one message per line (arrow keys edit, Up/Down recall); /undo drops the last \
         exchange, /reset starts over, /quit or Ctrl-D stops"
    );
    let mut editor = loadngo_line_editor::LineEditor::new();
    loop {
        let line = match editor.read_line("> ") {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(e) => fail(&format!("reading the terminal: {e}")),
        };
        match line.trim() {
            "" => {}
            "/quit" => break,
            "/reset" => {
                chat.reset();
                eprintln!("[conversation cleared]");
            }
            "/undo" => eprintln!(
                "{}",
                if chat.undo() {
                    "[last exchange removed; file changes stay]"
                } else {
                    "[nothing to undo]"
                }
            ),
            text => turn(chat, backend, text, limit, o.show_reasoning),
        }
    }
}

/// Hands the session's edits off on the board and lists them.
pub fn finish(chat: &mut Agent<Harmony<'_>>) {
    let left = chat.finish();
    if !left.is_empty() {
        eprintln!("[board] handed off on AGENT-BOARD.md; uncommitted changes:");
        for (area, file) in left {
            eprintln!("  {area}: {file}");
        }
    }
}
