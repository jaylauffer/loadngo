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
    transcript::{Tee, Transcript},
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
    /// The context, as tokens (for a saved chat).
    held: Vec<u32>,
}

impl<'t> GptOss<'t> {
    pub fn new(engine: Engine, tokenizer: &'t Tokenizer, profile: bool) -> Self {
        Self {
            engine,
            tokenizer,
            logits: Vec::new(),
            profile,
            rate: None,
            held: Vec::new(),
        }
    }
}

impl Backend for GptOss<'_> {
    fn load(&mut self, tokens: &[u32]) -> Result<(), String> {
        self.engine.reset(Which::Main)?;
        self.held.clear();
        self.logits = self.engine.feed(Which::Main, tokens)?;
        self.held.extend_from_slice(tokens);
        if self.profile {
            eprintln!("[prompt] {}", self.engine.profile());
        }
        Ok(())
    }

    fn feed(&mut self, tokens: &[u32]) -> Result<(), String> {
        if !tokens.is_empty() {
            self.logits = self.engine.feed(Which::Main, tokens)?;
            self.held.extend_from_slice(tokens);
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
            self.held.push(next);
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

    fn truncate(&mut self, len: usize) -> Result<(), String> {
        // Harmony renders the whole conversation each turn; the next turn reloads.
        if len < self.engine.position(Which::Main) {
            self.engine.truncate(Which::Main, len)?;
            self.held.truncate(len);
            self.logits.clear();
        }
        Ok(())
    }

    fn held(&self) -> &[u32] {
        &self.held
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
            Event::Result { name, text, ok } => {
                let arguments = self
                    .call
                    .take()
                    .filter(|(n, _)| n == name)
                    .map_or_else(String::new, |(_, a)| a);
                eprintln!(
                    "[tool] {name} {arguments} -> {} characters",
                    text.chars().count()
                );
                if !ok {
                    let first: String = text.chars().take(220).collect();
                    eprintln!("        {}", first.replace('\n', " "));
                }
            }
            Event::User(_) | Event::Token(_) | Event::State(_) | Event::TurnEnd { .. } => {}
            Event::Note(note) => eprintln!("{note}"),
        }
    }
}

/// The chat for these options: tools from the shared workspace setup, or none; with
/// `--resume`, the saved state to pick up ([`Agent::restore`]) once the backend exists.
pub fn chat<'t>(
    tokenizer: &'t Tokenizer,
    o: &Options,
) -> (Agent<'t, Harmony<'t>>, Option<serde_json::Value>) {
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
    let mut observers: Vec<Box<dyn Observer>> = vec![Box::new(Terminal::default())];
    let mut saved = None;
    if let Some(home) = std::env::var_os("HOME") {
        let dir = PathBuf::from(home).join(".loadngo/gpt-oss/transcripts");
        let opened = match &o.resume {
            Some(which) => Transcript::resume(&dir, which, "harmony").map(|(t, state)| {
                saved = Some(state);
                t
            }),
            None => Transcript::create(&dir, "harmony", "gpt-oss-20b"),
        };
        match opened {
            Ok(t) => {
                eprintln!("transcript: {}", t.path().display());
                observers.push(Box::new(t));
            }
            Err(e) if o.resume.is_some() => fail(&e),
            Err(e) => eprintln!("transcript: {e}; the chat is not saved"),
        }
    }
    let mut chat = Agent::new(template, workspace, o.jev, Box::new(Tee(observers)));
    chat.set_budget(o.budget);
    (chat, saved)
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
    chat: &mut Agent<'_, Harmony<'_>>,
    backend: &mut GptOss<'_>,
    text: &str,
    limit: usize,
    show_reasoning: bool,
) {
    chat.set_about(about(backend));
    let cancel = AtomicBool::new(false);
    let end = agent::turn(chat, backend, text, limit, &cancel).unwrap_or_else(|e| fail(&e));
    show(&end, show_reasoning);
}

/// The answer to stdout; reasoning and a pause to stderr.
fn show(end: &TurnEnd, show_reasoning: bool) {
    let TurnEnd {
        read,
        stopped,
        paused,
    } = end;
    if let Some(why) = paused {
        eprintln!(
            "[paused: {why}. /continue goes on; a new message answers waiting calls as not run]"
        );
        return;
    }
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
    chat: &mut Agent<'_, Harmony<'_>>,
    backend: &mut GptOss<'_>,
    limit: usize,
    o: &Options,
) {
    eprintln!(
        "chat: one message per line (arrow keys edit, Up/Down recall); /continue goes on with a \
         paused turn, /undo drops the last exchange, /reset starts over, /quit or Ctrl-D stops"
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
                let keep = chat.reset();
                backend.truncate(keep).unwrap_or_else(|e| fail(&e));
                eprintln!("[conversation cleared]");
            }
            "/continue" => {
                let cancel = AtomicBool::new(false);
                match agent::resume(chat, backend, limit, &cancel) {
                    Ok(Some(end)) => show(&end, o.show_reasoning),
                    Ok(None) => eprintln!("[nothing to continue]"),
                    Err(e) => fail(&e),
                }
            }
            "/undo" => match chat.undo() {
                Some(at) => {
                    backend.truncate(at).unwrap_or_else(|e| fail(&e));
                    eprintln!("[last exchange removed; file changes stay]");
                }
                None => eprintln!("[nothing to undo]"),
            },
            text => turn(chat, backend, text, limit, o.show_reasoning),
        }
    }
}

/// Hands the session's edits off on the board and lists them.
pub fn finish(chat: &mut Agent<'_, Harmony<'_>>) {
    let left = chat.finish();
    if !left.is_empty() {
        eprintln!("[board] handed off on AGENT-BOARD.md; uncommitted changes:");
        for (area, file) in left {
            eprintln!("  {area}: {file}");
        }
    }
}

/// `--eval FILE`: the orchestration cases scored by the rules and by gpt-oss's typed
/// answers (`loadngo_inference::agent::eval`); the report to stderr, the answers as JSON to
/// stdout.
pub fn evaluate(backend: &mut GptOss<'_>, path: &std::path::Path) {
    use loadngo_inference::agent::eval;
    let text =
        std::fs::read_to_string(path).unwrap_or_else(|e| fail(&format!("{}: {e}", path.display())));
    let cases = eval::load(&text).unwrap_or_else(|e| fail(&e));
    let date = agent::clock::now().date;
    let mut run = eval::Run::default();
    for (i, case) in cases.iter().enumerate() {
        let started = Instant::now();
        let judged = {
            let mut judge = backend.judge(&date).unwrap_or_else(|| fail("no judge"));
            eval::judge(judge.as_mut(), case)
        };
        let j = judged.unwrap_or_else(|e| fail(&e));
        eprintln!(
            "[{}/{}] {}: key {}, model {} (attention {:.2}), rules {}; {:.1}s",
            i + 1,
            cases.len(),
            case.id,
            case.class.name(),
            j.best().name(),
            j.attention,
            eval::mechanical(&case.report).name(),
            started.elapsed().as_secs_f64()
        );
        run.add(case, Some(&j));
    }
    eprint!("{}", run.report("gpt-oss-20b"));
    println!("{}", run.to_json("gpt-oss-20b"));
}
