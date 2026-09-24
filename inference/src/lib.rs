//! Model-independent local inference session state. No network, model weights,
//! tokenizer, thread, timer, or implicit scheduler lives here.
//!
//! A backend supplies one next token, using the complete context. Backends may
//! cache internally, but must invalidate that cache after undo/reset. The caller
//! owns execution: a terminal can block; a GUI must submit bounded work through
//! its host proactor/offload path, never run inference in paint/input callbacks.
#![forbid(unsafe_code)]

#[cfg(feature = "cas")]
pub mod cas_tools;
pub mod compute;
pub mod tools;

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    EndToken,
    TokenLimit,
    ContextLimit,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Generation {
    /// Includes the terminating token, which is retained but not emitted.
    pub tokens: usize,
    pub reason: StopReason,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    InvalidLimit,
    ContextFull,
    TurnPending,
    NoPendingTurn,
    Backend(String),
    Output(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLimit => write!(f, "token limits must be positive"),
            Self::ContextFull => write!(f, "context limit reached; undo or reset the conversation"),
            Self::TurnPending => write!(f, "unfinished reply; continue or undo it first"),
            Self::NoPendingTurn => write!(f, "no unfinished reply to continue"),
            Self::Backend(e) => write!(f, "inference failed: {e}"),
            Self::Output(e) => write!(f, "output failed: {e}"),
        }
    }
}
impl std::error::Error for Error {}

/// Exact token history, including generated structure/thinking and stop tokens.
/// Truncated/cancelled replies stay pending: never silently turn an incomplete
/// assistant message into a completed one, or discard history to fit a limit.
#[derive(Debug)]
pub struct Session {
    tokens: Vec<u32>,
    turns: Vec<usize>,
    pending: bool,
    max_context: usize,
}

impl Session {
    pub fn new(max_context: usize) -> Result<Self, Error> {
        if max_context == 0 {
            return Err(Error::InvalidLimit);
        }
        Ok(Self {
            tokens: Vec::new(),
            turns: Vec::new(),
            pending: false,
            max_context,
        })
    }

    pub fn tokens(&self) -> &[u32] {
        &self.tokens
    }
    pub fn is_pending(&self) -> bool {
        self.pending
    }
    pub fn max_context(&self) -> usize {
        self.max_context
    }

    /// Append a backend-rendered user message and generation prefix atomically.
    /// Reserve at least one position for generation before accepting the turn.
    pub fn begin_turn(&mut self, prompt: &[u32]) -> Result<(), Error> {
        if self.pending {
            return Err(Error::TurnPending);
        }
        if prompt.is_empty() || prompt.len() >= self.max_context - self.tokens.len() {
            return Err(Error::ContextFull);
        }
        self.turns.push(self.tokens.len());
        self.tokens.extend_from_slice(prompt);
        self.pending = true;
        Ok(())
    }

    /// Remove the most recent user/assistant turn (including a partial reply).
    pub fn undo(&mut self) -> bool {
        let Some(start) = self.turns.pop() else {
            return false;
        };
        self.tokens.truncate(start);
        self.pending = false;
        true
    }

    pub fn reset(&mut self) {
        self.tokens.clear();
        self.turns.clear();
        self.pending = false;
    }

    /// Synchronous bounded generation, with natural backpressure from `emit`.
    /// Cancellation is checked before/after each backend call. Slow backends
    /// should also consult the same flag at their own safe checkpoints.
    /// On error all successfully appended tokens remain available for retry.
    pub fn generate(
        &mut self,
        max_tokens: usize,
        stop_tokens: &[u32],
        cancel: &AtomicBool,
        mut next: impl FnMut(&[u32]) -> Result<u32, String>,
        mut emit: impl FnMut(u32) -> Result<(), String>,
    ) -> Result<Generation, Error> {
        if max_tokens == 0 {
            return Err(Error::InvalidLimit);
        }
        if !self.pending {
            return Err(Error::NoPendingTurn);
        }
        let mut count = 0;
        let reason = loop {
            if cancel.load(Ordering::Relaxed) {
                break StopReason::Cancelled;
            }
            if count == max_tokens {
                break StopReason::TokenLimit;
            }
            if self.tokens.len() == self.max_context {
                break StopReason::ContextLimit;
            }
            let result = next(&self.tokens);
            if cancel.load(Ordering::Relaxed) {
                break StopReason::Cancelled;
            }
            let token = result.map_err(Error::Backend)?;
            self.tokens.push(token);
            count += 1;
            if stop_tokens.contains(&token) {
                self.pending = false;
                break StopReason::EndToken;
            }
            emit(token).map_err(Error::Output)?;
        };
        Ok(Generation {
            tokens: count,
            reason,
        })
    }
}

/// Streaming UTF-8 decoding. Incomplete characters survive token boundaries;
/// only genuinely malformed bytes (or an incomplete tail at finish) become U+FFFD.
#[derive(Debug, Default)]
pub struct Utf8Stream {
    tail: Vec<u8>,
}

impl Utf8Stream {
    pub fn push(&mut self, bytes: &[u8]) -> String {
        self.tail.extend_from_slice(bytes);
        let mut out = String::new();
        let mut used = 0;
        while used < self.tail.len() {
            match std::str::from_utf8(&self.tail[used..]) {
                Ok(text) => {
                    out.push_str(text);
                    used = self.tail.len();
                }
                Err(error) => {
                    let valid = used + error.valid_up_to();
                    out.push_str(
                        std::str::from_utf8(&self.tail[used..valid]).expect("validated prefix"),
                    );
                    used = valid;
                    match error.error_len() {
                        Some(len) => {
                            out.push('\u{fffd}');
                            used += len;
                        }
                        None => break,
                    }
                }
            }
        }
        self.tail.drain(..used);
        out
    }

    pub fn finish(&mut self) -> String {
        let out = String::from_utf8_lossy(&self.tail).into_owned();
        self.tail.clear();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follow_up_has_exact_first_turn_including_end_token() {
        let cancel = AtomicBool::new(false);
        let mut s = Session::new(20).unwrap();
        s.begin_turn(&[1, 2]).unwrap();
        let mut output = Vec::new();
        let mut replies = [3, 99].into_iter();
        let g = s
            .generate(
                4,
                &[99],
                &cancel,
                |_| Ok(replies.next().unwrap()),
                |t| {
                    output.push(t);
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(g.reason, StopReason::EndToken);
        assert_eq!(output, [3]);
        s.begin_turn(&[4]).unwrap();
        s.generate(
            1,
            &[99],
            &cancel,
            |ctx| {
                assert_eq!(ctx, [1, 2, 3, 99, 4]);
                Ok(99)
            },
            |_| Ok(()),
        )
        .unwrap();
        assert!(s.undo());
        assert_eq!(s.tokens(), [1, 2, 3, 99]);
        s.reset();
        assert!(s.tokens().is_empty());
        assert!(!s.is_pending());
    }

    #[test]
    fn truncated_reply_must_continue_or_be_undone() {
        let mut s = Session::new(10).unwrap();
        s.begin_turn(&[1]).unwrap();
        let cancel = AtomicBool::new(false);
        assert_eq!(
            s.generate(1, &[99], &cancel, |_| Ok(2), |_| Ok(()))
                .unwrap()
                .reason,
            StopReason::TokenLimit
        );
        assert_eq!(s.begin_turn(&[3]), Err(Error::TurnPending));
        s.generate(
            1,
            &[99],
            &cancel,
            |ctx| {
                assert_eq!(ctx, [1, 2]);
                Ok(99)
            },
            |_| Ok(()),
        )
        .unwrap();
        assert!(!s.is_pending());
    }

    #[test]
    fn context_overflow_is_atomic_and_generation_is_bounded() {
        let mut s = Session::new(3).unwrap();
        assert_eq!(s.begin_turn(&[1, 2, 3]), Err(Error::ContextFull));
        assert!(s.tokens().is_empty());
        s.begin_turn(&[1, 2]).unwrap();
        assert_eq!(
            s.generate(10, &[99], &AtomicBool::new(false), |_| Ok(3), |_| Ok(()))
                .unwrap()
                .reason,
            StopReason::ContextLimit
        );
        assert_eq!(s.tokens(), [1, 2, 3]);
    }

    #[test]
    fn cancellation_before_or_during_backend_does_not_append_a_token() {
        let cancel = AtomicBool::new(true);
        let mut s = Session::new(4).unwrap();
        s.begin_turn(&[1]).unwrap();
        let g = s
            .generate(
                1,
                &[],
                &cancel,
                |_| panic!("must not run"),
                |_| panic!("must not emit"),
            )
            .unwrap();
        assert_eq!(g.reason, StopReason::Cancelled);
        cancel.store(false, Ordering::Relaxed);
        let g = s
            .generate(
                1,
                &[],
                &cancel,
                |_| {
                    cancel.store(true, Ordering::Relaxed);
                    Err("cancelled".into())
                },
                |_| panic!("must not emit"),
            )
            .unwrap();
        assert_eq!(g.reason, StopReason::Cancelled);
        assert_eq!(s.tokens(), [1]);
        assert!(s.is_pending());
    }

    #[test]
    fn failure_can_be_retried_and_output_failure_is_not_silenced() {
        let mut s = Session::new(4).unwrap();
        s.begin_turn(&[1]).unwrap();
        let cancel = AtomicBool::new(false);
        assert_eq!(
            s.generate(1, &[], &cancel, |_| Err("disk".into()), |_| Ok(())),
            Err(Error::Backend("disk".into()))
        );
        assert_eq!(s.tokens(), [1]);
        assert_eq!(
            s.generate(1, &[], &cancel, |_| Ok(2), |_| Err("pipe".into())),
            Err(Error::Output("pipe".into()))
        );
        assert_eq!(s.tokens(), [1, 2]);
    }

    #[test]
    fn utf8_survives_every_split_and_handles_bad_bytes() {
        let text = "สวัสดี 🌱 你好";
        for split in 0..=text.len() {
            let mut decoder = Utf8Stream::default();
            let mut got = decoder.push(&text.as_bytes()[..split]);
            got.push_str(&decoder.push(&text.as_bytes()[split..]));
            got.push_str(&decoder.finish());
            assert_eq!(got, text);
        }
        let mut decoder = Utf8Stream::default();
        assert_eq!(decoder.push(&[0xff, b'a', 0xf0]), "�a");
        assert_eq!(decoder.finish(), "�");
    }
}
