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
pub mod eval;
pub mod evidence;
pub mod flow;
pub mod guards;
pub mod jev;
pub mod state;
pub mod transcript;
pub mod workspace;

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::edit_tools::EditSession;
use crate::system_one::LabelModel;
use crate::tools::Toolbox;
use clock::Now;
use evidence::{excerpt, Evidence};
use flow::Round;
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
    /// The context, as tokens (for a saved chat).
    fn held(&self) -> &[u32];
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
    /// The chat's state, to save for resuming ([`Agent::state`]).
    State(&'a Value),
    /// The turn is over (or paused): why, when it ended without an answer.
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
    /// Replace the context with these tokens (rebuilt from a handoff) and generate again.
    Load(Vec<u32>),
    /// Go back to this position (dropping a reply cut by the end of the context), feed
    /// these tokens and generate again.
    Retry { to: usize, feed: Vec<u32> },
    /// The turn waits for `/continue` or a new message.
    Pause(String),
    /// The turn is over with this answer.
    Answer(Read),
    /// The turn is over without a proper answer.
    Stop { why: String, read: Read },
}

/// Limits on the work one message leads to, checked after each reply before its calls
/// run; a spent budget pauses the turn (`/continue` gives a fresh one).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Budget {
    pub time: Option<Duration>,
    /// Tokens generated across the turn's replies.
    pub tokens: Option<usize>,
}

/// A turn paused (Ctrl-C, a spent budget, a reply's token limit), waiting for
/// `/continue` or a new message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pending {
    /// Calls not run yet, after the results of those that ran.
    Calls {
        done: Vec<(Call, String)>,
        remaining: Vec<Call>,
    },
    /// A reply cut short, already fed; `/continue` goes on writing it.
    Reply(Vec<u32>),
}

/// Where the backend is, for the agent's decisions about room.
#[derive(Clone, Copy, Debug)]
pub struct At {
    /// The position the last generation began at.
    pub start: usize,
    pub position: usize,
    pub capacity: usize,
}

/// Not run: answered for calls left waiting when Jay sends a new message.
const NOT_RUN: &str = "Not run: Jay paused this turn before this call ran and has sent a new \
message; nothing was done for it.";

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
    /// The budget's start and the tokens generated since.
    budget_from: Option<Instant>,
    budget_tokens: usize,
    /// A reply cut short and continued: its tokens so far.
    partial: Vec<u32>,
    /// The last reply as fed, without the token that ended it.
    reply: Vec<u32>,
    /// This turn's tool rounds, for a context rebuilt from a handoff.
    rounds: Vec<Round>,
    /// The next reply is a handoff.
    handoff: bool,
    /// Calls made since the last rebuild, and in the interval before it: a rebuild
    /// after an interval that found nothing new is a cycle.
    interval: Vec<(String, Value)>,
    previous_interval: Option<Vec<(String, Value)>>,
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
    budget: Budget,
    pending: Option<Pending>,
    /// Context flow (compaction through a handoff) is on.
    flow: bool,
    /// The context's length right after the last compaction.
    compacted: usize,
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
            budget: Budget::default(),
            pending: None,
            flow: true,
            compacted: 0,
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

    pub fn set_budget(&mut self, budget: Budget) {
        self.budget = budget;
    }

    /// Context flow (compaction through a handoff); on by default.
    pub fn set_flow(&mut self, on: bool) {
        self.flow = on;
    }

    /// The paused turn, if one waits for `/continue`.
    pub fn pending(&self) -> Option<&Pending> {
        self.pending.as_ref()
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
        // A paused turn is the newest; it has no exchange yet.
        if self.pending.take().is_some() {
            self.turn.request.clear();
            return Some(self.turn.start);
        }
        self.history.pop()?;
        self.starts.pop()
    }

    /// Starts the conversation over (receipts included); returns the context to keep,
    /// for [`Backend::truncate`]: the opening.
    pub fn reset(&mut self) -> usize {
        self.history.clear();
        self.starts.clear();
        self.pending = None;
        self.compacted = 0;
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
            budget_from: Some(Instant::now()),
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

    /// Reads a reply and decides what follows. `reply` holds the tokens this generation
    /// wrote; a continued reply's earlier part is kept by the agent. `seconds` is how long
    /// it took, for the observer.
    #[allow(clippy::too_many_lines)]
    pub fn reply(&mut self, reply: &[u32], ended: Ended, at: At, seconds: f64) -> Next {
        let earlier = std::mem::take(&mut self.turn.partial);
        let mut full = earlier.clone();
        full.extend_from_slice(reply);
        self.turn.ended_by = match ended {
            Ended::Stop => full.last().copied(),
            _ => None,
        };
        self.turn.reply = match ended {
            Ended::Stop => full[..full.len().saturating_sub(1)].to_vec(),
            _ => full.clone(),
        };
        let read = self.template.read(&full);
        self.turn.replies += 1;
        self.turn.tokens += reply.len();
        self.turn.budget_tokens += reply.len();
        self.observer.event(Event::Reply {
            read: &read,
            tokens: reply.len(),
            seconds,
            ended,
        });
        if std::mem::take(&mut self.turn.handoff) {
            return match ended {
                Ended::Stop | Ended::Limit => {
                    let space = if read.answer.starts_with(char::is_whitespace) {
                        ""
                    } else {
                        " "
                    };
                    let handoff = format!("{}{space}{}", flow::HANDOFF_OPENING, read.answer);
                    match self.rebuild(&handoff, at.capacity) {
                        Ok(tokens) => {
                            self.note(&format!(
                                "[context] rebuilt from a handoff ({} tokens): {} -> {} positions",
                                reply.len(),
                                at.position,
                                tokens.len()
                            ));
                            self.observer
                                .event(Event::Note(&format!("[handoff] {}", handoff.trim())));
                            Next::Load(tokens)
                        }
                        Err(e) => self.stop(&e, read),
                    }
                }
                _ => self.stop("the context is full and the handoff did not finish", read),
            };
        }
        match ended {
            Ended::Stop => {}
            Ended::Limit | Ended::Cancelled => {
                let why = if ended == Ended::Limit {
                    "the reply reached its token limit"
                } else {
                    "stopped by Ctrl-C"
                };
                self.pending = Some(Pending::Reply(full));
                return self.pause(why);
            }
            Ended::Halted => return self.stop("the reply was repeating one block", read),
            Ended::Context => {
                // Drop the cut reply and write it again in a context rebuilt from a handoff.
                let to = at.start.saturating_sub(earlier.len());
                if self.flow && to >= self.compacted + at.capacity / 8 {
                    if let Ok(feed) = self.handoff_request(None) {
                        if to + feed.len() + flow::handoff_limit(at.capacity) <= at.capacity {
                            self.turn.handoff = true;
                            self.note("[context] full: asking for a handoff");
                            return Next::Retry { to, feed };
                        }
                    }
                }
                return self.stop("the context is full; /undo or /reset make room", read);
            }
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
            let room = at.capacity.saturating_sub(at.position);
            match self.template.note(self.turn.ended_by, &note) {
                Ok(tokens) if tokens.len() + REPLY_RESERVE <= room => return Next::Feed(tokens),
                Ok(_) => self.note("[check] no room left for the check; the answer goes through"),
                Err(e) => self.note(&format!("[check] cannot write the note: {e}")),
            }
        }
        self.finish_turn(&read.answer, None);
        Next::Answer(read)
    }

    /// Pauses the turn (its pending work is already set).
    fn pause(&mut self, why: &str) -> Next {
        let seconds = self.turn.started.map_or(0.0, |t| t.elapsed().as_secs_f64());
        self.observer.event(Event::TurnEnd {
            stopped: Some(&format!("paused: {why}")),
            replies: self.turn.replies,
            tokens: self.turn.tokens,
            seconds,
        });
        Next::Pause(why.to_owned())
    }

    /// What the budget says, if it is spent.
    fn budget_spent(&self) -> Option<String> {
        let since = self
            .turn
            .budget_from
            .map_or(Duration::ZERO, |t| t.elapsed());
        if let Some(limit) = self.budget.time {
            if since >= limit {
                return Some(format!(
                    "the turn's {} minutes are spent",
                    limit.as_secs().div_ceil(60)
                ));
            }
        }
        if let Some(limit) = self.budget.tokens {
            if self.turn.budget_tokens >= limit {
                return Some(format!("the turn's {limit} tokens are spent"));
            }
        }
        None
    }

    /// Takes the paused turn's work, with a fresh budget, for `/continue`.
    pub fn take_pending(&mut self) -> Option<Pending> {
        let pending = self.pending.take()?;
        self.turn.budget_from = Some(Instant::now());
        self.turn.budget_tokens = 0;
        if self.turn.started.is_none() {
            self.turn.started = Some(Instant::now());
        }
        Some(pending)
    }

    /// Ends a paused turn because Jay sent a new message: waiting calls are answered as
    /// not run, so the history stays a well-formed conversation. Returns tokens to feed
    /// before the new message.
    ///
    /// # Errors
    /// When the template cannot write the results.
    pub fn settle(&mut self) -> Result<Option<Vec<u32>>, String> {
        match self.pending.take() {
            None => Ok(None),
            Some(Pending::Calls {
                mut done,
                remaining,
            }) => {
                done.extend(remaining.into_iter().map(|c| (c, NOT_RUN.to_owned())));
                let tokens = self.template.results(self.turn.ended_by, &done)?;
                self.finish_turn("", Some("paused; Jay sent a new message"));
                Ok(Some(tokens))
            }
            Some(Pending::Reply(tokens)) => {
                let read = self.template.read(&tokens);
                self.finish_turn(&read.answer, Some("paused; Jay sent a new message"));
                Ok(None)
            }
        }
    }

    /// The request for a handoff, the reply begun for the model.
    fn handoff_request(&self, ended_by: Option<u32>) -> Result<Vec<u32>, String> {
        let mut tokens = self.template.note(ended_by, flow::REQUEST)?;
        tokens.extend(self.template.answer_opening(flow::HANDOFF_OPENING)?);
        Ok(tokens)
    }

    /// The context rebuilt from `handoff`: the opening and this turn's message with a
    /// note holding the handoff and Jay's earlier messages, then this turn's newest tool
    /// rounds that fit an eighth of `capacity`.
    fn rebuild(&mut self, handoff: &str, capacity: usize) -> Result<Vec<u32>, String> {
        let now = clock::now();
        // Work since the last rebuild that adds nothing to the interval before it means
        // the model is going round: answer from what there is.
        let interval = std::mem::take(&mut self.turn.interval);
        let cycling = self
            .turn
            .previous_interval
            .as_ref()
            .is_some_and(|before| interval.iter().all(|k| before.contains(k)));
        self.turn.previous_interval = Some(interval);
        let evidence = if self.tools.is_some() {
            self.evidence.summary()
        } else {
            String::new()
        };
        // The note (handoff, Jay's messages, this turn's calls) stays within an eighth of
        // the context: fewer call lines until it does.
        let earlier = flow::earlier_messages(&self.history);
        let mut lines = flow::WORK_LINES.min(self.turn.work.len());
        let mut tokens = loop {
            let work = &self.turn.work[self.turn.work.len() - lines..];
            let mut about = flow::note(handoff, &earlier, work);
            if !self.about.is_empty() {
                about = format!("{}\n\n{about}", self.about);
            }
            let (Rendered::Full(tokens) | Rendered::Append(tokens)) =
                self.template.render(&Prompt {
                    now: &now,
                    instructions: &self.instructions,
                    about: &about,
                    evidence: &evidence,
                    tools: self.declaration.as_deref(),
                    history: &[],
                    user: &self.turn.request,
                    first: true,
                })?;
            if lines == 0 || tokens.len() <= self.opening + capacity / 8 {
                break tokens;
            }
            lines /= 2;
        };
        let mut kept = Vec::new();
        let mut used = 0;
        for round in self.turn.rounds.iter().rev() {
            let mut t = round.reply.clone();
            t.extend(self.template.results(round.ended_by, &round.results)?);
            if used + t.len() > capacity / 8 {
                break;
            }
            used += t.len();
            kept.push(t);
        }
        for t in kept.into_iter().rev() {
            tokens.extend(t);
        }
        if cycling {
            self.turn.closed = true;
            self.note("[context] the same work again after a handoff: tools closed");
            tokens.extend(self.template.answer_opening(ANSWER_OPENING)?);
        }
        if tokens.len() + REPLY_RESERVE > capacity {
            return Err("the context is full even after a handoff; /reset starts over".into());
        }
        // Earlier results are gone from the context: reads may be made again (an edit is
        // still never replayed).
        self.turn.earlier.retain(|(name, _)| guards::writes(name));
        self.turn.repeated_rounds = 0;
        // Earlier positions are gone: an undo now goes back to the opening.
        for start in &mut self.starts {
            *start = (*start).min(self.opening);
        }
        self.turn.start = self.opening;
        Ok(tokens)
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
        self.pending = None;
        self.turn.partial.clear();
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
    /// `done` holds results of calls that ran before a pause; `cancel` (Ctrl-C) pauses
    /// before the next call.
    #[allow(clippy::too_many_lines)]
    pub fn run(
        &mut self,
        done: Vec<(Call, String)>,
        calls: &[Call],
        backend: &mut dyn Backend,
        at: At,
        cancel: &AtomicBool,
    ) -> Next {
        if !self.turn.closed {
            if let Some(why) = self.budget_spent() {
                self.pending = Some(Pending::Calls {
                    done,
                    remaining: calls.to_vec(),
                });
                return self.pause(&why);
            }
        }
        let mut results: Vec<(Call, String)> = done;
        let mut repeats = 0;
        let mut write_limit = None;
        for (i, call) in calls.iter().enumerate() {
            if write_limit.is_none() && cancel.load(std::sync::atomic::Ordering::Relaxed) {
                self.pending = Some(Pending::Calls {
                    done: results,
                    remaining: calls[i..].to_vec(),
                });
                return self.pause("stopped by Ctrl-C");
            }
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
                if !self.turn.interval.contains(&key) {
                    self.turn.interval.push(key.clone());
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
        let room = at.capacity.saturating_sub(at.position);
        let mut tokens = match self.fit(&mut results, room) {
            Ok(tokens) => tokens,
            Err(why) => return self.stop(&why, Read::default()),
        };
        self.turn.rounds.push(Round {
            reply: self.turn.reply.clone(),
            ended_by: self.turn.ended_by,
            results: results.clone(),
        });
        if self.flow && flow::due(at.position, tokens.len(), at.capacity, self.compacted) {
            if let Ok(request) = self.handoff_request(self.turn.ended_by) {
                if at.position + request.len() + flow::handoff_limit(at.capacity) <= at.capacity {
                    self.turn.handoff = true;
                    self.note("[context] nearly full: asking for a handoff");
                    return Next::Feed(request);
                }
            }
        }
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

    /// The chat's state, to resume from: the backend's context and the agent's.
    pub fn state(&self, backend: &dyn Backend) -> Value {
        let turn = if self.pending.is_some() {
            json!({
                "request": self.turn.request,
                "date": self.turn.date,
                "start": self.turn.start,
                "ended_by": self.turn.ended_by,
                "reply": self.turn.reply,
                "rounds": self.turn.rounds.iter().map(state::round_to_json).collect::<Vec<_>>(),
                "unchecked": self.turn.unchecked,
                "wrote": self.turn.wrote,
            })
        } else {
            Value::Null
        };
        json!({
            "version": state::VERSION,
            "opening": self.opening,
            "compacted": self.compacted,
            "tokens": backend.held(),
            "history": state::history_to_json(&self.history),
            "starts": self.starts,
            "evidence": self.evidence.to_json(),
            "last_answer": self.last_answer,
            "pending": self.pending.as_ref().map(state::pending_to_json),
            "turn": turn,
        })
    }

    /// Tells the observer the state, for a saved chat.
    pub fn save(&mut self, backend: &dyn Backend) {
        let state = self.state(backend);
        self.observer.event(Event::State(&state));
    }

    /// Picks up a saved chat ([`Self::state`]): the backend reads its exact context (the
    /// opening first, so `/reset` returns to it) and the agent takes its history, receipts
    /// and paused turn. Use instead of [`Self::prepare`].
    ///
    /// # Errors
    /// When the snapshot is not one, is from another chat format version, or does not
    /// fit the backend's context.
    pub fn restore(&mut self, saved: &Value, backend: &mut dyn Backend) -> Result<(), String> {
        if saved["version"].as_u64() != Some(state::VERSION) {
            return Err(
                "this saved chat was written by the chat before the shared loop (or a newer \
                 one); it cannot be resumed here"
                    .into(),
            );
        }
        let tokens = state::tokens_from_json(&saved["tokens"])?;
        if tokens.len() + REPLY_RESERVE > backend.capacity() {
            return Err(format!(
                "the saved chat holds {} tokens; give a context of at least {}",
                tokens.len(),
                tokens.len() + REPLY_RESERVE
            ));
        }
        let number = |v: &Value| {
            v.as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .unwrap_or(0)
        };
        let opening = number(&saved["opening"]).min(tokens.len());
        if opening > 0 {
            backend.load(&tokens[..opening])?;
            if tokens.len() > opening {
                backend.feed(&tokens[opening..])?;
            }
        } else if !tokens.is_empty() {
            backend.load(&tokens)?;
        }
        self.opening = opening;
        self.compacted = number(&saved["compacted"]);
        self.history = state::history_from_json(&saved["history"]);
        self.starts = state::usize_list(&saved["starts"]);
        self.starts.resize(self.history.len(), opening);
        self.evidence = Evidence::from_json(&saved["evidence"]);
        self.last_answer = saved["last_answer"].as_str().unwrap_or_default().to_owned();
        self.pending = state::pending_from_json(&saved["pending"])?;
        let t = &saved["turn"];
        if self.pending.is_some() {
            self.turn = Turn {
                request: t["request"].as_str().unwrap_or_default().to_owned(),
                date: t["date"].as_str().unwrap_or_default().to_owned(),
                start: number(&t["start"]),
                ended_by: t["ended_by"].as_u64().and_then(|n| u32::try_from(n).ok()),
                reply: state::tokens_from_json(&t["reply"])?,
                rounds: t["rounds"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(state::round_from_json)
                    .collect::<Result<_, _>>()?,
                unchecked: t["unchecked"].as_bool().unwrap_or(false),
                wrote: t["wrote"].as_bool().unwrap_or(false),
                ..Turn::default()
            };
            if let Some(edits) = &self.edits {
                edits.set_task(&self.turn.request);
            }
        }
        Ok(())
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
    /// Why it paused (`/continue` goes on).
    pub paused: Option<String>,
}

/// One user message, blocking: the prompt, then replies and tool rounds until the turn
/// ends or pauses. Each generation is bounded by `limit` tokens; a reply looping on one
/// block is halted. A paused turn is ended first, its waiting calls answered as not run.
/// Past three quarters of the context, the model writes a handoff and the context is
/// rebuilt from it ([`flow`]).
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
    let result = begin_turn(agent, backend, text, limit, cancel);
    if let Err(e) = &result {
        agent.abandon(e);
        agent.save(backend);
    }
    result
}

/// `/continue`: goes on with a paused turn, with a fresh budget. `None` when no turn is
/// paused.
///
/// # Errors
/// As [`turn`].
pub fn resume<T: Template>(
    agent: &mut Agent<'_, T>,
    backend: &mut dyn Backend,
    limit: usize,
    cancel: &AtomicBool,
) -> Result<Option<TurnEnd>, String> {
    let step = match agent.take_pending() {
        None => return Ok(None),
        Some(Pending::Calls { done, remaining }) => Step::Calls {
            done,
            calls: remaining,
        },
        Some(Pending::Reply(tokens)) => {
            agent.turn.partial = tokens;
            Step::Generate
        }
    };
    let result = drive(agent, backend, limit, cancel, step);
    if let Err(e) = &result {
        agent.abandon(e);
        agent.save(backend);
    }
    result.map(Some)
}

fn begin_turn<T: Template>(
    agent: &mut Agent<'_, T>,
    backend: &mut dyn Backend,
    text: &str,
    limit: usize,
    cancel: &AtomicBool,
) -> Result<TurnEnd, String> {
    if let Some(tokens) = agent.settle()? {
        backend.feed(&tokens)?;
    }
    let started = Instant::now();
    let start = backend.position();
    let prompt = agent.begin(text, start)?;
    let capacity = backend.capacity();
    let (tokens, held) = match &prompt {
        Rendered::Full(tokens) => (tokens, 0),
        Rendered::Append(tokens) => (tokens, start),
    };
    if matches!(prompt, Rendered::Append(_))
        && agent.flow
        && !agent.history.is_empty()
        && flow::due(held, tokens.len(), capacity, agent.compacted)
    {
        // The handoff is written in the old context; the rebuilt one holds this message.
        let request = agent.handoff_request(None)?;
        if held + request.len() + flow::handoff_limit(capacity) <= capacity {
            agent.note("[context] a new message into a crowded context: asking for a handoff");
            backend.feed(&request)?;
            agent.turn.handoff = true;
            return drive(agent, backend, limit, cancel, Step::Generate);
        }
    }
    if held + tokens.len() + REPLY_RESERVE > capacity {
        let why = format!(
            "the context is full ({held} of {capacity} positions, and this message needs {}); \
             /undo or /reset make room",
            tokens.len()
        );
        agent.note(&format!("[stopped: {why}]"));
        agent.finish_turn("", Some(&why));
        agent.save(backend);
        return Ok(TurnEnd {
            read: Read::default(),
            stopped: Some(why),
            paused: None,
        });
    }
    match &prompt {
        Rendered::Full(tokens) => backend.load(tokens)?,
        Rendered::Append(tokens) => backend.feed(tokens)?,
    }
    agent.observer.event(Event::Prompt {
        tokens: tokens.len(),
        seconds: started.elapsed().as_secs_f64(),
    });
    drive(agent, backend, limit, cancel, Step::Generate)
}

/// What the driver does next.
enum Step {
    Generate,
    Calls {
        done: Vec<(Call, String)>,
        calls: Vec<Call>,
    },
}

fn drive<T: Template>(
    agent: &mut Agent<'_, T>,
    backend: &mut dyn Backend,
    limit: usize,
    cancel: &AtomicBool,
    mut step: Step,
) -> Result<TurnEnd, String> {
    let mut seen = Vec::with_capacity(limit.min(4096));
    loop {
        let capacity = backend.capacity();
        let next = match step {
            Step::Generate => {
                let started = Instant::now();
                let start = backend.position();
                let limit = if agent.turn.handoff {
                    flow::handoff_limit(capacity)
                } else {
                    limit
                };
                seen.clear();
                seen.extend_from_slice(&agent.turn.partial);
                let stops = agent.stops().to_vec();
                let observer = &mut agent.observer;
                let (reply, ended) = backend.generate(limit, &stops, cancel, &mut |token| {
                    observer.event(Event::Token(token));
                    seen.push(token);
                    guards::looping(&seen).is_none()
                })?;
                let at = At {
                    start,
                    position: backend.position(),
                    capacity,
                };
                agent.reply(&reply, ended, at, started.elapsed().as_secs_f64())
            }
            Step::Calls { done, calls } => {
                let at = At {
                    start: backend.position(),
                    position: backend.position(),
                    capacity,
                };
                agent.run(done, &calls, backend, at, cancel)
            }
        };
        step = match next {
            Next::Tools(calls) => Step::Calls {
                done: Vec::new(),
                calls,
            },
            Next::Feed(tokens) => {
                let started = Instant::now();
                backend.feed(&tokens)?;
                agent.observer.event(Event::Prompt {
                    tokens: tokens.len(),
                    seconds: started.elapsed().as_secs_f64(),
                });
                agent.save(backend);
                Step::Generate
            }
            Next::Load(tokens) => {
                let started = Instant::now();
                backend.load(&tokens)?;
                agent.compacted = backend.position();
                agent.observer.event(Event::Prompt {
                    tokens: tokens.len(),
                    seconds: started.elapsed().as_secs_f64(),
                });
                agent.save(backend);
                Step::Generate
            }
            Next::Retry { to, feed } => {
                backend.truncate(to)?;
                backend.feed(&feed)?;
                Step::Generate
            }
            Next::Pause(why) => {
                agent.save(backend);
                return Ok(TurnEnd {
                    read: Read::default(),
                    stopped: None,
                    paused: Some(why),
                });
            }
            Next::Answer(read) => {
                agent.save(backend);
                return Ok(TurnEnd {
                    read,
                    stopped: None,
                    paused: None,
                });
            }
            Next::Stop { why, read } => {
                agent.save(backend);
                return Ok(TurnEnd {
                    read,
                    stopped: Some(why),
                    paused: None,
                });
            }
        };
    }
}

#[cfg(test)]
mod tests;
