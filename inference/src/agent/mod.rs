//! One chat program for every local model: the turn loop that runs a model's tool calls
//! and checks its work, independent of the model's chat format and of how its tokens are
//! computed. See `docs/AGENT_LOOP.md`.
//!
//! Until 2026-10-09 Kimi's chat (kimi-k3-in-rust) and gpt-oss's (`gpt_oss_generate`)
//! were separate programs, and each fix landed in one of them. Here:
//!
//! - a [`Template`] is a model's chat format: how a prompt, tool results and notes from
//!   the chat are written as tokens, and how a reply is read back;
//! - a [`Backend`] holds the model's context and generates (the GPU or CPU path, with a
//!   side session for Jev's questions);
//! - an [`Agent`] is the turn state: it decides, after each reply, whether to run tools,
//!   send a note, or end the turn, and runs the tools with the guards that need no
//!   judging (repeated calls, write failures, edits undone, unchecked changes) and Jev's
//!   checkpoints;
//! - [`turn`] drives one user message through them, blocking, for a terminal.
//!
//! The agent does no I/O of its own besides calling tools: a driver feeds it replies and
//! feeds the backend what it returns, so a GUI can run the same agent from host-proactor
//! completions instead of blocking. Tools that run programs (`cargo`, `git`) and the
//! chat's own checks wait in a loadngo proactor (`work_tools::run`).

pub mod clock;
pub mod evidence;
pub mod guards;
pub mod jev;
pub mod transcript;
pub mod workspace;

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use serde_json::Value;

use crate::edit_tools::EditSession;
use crate::system_one::LabelModel;
use crate::tools::Toolbox;
use clock::Now;
use evidence::{excerpt, Evidence};
use workspace::Workspace;

/// Longest tool result passed back, in characters; the tools bound their own output
/// well below this, and the rest of the context stays for the reply.
const MAX_RESULT_CHARS: usize = 24_000;
/// Tool calls between Jev checkpoints.
const CHECKPOINT_EVERY: usize = 6;
/// Write failures without a successful write that end a turn.
const WRITE_FAILURES: usize = 5;
/// Positions kept free for the reply after a prompt, results or a note.
pub const REPLY_RESERVE: usize = 512;
/// How a reply begins once the tools are closed (the same device as Kimi's handoff
/// opening: on the model, an instruction alone was answered with one more tool call).
pub const ANSWER_OPENING: &str = "I'll stop searching here and answer from what I have read.\n\n";

/// A tool call read from a reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Call {
    /// The format's call id, when it has one (Kimi Linear's `functions.NAME:N`).
    pub id: Option<String>,
    pub name: String,
    /// The JSON arguments, as written.
    pub arguments: String,
}

/// A reply read back.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Read {
    pub calls: Vec<Call>,
    /// What the user is shown.
    pub answer: String,
    /// Reasoning the format keeps apart (gpt-oss's analysis channel).
    pub reasoning: String,
    /// Whether the reply ended as the format ends a finished message.
    pub complete: bool,
}

/// An earlier user message and the answer it got.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exchange {
    pub user: String,
    pub answer: String,
}

/// What a new user message is written from.
pub struct Prompt<'a> {
    pub now: &'a Now,
    /// The standing instructions ([`Workspace::instructions`]).
    pub instructions: &'a str,
    /// Facts about the chat itself ([`Agent::set_about`]).
    pub about: &'a str,
    /// Receipts of the tool calls made so far, across turns. A format that rewrites the
    /// conversation each turn needs them (earlier calls are gone from its history); an
    /// appending format already holds every call and result, and leaves them out.
    pub evidence: &'a str,
    /// The tool declarations ([`Toolbox::declaration`]), when there are tools.
    pub tools: Option<&'a str>,
    /// Earlier exchanges, oldest first.
    pub history: &'a [Exchange],
    pub user: &'a str,
    /// Whether the backend holds nothing yet: an appending format writes its opening
    /// first. (After [`Agent::prepare`] the opening is already held.)
    pub first: bool,
}

/// A rendered prompt.
pub enum Rendered {
    /// The whole context, replacing what the backend holds (a format that rewrites
    /// earlier turns, as harmony drops earlier reasoning).
    Full(Vec<u32>),
    /// Tokens to add after what the backend holds.
    Append(Vec<u32>),
}

/// A model's chat format.
pub trait Template {
    /// # Errors
    /// When the tokenizer lacks a token the format needs, or the tools cannot be declared.
    fn render(&self, prompt: &Prompt<'_>) -> Result<Rendered, String>;
    /// What every conversation opens with, the same each time (an appending format's
    /// instructions and tool declarations), so a backend can read it once and start each
    /// conversation from it. Empty for a format that renders everything each turn.
    ///
    /// # Errors
    /// As [`Self::render`].
    fn opening(&self, _instructions: &str, _tools: Option<&str>) -> Result<Vec<u32>, String> {
        Ok(Vec::new())
    }
    /// The tokens that end a reply: a finished answer or a tool call.
    fn stops(&self) -> &[u32];
    /// The calls, answer and reasoning in a reply (its tokens, the ending one included).
    fn read(&self, reply: &[u32]) -> Read;
    /// The results of a reply's calls, then the opening of the next reply. `ended_by` is
    /// the token that ended the reply, which the backend has not been fed.
    ///
    /// # Errors
    /// When the tokenizer lacks a token the format needs.
    fn results(
        &self,
        ended_by: Option<u32>,
        results: &[(Call, String)],
    ) -> Result<Vec<u32>, String>;
    /// A message from the chat program (not from Jay) after an answer, then the opening
    /// of the next reply.
    ///
    /// # Errors
    /// When the tokenizer lacks a token the format needs.
    fn note(&self, ended_by: Option<u32>, text: &str) -> Result<Vec<u32>, String>;
    /// Tokens that begin the answer itself with `text`, after a reply's opening.
    ///
    /// # Errors
    /// When the tokenizer lacks a token the format needs.
    fn answer_opening(&self, text: &str) -> Result<Vec<u32>, String>;
}

/// How a generation ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ended {
    /// On one of the template's stop tokens.
    Stop,
    /// The token limit for one reply.
    Limit,
    /// The context is full.
    Context,
    Cancelled,
    /// The driver stopped it (a reply looping on one block).
    Halted,
}

/// The model: its context and generation.
pub trait Backend {
    /// Replaces the context with `tokens`.
    ///
    /// # Errors
    /// Whatever stopped the model.
    fn load(&mut self, tokens: &[u32]) -> Result<(), String>;
    /// Adds `tokens` to the context.
    ///
    /// # Errors
    /// Whatever stopped the model.
    fn feed(&mut self, tokens: &[u32]) -> Result<(), String>;
    /// Generates up to `limit` tokens. Every token is fed except one in `stops`, which
    /// ends the reply and is returned last; `emit` sees each token and returns `false`
    /// to stop ([`Ended::Halted`]).
    ///
    /// # Errors
    /// Whatever stopped the model.
    fn generate(
        &mut self,
        limit: usize,
        stops: &[u32],
        cancel: &AtomicBool,
        emit: &mut dyn FnMut(u32) -> bool,
    ) -> Result<(Vec<u32>, Ended), String>;
    /// Drops every position from `len` on (after `/undo` or `/reset`).
    ///
    /// # Errors
    /// Whatever stopped the model.
    fn truncate(&mut self, len: usize) -> Result<(), String>;
    /// Positions held.
    fn position(&self) -> usize;
    /// Positions the context can hold.
    fn capacity(&self) -> usize;
    /// The model as Jev, judging in a session apart from the conversation; `None`
    /// when it cannot.
    fn judge(&mut self, date: &str) -> Option<Box<dyn LabelModel + '_>>;
}

/// What the chat reports as it goes (to a terminal, a transcript).
pub enum Event<'a> {
    /// Jay's message, as a turn begins.
    User(&'a str),
    /// Tokens fed: the prompt, results or a note.
    Prompt {
        tokens: usize,
        seconds: f64,
    },
    /// A token as it is generated, for showing the reply as it is written.
    Token(u32),
    /// A reply, read.
    Reply {
        read: &'a Read,
        tokens: usize,
        seconds: f64,
        ended: Ended,
    },
    Call {
        name: &'a str,
        arguments: &'a str,
    },
    /// A call's result, as the model reads it.
    Result {
        name: &'a str,
        text: &'a str,
        ok: bool,
    },
    /// A check, a Jev answer or a guard's note, already worded (`[check] …`).
    Note(&'a str),
    /// The turn is over: why, when it ended without an answer.
    TurnEnd {
        stopped: Option<&'a str>,
        replies: usize,
        tokens: usize,
        seconds: f64,
    },
}

pub trait Observer {
    fn event(&mut self, event: Event<'_>);
}

/// An observer that drops everything.
pub struct Quiet;
impl Observer for Quiet {
    fn event(&mut self, _: Event<'_>) {}
}

/// After a reply or a round of tools: what to do next.
#[derive(Debug, PartialEq, Eq)]
pub enum Next {
    /// Run these calls ([`Agent::run`]).
    Tools(Vec<Call>),
    /// Feed these tokens and generate again.
    Feed(Vec<u32>),
    /// The turn is over with this answer.
    Answer(Read),
    /// The turn is over without a proper answer.
    Stop { why: String, read: Read },
}

/// One user turn's state.
#[derive(Default)]
struct Turn {
    request: String,
    date: String,
    /// One line per call, for Jev.
    work: Vec<String>,
    since_checkpoint: usize,
    nudged: bool,
    asked_jay: bool,
    checked_done: bool,
    /// The tools are closed: the next reply must be the answer.
    closed: bool,
    web_approved: bool,
    /// Calls of one tool failing in a row.
    failing: (String, usize),
    /// Files changed since the last `cargo` run that succeeded.
    unchecked: bool,
    /// Files changed this turn (the chat checks them when it ends).
    wrote: bool,
    /// The verification note was sent.
    gated: bool,
    /// The archive coverage note was sent.
    scope_checked: bool,
    /// Revisions each file has had in this turn.
    revisions: HashMap<String, Vec<String>>,
    /// Calls that succeeded this turn (the repeat guard).
    earlier: Vec<(String, Value)>,
    /// Rounds made only of repeated calls.
    repeated_rounds: usize,
    /// Writes that failed, with their errors, since the last successful write.
    failed_writes: Vec<((String, Value), String)>,
    /// The token that ended the last reply, not yet fed.
    ended_by: Option<u32>,
    /// Where the turn began in the backend's context.
    start: usize,
    started: Option<Instant>,
    replies: usize,
    tokens: usize,
}

/// The chat: the tools, the instructions, the conversation, and the turn in progress.
pub struct Agent<'o, T> {
    pub template: T,
    tools: Option<Toolbox>,
    declaration: Option<String>,
    instructions: String,
    about: String,
    edits: Option<Rc<EditSession>>,
    base: PathBuf,
    jev: bool,
    history: Vec<Exchange>,
    /// Where each exchange began in the backend's context.
    starts: Vec<usize>,
    /// Positions the opening takes ([`Self::prepare`]).
    opening: usize,
    evidence: Evidence,
    verified: Option<(Vec<String>, bool)>,
    last_answer: String,
    turn: Turn,
    handed_off: bool,
    observer: Box<dyn Observer + 'o>,
}

impl<'o, T: Template> Agent<'o, T> {
    /// An agent with `workspace`'s tools, or none.
    pub fn new(
        template: T,
        workspace: Option<Workspace>,
        jev: bool,
        observer: Box<dyn Observer + 'o>,
    ) -> Self {
        let (tools, edits, instructions, base) = match workspace {
            Some(w) => (Some(w.tools), w.edits, w.instructions, w.base),
            None => (None, None, String::new(), PathBuf::from(".")),
        };
        let declaration = tools
            .as_ref()
            .filter(|t| !t.is_empty())
            .map(Toolbox::declaration);
        Self {
            template,
            tools,
            declaration,
            instructions,
            about: String::new(),
            edits,
            base,
            jev,
            history: Vec::new(),
            starts: Vec::new(),
            opening: 0,
            evidence: Evidence::default(),
            verified: None,
            last_answer: String::new(),
            turn: Turn::default(),
            handed_off: false,
            observer,
        }
    }

    /// Facts about the chat itself, told to the model each turn: which model it is,
    /// where it runs, how fast. Kimi could not answer where her transcripts were or how
    /// fast she ran (2026-10-08) because nothing told her.
    pub fn set_about(&mut self, about: impl Into<String>) {
        self.about = about.into();
    }

    pub fn history(&self) -> &[Exchange] {
        &self.history
    }

    /// Reads the format's opening into `backend` once, so the first message and every
    /// [`Self::reset`] start from it.
    ///
    /// # Errors
    /// When the template or the backend fails.
    pub fn prepare(&mut self, backend: &mut dyn Backend) -> Result<usize, String> {
        let opening = self
            .template
            .opening(&self.instructions, self.declaration.as_deref())?;
        if !opening.is_empty() {
            backend.load(&opening)?;
        }
        self.opening = opening.len();
        Ok(self.opening)
    }

    /// Removes the last exchange; returns where it began in the backend's context, for
    /// [`Backend::truncate`]. File changes stay.
    pub fn undo(&mut self) -> Option<usize> {
        self.history.pop()?;
        self.starts.pop()
    }

    /// Starts the conversation over (receipts included); returns the context to keep,
    /// for [`Backend::truncate`]: the opening.
    pub fn reset(&mut self) -> usize {
        self.history.clear();
        self.starts.clear();
        self.evidence = Evidence::default();
        self.opening
    }

    fn note(&mut self, text: &str) {
        self.observer.event(Event::Note(text));
    }

    /// Begins a turn with Jay's message: the prompt to give the backend.
    /// `start` is the backend's position now.
    ///
    /// # Errors
    /// When the template cannot render it.
    pub fn begin(&mut self, text: &str, start: usize) -> Result<Rendered, String> {
        let now = clock::now();
        self.turn = Turn {
            request: text.to_owned(),
            date: now.date.clone(),
            start,
            started: Some(Instant::now()),
            ..Turn::default()
        };
        self.observer.event(Event::User(text));
        self.evidence.turn += 1;
        if let Some(edits) = &self.edits {
            edits.set_task(text);
        }
        let evidence = if self.tools.is_some() {
            self.evidence.summary()
        } else {
            String::new()
        };
        self.template.render(&Prompt {
            now: &now,
            instructions: &self.instructions,
            about: &self.about,
            evidence: &evidence,
            tools: self.declaration.as_deref(),
            history: &self.history,
            user: text,
            first: start == 0,
        })
    }

    pub fn stops(&self) -> &[u32] {
        self.template.stops()
    }

    /// Reads a reply and decides what follows. `room` is how many positions the
    /// backend has left after it;
    /// `seconds` is how long it took, for the observer.
    pub fn reply(&mut self, reply: &[u32], ended: Ended, room: usize, seconds: f64) -> Next {
        self.turn.ended_by = match ended {
            Ended::Stop => reply.last().copied(),
            _ => None,
        };
        let read = self.template.read(reply);
        self.turn.replies += 1;
        self.turn.tokens += reply.len();
        self.observer.event(Event::Reply {
            read: &read,
            tokens: reply.len(),
            seconds,
            ended,
        });
        let unfinished = match ended {
            Ended::Stop => None,
            Ended::Limit => Some("the reply reached its token limit"),
            Ended::Context => Some("the context is full"),
            Ended::Cancelled => Some("cancelled"),
            Ended::Halted => Some("the reply was repeating one block"),
        };
        if let Some(why) = unfinished {
            return self.stop(why, read);
        }
        if !read.calls.is_empty() {
            if self.turn.closed {
                return self.stop("tools were called after they were closed", read);
            }
            if self.tools.is_none() {
                return self.stop("a tool was called, but this chat has no tools", read);
            }
            return Next::Tools(read.calls);
        }
        if let Some(note) = self.before_answer() {
            match self.template.note(self.turn.ended_by, &note) {
                Ok(tokens) if tokens.len() + REPLY_RESERVE <= room => return Next::Feed(tokens),
                Ok(_) => self.note("[check] no room left for the check; the answer goes through"),
                Err(e) => self.note(&format!("[check] cannot write the note: {e}")),
            }
        }
        self.finish_turn(&read.answer, None);
        Next::Answer(read)
    }

    /// A note the answer must wait for: archive coverage, or changes no check has
    /// passed since. Each is sent once a turn.
    fn before_answer(&mut self) -> Option<String> {
        if self.turn.closed {
            return None;
        }
        let archive_work = self
            .turn
            .work
            .iter()
            .any(|w| w.contains(". cas_find ") || w.contains(". cas_grep "));
        let request = self.turn.request.to_ascii_lowercase();
        let archive_followup = request.contains("archive")
            || request
                .split(|c: char| !c.is_ascii_alphanumeric())
                .any(|w| w == "cas");
        if !self.turn.scope_checked
            && (archive_work || archive_followup)
            && (self.evidence.any(", cas_find ") || self.evidence.any(", cas_grep "))
        {
            self.turn.scope_checked = true;
            self.note("[check] archive coverage: asked to scope the answer to actual searches");
            return Some(format!(
                "Archive coverage check before your answer reaches Jay. Revise the answer using \
                 ONLY these actual receipts:\n{}\n\n\
                 Zero matches means not found in the stated scope, never that no references \
                 exist. cas_find searches a path glob only; * does not cross directories. \
                 cas_grep searches only the exact case-sensitive literal in eligible UTF-8 files, \
                 and stops at limits. If a result is incomplete or skips files, say explicitly \
                 that absence has NOT been established and name the remaining scope. Do not say \
                 all files, all readable files, variants, or notes were checked without receipts \
                 proving that. Do not repeat a search just to repeat it; answer with its actual \
                 coverage and what remains unknown.",
                self.evidence.summary()
            ));
        }
        if self.turn.unchecked && !self.turn.gated {
            self.turn.gated = true;
            self.note("[check] files changed with no passing cargo run since; asked to check before answering");
            return Some(
                "Automatic check before your answer reaches Jay: you changed files after your \
                 last successful cargo run (or ran none). Run cargo test, clippy and fmt (--check) \
                 in the crate you changed now, fix what fails, then answer again with the \
                 results. Do not say a check passed unless you ran it and it did."
                    .into(),
            );
        }
        None
    }

    fn stop(&mut self, why: &str, read: Read) -> Next {
        self.note(&format!("[stopped: {why}]"));
        self.finish_turn(&read.answer, Some(why));
        Next::Stop {
            why: why.to_owned(),
            read,
        }
    }

    /// Ends the turn: the exchange joins the history, and when files were written the
    /// chat checks the crates itself.
    fn finish_turn(&mut self, answer: &str, stopped: Option<&str>) {
        self.last_answer = answer.trim().to_owned();
        self.history.push(Exchange {
            user: std::mem::take(&mut self.turn.request),
            answer: self.last_answer.clone(),
        });
        self.starts.push(self.turn.start);
        if self.turn.wrote {
            if let Some(edits) = self.edits.clone() {
                if let Some(result) = workspace::verify(&self.base, &edits) {
                    for line in &result.0 {
                        self.note(&format!("[verify] {line}"));
                    }
                    self.verified = Some(result);
                }
            }
        }
        let seconds = self
            .turn
            .started
            .take()
            .map_or(0.0, |t| t.elapsed().as_secs_f64());
        self.observer.event(Event::TurnEnd {
            stopped,
            replies: self.turn.replies,
            tokens: self.turn.tokens,
            seconds,
        });
    }

    /// Ends a turn the backend or template failed in, so the history still matches what
    /// the backend holds.
    pub fn abandon(&mut self, why: &str) {
        self.note(&format!("[stopped: {why}]"));
        self.finish_turn("", Some(why));
    }

    /// Runs a reply's calls with the guards, and returns the results to feed (fitted
    /// into `room` positions), or the end of the turn.
    #[allow(clippy::too_many_lines)]
    pub fn run(&mut self, calls: &[Call], backend: &mut dyn Backend, room: usize) -> Next {
        let mut results: Vec<(Call, String)> = Vec::with_capacity(calls.len());
        let mut repeats = 0;
        let mut write_limit = None;
        for call in calls {
            self.observer.event(Event::Call {
                name: &call.name,
                arguments: &call.arguments,
            });
            if write_limit.is_some() {
                results.push((
                    call.clone(),
                    "Not run: the turn stopped before this call.".into(),
                ));
                continue;
            }
            let key = guards::call_key(&call.name, &call.arguments);
            let repeated = !guards::repeatable(&call.name) && self.turn.earlier.contains(&key);
            let mut result = if repeated {
                repeats += 1;
                "Not run: you already made this exact call in this turn, and its result is \
                 above. Do not make it again: use what it returned, try a different call, or \
                 answer with what you have."
                    .to_owned()
            } else {
                self.call(backend, call)
            };
            result = excerpt(&result, MAX_RESULT_CHARS);
            let ok = !result.starts_with("error:") && !repeated;
            self.observer.event(Event::Result {
                name: &call.name,
                text: &result,
                ok: !result.starts_with("error:"),
            });
            if ok {
                if guards::mutates(&call.name) {
                    // What was read may read differently now; an edit is never replayed.
                    self.turn.earlier.retain(|(name, _)| guards::writes(name));
                    self.turn.repeated_rounds = 0;
                }
                if !self.turn.earlier.contains(&key) {
                    self.turn.earlier.push(key.clone());
                }
                if guards::writes(&call.name) {
                    self.turn.unchecked = true;
                    self.turn.wrote = true;
                    self.turn.failed_writes.clear();
                }
                if call.name == "cargo" && result.contains("succeeded (exit 0)") {
                    self.turn.unchecked = false;
                }
            }
            if result.starts_with("error:") {
                self.turn.failing = if self.turn.failing.0 == call.name {
                    (call.name.clone(), self.turn.failing.1 + 1)
                } else {
                    (call.name.clone(), 1)
                };
                if guards::writes(&call.name) {
                    let failure = (key.clone(), result.clone());
                    if self.turn.failed_writes.contains(&failure) {
                        write_limit = Some("the same write failed twice with the same error");
                    }
                    self.turn.failed_writes.push(failure);
                    if self.turn.failed_writes.len() >= WRITE_FAILURES {
                        write_limit = Some("five writes failed without one succeeding");
                    }
                }
            } else if !repeated {
                self.turn.failing = (String::new(), 0);
            }
            if call.name.starts_with("text_") && ok {
                if let Some(note) =
                    guards::undone(&mut self.turn.revisions, &call.arguments, &result)
                {
                    self.note(&format!("[undo] {note}"));
                    result.push_str(&format!("\n\n[{note}]"));
                }
            }
            if self.turn.failing.1 == 3 {
                result.push_str(&if call.name == "text_edit" {
                    "\n\n[text_edit has failed 3 times in a row. Copy old_text exactly as the file \
                     has it, without text_read's line numbers, or anchor on one short line that \
                     appears once (such as a function's first line) and repeat it at the start of \
                     new_text.]"
                        .to_owned()
                } else {
                    format!(
                        "\n\n[{} has failed 3 times in a row. Read the error above and change the \
                         call or the approach instead of repeating it.]",
                        call.name
                    )
                });
            }
            if !repeated {
                self.turn.work.push(guards::work_line(
                    self.turn.work.len() + 1,
                    &call.name,
                    &call.arguments,
                    &result,
                ));
                self.evidence.record(&call.name, &call.arguments, &result);
                self.turn.since_checkpoint += 1;
                if let Some(note) = self.checkpoint(backend) {
                    result.push_str(&note);
                }
            }
            results.push((call.clone(), result));
        }
        if let Some(why) = write_limit {
            return self.stop(
                &format!("{why}; fix the cause, then ask again"),
                Read::default(),
            );
        }
        if repeats == calls.len() {
            self.turn.repeated_rounds += 1;
            if self.turn.repeated_rounds == 2 {
                self.turn.closed = true;
                self.note("[tools closed: the same calls were repeated; asked for an answer]");
                for (_, result) in &mut results {
                    result.push_str(
                        "\n\n[Your tools are closed for this turn. Answer now with what you \
                         found, with paths and line numbers, and what you did not find.]",
                    );
                }
            }
        }
        let mut tokens = match self.fit(&mut results, room) {
            Ok(tokens) => tokens,
            Err(why) => return self.stop(&why, Read::default()),
        };
        if self.turn.closed {
            match self.template.answer_opening(ANSWER_OPENING) {
                Ok(opening) => tokens.extend(opening),
                Err(e) => return self.stop(&e, Read::default()),
            }
        }
        Next::Feed(tokens)
    }

    /// One call: through the web gate when it goes to the web.
    fn call(&mut self, backend: &mut dyn Backend, call: &Call) -> String {
        if self.tools.is_none() {
            return "error: tools are off".into();
        }
        if self.jev && call.name.starts_with("web_") && !self.turn.web_approved {
            let intent = match call.name.as_str() {
                "web_search" => format!("search the web with {}", call.arguments),
                _ => format!("fetch from the web {}", call.arguments),
            };
            let summary = self.evidence.summary();
            let judged = backend.judge(&self.turn.date).map(|mut judge| {
                jev::local_first(judge.as_mut(), &self.turn.request, &intent, &summary)
            });
            match judged {
                Some(Ok(p)) if p >= 0.5 => {
                    self.note(&format!("[jev] web gate: local first (p {p:.2}); not sent"));
                    return format!(
                        "not sent: this looks answerable on this machine (Jev, p {p:.2}). Look in \
                         the workspace contents (fs_grep), each relevant archive's contents \
                         (cas_archives then cas_grep), and your notes (memory_search) first. \
                         cas_find checks paths only; it cannot rule out content references."
                    );
                }
                Some(Ok(p)) => {
                    self.turn.web_approved = true;
                    self.note(&format!(
                        "[jev] web gate: web is reasonable (p local {p:.2})"
                    ));
                }
                Some(Err(e)) => self.note(&format!("[jev] web gate unavailable: {e}")),
                None => {}
            }
        }
        let tools = self.tools.as_ref().expect("checked above");
        tools
            .call(&call.name, &call.arguments)
            .unwrap_or_else(|e| format!("error: {e}"))
    }

    /// Jev's checkpoint, every few calls: a note for the model to ride on the result.
    fn checkpoint(&mut self, backend: &mut dyn Backend) -> Option<String> {
        if !self.jev || self.turn.closed || self.turn.since_checkpoint < CHECKPOINT_EVERY {
            return None;
        }
        self.turn.since_checkpoint = 0;
        let judged = backend
            .judge(&self.turn.date)
            .map(|mut judge| jev::checkpoint(judge.as_mut(), &self.turn.request, &self.turn.work));
        let check = match judged? {
            Ok(check) => check,
            Err(e) => {
                self.note(&format!("[jev] checkpoint unavailable: {e}"));
                return None;
            }
        };
        self.note(&format!("[jev] {}", check.summary()));
        let flagged = check.p("stuck") >= 0.5 || check.repeating >= 0.7;
        if flagged && !self.turn.nudged {
            self.turn.nudged = true;
            Some(format!(
                "\n\n[Jev checkpoint: this looks stuck or repeating (stuck {:.2}, repeating \
                 {:.2}). Do not call tools you have already called; try a different approach, \
                 or answer with what you found and what blocks you.]",
                check.p("stuck"),
                check.repeating
            ))
        } else if flagged {
            self.turn.closed = true;
            Some(
                "\n\n[Jev checkpoint: still stuck. Tools are closed for this turn: answer now \
                 with what you found, what you changed and what blocks you.]"
                    .into(),
            )
        } else if check.p("needs-input") >= 0.6 && !self.turn.asked_jay {
            self.turn.asked_jay = true;
            Some("\n\n[Jev checkpoint: if you need Jay to decide something, ask him now.]".into())
        } else if check.p("complete") >= 0.7 && !self.turn.checked_done {
            self.turn.checked_done = true;
            Some(
                "\n\n[Jev checkpoint: this looks complete. If you changed files and have not \
                 checked them, do it now; then answer.]"
                    .into(),
            )
        } else {
            None
        }
    }

    /// The results as tokens within `room` positions (less the reply's reserve): long
    /// results are halved until they fit, and when nothing fits each says so.
    fn fit(&mut self, results: &mut [(Call, String)], room: usize) -> Result<Vec<u32>, String> {
        let room = room.saturating_sub(REPLY_RESERVE);
        loop {
            let tokens = self.template.results(self.turn.ended_by, results)?;
            if tokens.len() <= room {
                return Ok(tokens);
            }
            let mut cut = false;
            for (_, text) in results.iter_mut().filter(|(_, t)| t.chars().count() > 200) {
                let keep = text.chars().count() / 2;
                *text = format!(
                    "{}\n[truncated to fit the context]",
                    text.chars().take(keep).collect::<String>()
                );
                cut = true;
            }
            if !cut {
                break;
            }
        }
        for (_, text) in results.iter_mut() {
            *text = "error: the result does not fit in what is left of the context; answer with \
                     what you have"
                .into();
        }
        let tokens = self.template.results(self.turn.ended_by, results)?;
        if tokens.len() <= room {
            Ok(tokens)
        } else {
            Err("the context is full".into())
        }
    }

    /// When the chat ends: claims become handoffs on the board. Returns the files left
    /// uncommitted.
    pub fn finish(&mut self) -> Vec<(String, String)> {
        if self.handed_off {
            return Vec::new();
        }
        self.handed_off = true;
        match &self.edits {
            Some(edits) => workspace::hand_off(edits, &self.last_answer, self.verified.as_ref()),
            None => Vec::new(),
        }
    }
}

/// How a turn ended, for the terminal.
pub struct TurnEnd {
    pub read: Read,
    /// Why it ended without an answer.
    pub stopped: Option<String>,
}

/// One user message, blocking: the prompt, then replies and tool rounds until the turn
/// ends. Each generation is bounded by `limit` tokens; a reply looping on one block is
/// halted. A prompt that would leave no room for a reply is refused: `/undo` or
/// `/reset` make room (compaction through a handoff is step 3 of
/// `docs/AGENT_LOOP.md`).
///
/// # Errors
/// When the backend or the template fails; the turn is then ended in the history.
pub fn turn<T: Template>(
    agent: &mut Agent<'_, T>,
    backend: &mut dyn Backend,
    text: &str,
    limit: usize,
    cancel: &AtomicBool,
) -> Result<TurnEnd, String> {
    let start = backend.position();
    let prompt = agent.begin(text, start);
    let result = prompt.and_then(|prompt| run_turn(agent, backend, &prompt, limit, cancel));
    if let Err(e) = &result {
        agent.abandon(e);
    }
    result
}

fn run_turn<T: Template>(
    agent: &mut Agent<'_, T>,
    backend: &mut dyn Backend,
    prompt: &Rendered,
    limit: usize,
    cancel: &AtomicBool,
) -> Result<TurnEnd, String> {
    let started = Instant::now();
    let (tokens, held) = match prompt {
        Rendered::Full(tokens) => (tokens, 0),
        Rendered::Append(tokens) => (tokens, backend.position()),
    };
    if held + tokens.len() + REPLY_RESERVE > backend.capacity() {
        let why = format!(
            "the context is full ({} of {} positions, and this message needs {}); /undo or \
             /reset make room",
            held,
            backend.capacity(),
            tokens.len()
        );
        agent.note(&format!("[stopped: {why}]"));
        agent.finish_turn("", Some(&why));
        return Ok(TurnEnd {
            read: Read::default(),
            stopped: Some(why),
        });
    }
    match prompt {
        Rendered::Full(tokens) => backend.load(tokens)?,
        Rendered::Append(tokens) => backend.feed(tokens)?,
    }
    agent.observer.event(Event::Prompt {
        tokens: tokens.len(),
        seconds: started.elapsed().as_secs_f64(),
    });
    let mut seen = Vec::with_capacity(limit.min(4096));
    loop {
        let started = Instant::now();
        seen.clear();
        let stops = agent.stops().to_vec();
        let observer = &mut agent.observer;
        let (reply, ended) = backend.generate(limit, &stops, cancel, &mut |token| {
            observer.event(Event::Token(token));
            seen.push(token);
            guards::looping(&seen).is_none()
        })?;
        let room = backend.capacity().saturating_sub(backend.position());
        let mut next = agent.reply(&reply, ended, room, started.elapsed().as_secs_f64());
        if let Next::Tools(calls) = next {
            next = agent.run(&calls, backend, room);
        }
        match next {
            Next::Feed(tokens) => {
                let started = Instant::now();
                backend.feed(&tokens)?;
                agent.observer.event(Event::Prompt {
                    tokens: tokens.len(),
                    seconds: started.elapsed().as_secs_f64(),
                });
            }
            Next::Answer(read) => {
                return Ok(TurnEnd {
                    read,
                    stopped: None,
                })
            }
            Next::Stop { why, read } => {
                return Ok(TurnEnd {
                    read,
                    stopped: Some(why),
                })
            }
            Next::Tools(_) => unreachable!("run returns feed or stop"),
        }
    }
}

#[cfg(test)]
mod tests;
