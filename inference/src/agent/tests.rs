//! The turn loop on a toy chat format and scripted replies: no model, no files.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::AtomicBool;

use serde_json::{json, Value};

use super::*;
use crate::tools::Tool;

// The toy format: text is one token per character (code + 1000); these are controls.
const USER: u32 = 1;
const REPLY: u32 = 2;
const DONE: u32 = 3;
const CALL: u32 = 4;
const RESULT: u32 = 5;
const NOTE: u32 = 6;
const ANSWER: u32 = 7;

fn text(s: &str) -> Vec<u32> {
    s.chars().map(|c| c as u32 + 1000).collect()
}

fn untext(tokens: &[u32]) -> String {
    tokens
        .iter()
        .filter(|&&t| t >= 1000)
        .map(|&t| char::from_u32(t - 1000).unwrap())
        .collect()
}

struct Toy;

impl Template for Toy {
    fn render(&self, p: &Prompt<'_>) -> Result<Rendered, String> {
        let mut out = Vec::new();
        for e in p.history {
            out.push(USER);
            out.extend(text(&e.user));
            out.push(REPLY);
            out.extend(text(&e.answer));
            out.push(DONE);
        }
        out.push(USER);
        out.extend(text(p.user));
        out.push(REPLY);
        Ok(Rendered::Full(out))
    }
    fn stops(&self) -> &[u32] {
        &[DONE, CALL]
    }
    /// A call is `name args` before CALL; anything else is the answer.
    fn read(&self, reply: &[u32]) -> Read {
        let body = untext(reply);
        if reply.last() == Some(&CALL) {
            let (name, arguments) = body.split_once(' ').unwrap_or((&body, "{}"));
            return Read {
                calls: vec![Call {
                    id: None,
                    name: name.into(),
                    arguments: arguments.into(),
                }],
                ..Read::default()
            };
        }
        Read {
            answer: body,
            complete: reply.last() == Some(&DONE),
            ..Read::default()
        }
    }
    fn results(
        &self,
        ended_by: Option<u32>,
        results: &[(Call, String)],
    ) -> Result<Vec<u32>, String> {
        let mut out: Vec<u32> = ended_by.into_iter().collect();
        for (_, r) in results {
            out.push(RESULT);
            out.extend(text(r));
        }
        out.push(REPLY);
        Ok(out)
    }
    fn note(&self, ended_by: Option<u32>, note: &str) -> Result<Vec<u32>, String> {
        let mut out: Vec<u32> = ended_by.into_iter().collect();
        out.push(NOTE);
        out.extend(text(note));
        out.push(REPLY);
        Ok(out)
    }
    fn answer_opening(&self, opening: &str) -> Result<Vec<u32>, String> {
        let mut out = vec![ANSWER];
        out.extend(text(opening));
        Ok(out)
    }
}

/// Replies given in order; records everything fed.
struct Scripted {
    replies: VecDeque<Vec<u32>>,
    held: Vec<u32>,
    capacity: usize,
    judge: Option<Vec<Vec<f32>>>,
    fail_next: bool,
}

impl Scripted {
    fn new(replies: Vec<Vec<u32>>) -> Self {
        Self {
            replies: replies.into(),
            held: Vec::new(),
            capacity: 100_000,
            judge: None,
            fail_next: false,
        }
    }
    fn held_text(&self) -> String {
        untext(&self.held)
    }
}

struct Labels<'a>(&'a mut Vec<Vec<f32>>);
impl LabelModel for Labels<'_> {
    fn label_logits(&mut self, _: &str, _: &str, labels: &[String]) -> Result<Vec<f32>, String> {
        let next = self.0.remove(0);
        assert_eq!(next.len(), labels.len());
        Ok(next)
    }
}

impl Backend for Scripted {
    fn load(&mut self, tokens: &[u32]) -> Result<(), String> {
        self.held = tokens.to_vec();
        Ok(())
    }
    fn feed(&mut self, tokens: &[u32]) -> Result<(), String> {
        if std::mem::take(&mut self.fail_next) {
            return Err("the GPU went away".into());
        }
        self.held.extend_from_slice(tokens);
        Ok(())
    }
    fn generate(
        &mut self,
        limit: usize,
        stops: &[u32],
        _cancel: &AtomicBool,
        emit: &mut dyn FnMut(u32) -> bool,
    ) -> Result<(Vec<u32>, Ended), String> {
        let script = self.replies.pop_front().expect("a scripted reply");
        let mut out = Vec::new();
        for t in script {
            if self.held.len() + 1 >= self.capacity {
                return Ok((out, Ended::Context));
            }
            out.push(t);
            if stops.contains(&t) {
                return Ok((out, Ended::Stop));
            }
            self.held.push(t);
            if !emit(t) {
                return Ok((out, Ended::Halted));
            }
            if out.len() == limit {
                return Ok((out, Ended::Limit));
            }
        }
        Ok((out, Ended::Limit))
    }
    fn truncate(&mut self, len: usize) -> Result<(), String> {
        if self.fail_next {
            return Err("disk".into());
        }
        self.held.truncate(len);
        Ok(())
    }
    fn held(&self) -> &[u32] {
        &self.held
    }
    fn position(&self) -> usize {
        self.held.len()
    }
    fn capacity(&self) -> usize {
        self.capacity
    }
    fn judge(&mut self, _date: &str) -> Option<Box<dyn LabelModel + '_>> {
        self.judge
            .as_mut()
            .map(|j| Box::new(Labels(j)) as Box<dyn LabelModel>)
    }
}

/// A tool answering from a closure, counting its calls.
struct Fake {
    name: &'static str,
    calls: Rc<RefCell<usize>>,
    answer: fn(&Value, usize) -> Result<String, String>,
}

impl Tool for Fake {
    fn name(&self) -> &'static str {
        self.name
    }
    fn description(&self) -> &'static str {
        "test tool"
    }
    fn parameters(&self) -> Value {
        json!({"type": "object"})
    }
    fn call(&self, args: &Value) -> Result<String, String> {
        *self.calls.borrow_mut() += 1;
        (self.answer)(args, *self.calls.borrow())
    }
}

fn fake(
    name: &'static str,
    answer: fn(&Value, usize) -> Result<String, String>,
) -> (Box<dyn Tool>, Rc<RefCell<usize>>) {
    let calls = Rc::new(RefCell::new(0));
    (
        Box::new(Fake {
            name,
            calls: Rc::clone(&calls),
            answer,
        }),
        calls,
    )
}

#[derive(Clone, Default)]
struct Notes(Rc<RefCell<Vec<String>>>);
impl Observer for Notes {
    fn event(&mut self, event: Event<'_>) {
        if let Event::Note(n) = event {
            self.0.borrow_mut().push(n.to_owned());
        }
    }
}

fn agent(tools: Vec<Box<dyn Tool>>, jev: bool) -> (Agent<'static, Toy>, Notes) {
    let mut toolbox = Toolbox::default();
    for t in tools {
        toolbox.push(t);
    }
    let notes = Notes::default();
    let workspace = Workspace {
        tools: toolbox,
        edits: None,
        instructions: "test".into(),
        base: PathBuf::from("."),
        described: Vec::new(),
    };
    (
        Agent::new(Toy, Some(workspace), jev, Box::new(notes.clone())),
        notes,
    )
}

fn call(s: &str) -> Vec<u32> {
    let mut t = text(s);
    t.push(CALL);
    t
}

fn answer(s: &str) -> Vec<u32> {
    let mut t = text(s);
    t.push(DONE);
    t
}

static NO: AtomicBool = AtomicBool::new(false);

#[test]
fn a_tool_round_then_an_answer_and_the_next_turn_sees_the_exchange() {
    let (read, reads) = fake("fs_read", |_, _| Ok("fn main() {}".into()));
    let (mut a, _) = agent(vec![read], false);
    let mut b = Scripted::new(vec![
        call(r#"fs_read {"path":"a.rs"}"#),
        answer("a.rs has main"),
        answer("yes"),
    ]);
    let end = turn(&mut a, &mut b, "what is in a.rs?", 100, &NO).unwrap();
    assert_eq!(end.read.answer, "a.rs has main");
    assert_eq!(end.stopped, None);
    assert_eq!(*reads.borrow(), 1);
    // The call's ending token was fed with its result.
    let held = b.held.clone();
    let at = held.iter().position(|&t| t == CALL).unwrap();
    assert_eq!(held[at + 1], RESULT);
    assert!(b.held_text().contains("fn main() {}"));
    turn(&mut a, &mut b, "sure?", 100, &NO).unwrap();
    assert_eq!(
        a.history(),
        [
            Exchange {
                user: "what is in a.rs?".into(),
                answer: "a.rs has main".into()
            },
            Exchange {
                user: "sure?".into(),
                answer: "yes".into()
            }
        ]
    );
    // The second prompt is the whole conversation again (a Full template).
    assert!(b
        .held_text()
        .starts_with("what is in a.rs?a.rs has mainsure?"));
    // A format that renders everything reloads next turn, whatever is truncated.
    assert!(a.undo().is_some());
    assert_eq!(a.history().len(), 1);
}

#[test]
fn a_repeated_call_is_not_run_and_a_second_repeated_round_closes_the_tools() {
    let (read, reads) = fake("fs_read", |_, _| Ok("contents".into()));
    let (mut a, notes) = agent(vec![read], false);
    let same = r#"fs_read {"path":"a.rs"}"#;
    let mut b = Scripted::new(vec![
        call(same),
        call(r#"fs_read { "path": "a.rs" }"#),
        call(same),
        answer("a.rs holds contents"),
    ]);
    let end = turn(&mut a, &mut b, "read a.rs", 100, &NO).unwrap();
    assert_eq!(*reads.borrow(), 1, "the call runs once");
    assert_eq!(end.read.answer, "a.rs holds contents");
    let held = b.held_text();
    assert!(held.contains("Not run: you already made this exact call"));
    assert!(held.contains("Your tools are closed for this turn"));
    // The answer was begun for the model.
    assert!(b.held.contains(&ANSWER));
    assert!(held.contains(ANSWER_OPENING.trim()));
    assert!(notes
        .0
        .borrow()
        .iter()
        .any(|n| n.starts_with("[tools closed")));

    // A call after the tools closed ends the turn.
    let (read, _) = fake("fs_read", |_, _| Ok("contents".into()));
    let (mut a, _) = agent(vec![read], false);
    let mut b = Scripted::new(vec![call(same), call(same), call(same), call(same)]);
    let end = turn(&mut a, &mut b, "read a.rs", 100, &NO).unwrap();
    assert_eq!(
        end.stopped.as_deref(),
        Some("tools were called after they were closed")
    );
}

#[test]
fn a_read_may_be_made_again_after_a_successful_edit() {
    let (read, reads) = fake("fs_read", |_, _| Ok("contents".into()));
    let (edit, _) = fake("text_edit", |_, n| {
        Ok(format!("edited a.rs: revision {n:016}; lines 1-2"))
    });
    let (mut a, _) = agent(vec![read, edit], false);
    let r = r#"fs_read {"path":"a.rs"}"#;
    let e = r#"text_edit {"path":"a.rs","old_text":"a","new_text":"b"}"#;
    let mut b = Scripted::new(vec![
        call(r),
        call(e),
        call(r),
        call(e),
        answer("it is done"),
        answer("done"),
    ]);
    let end = turn(&mut a, &mut b, "edit a.rs", 100, &NO).unwrap();
    assert_eq!(*reads.borrow(), 2);
    // The same edit is never replayed.
    assert_eq!(
        b.held_text().matches("Not run: you already made").count(),
        1
    );
    // Files changed and no cargo run passed: the answer waited for a check, once.
    assert!(b.held_text().contains("Automatic check before your answer"));
    assert!(b.held.contains(&NOTE));
    assert_eq!(end.read.answer, "done");
}

#[test]
fn the_verification_note_is_sent_once_and_a_passing_cargo_run_clears_it() {
    let (edit, _) = fake("text_edit", |_, _| {
        Ok("edited a.rs: revision 1111111111111111; x".into())
    });
    let (cargo, _) = fake("cargo", |_, _| Ok("cargo test succeeded (exit 0)".into()));
    let (mut a, _) = agent(vec![edit, cargo], false);
    let mut b = Scripted::new(vec![
        call(r#"text_edit {"path":"a.rs"}"#),
        answer("it compiles"),
        call(r#"cargo {"command":"test"}"#),
        answer("tests pass"),
    ]);
    let end = turn(&mut a, &mut b, "fix a.rs", 100, &NO).unwrap();
    assert_eq!(end.read.answer, "tests pass");
    assert_eq!(b.held_text().matches("Automatic check").count(), 1);

    let (edit, _) = fake("text_edit", |_, _| Ok("edited".into()));
    let (cargo, _) = fake("cargo", |_, _| Ok("cargo test succeeded (exit 0)".into()));
    let (mut a, _) = agent(vec![edit, cargo], false);
    let mut b = Scripted::new(vec![
        call(r#"text_edit {"path":"a.rs"}"#),
        call(r#"cargo {"command":"test"}"#),
        answer("tests pass"),
    ]);
    turn(&mut a, &mut b, "fix a.rs", 100, &NO).unwrap();
    assert!(!b.held_text().contains("Automatic check"));
}

#[test]
fn a_write_failing_twice_the_same_way_ends_the_turn_and_three_failures_get_a_note() {
    let (write, _) = fake("text_edit", |_, _| Err("old_text not found".into()));
    let (mut a, _) = agent(vec![write], false);
    let mut b = Scripted::new(vec![
        call(r#"text_edit {"path":"a.rs","old_text":"x"}"#),
        call(r#"text_edit {"path":"a.rs","old_text":"x"}"#),
    ]);
    let end = turn(&mut a, &mut b, "edit", 100, &NO).unwrap();
    assert!(end
        .stopped
        .unwrap()
        .starts_with("the same write failed twice with the same error"));

    let (write, _) = fake("text_edit", |args, _| {
        Err(format!("{} not found", args["old_text"]))
    });
    let (mut a, _) = agent(vec![write], false);
    let mut script: Vec<Vec<u32>> = (0..5)
        .map(|i| call(&format!(r#"text_edit {{"path":"a.rs","old_text":"{i}"}}"#)))
        .collect();
    script.push(answer("unused"));
    let mut b = Scripted::new(script);
    let end = turn(&mut a, &mut b, "edit", 100, &NO).unwrap();
    assert!(b
        .held_text()
        .contains("text_edit has failed 3 times in a row"));
    assert!(end
        .stopped
        .unwrap()
        .starts_with("five writes failed without one succeeding"));
}

#[test]
fn results_are_cut_to_fit_and_a_full_context_ends_the_turn() {
    let (read, _) = fake("fs_read", |_, _| Ok("x".repeat(5_000)));
    let (mut a, _) = agent(vec![read], false);
    let mut b = Scripted::new(vec![call(r#"fs_read {"path":"big"}"#), answer("big")]);
    b.capacity = 3_000;
    let end = turn(&mut a, &mut b, "read big", 100, &NO).unwrap();
    assert_eq!(end.read.answer, "big");
    assert!(b.held_text().contains("[truncated to fit the context]"));
    assert!(b.held.len() <= 3_000);

    let (read, _) = fake("fs_read", |_, _| Ok("x".repeat(5_000)));
    let (mut a, _) = agent(vec![read], false);
    let mut b = Scripted::new(vec![call(r#"fs_read {"path":"big"}"#)]);
    b.capacity = 560;
    let end = turn(&mut a, &mut b, "read big", 100, &NO).unwrap();
    assert_eq!(end.stopped.as_deref(), Some("the context is full"));
}

#[test]
fn a_looping_reply_is_halted() {
    let (mut a, _) = agent(Vec::new(), false);
    let looping: Vec<u32> = std::iter::repeat_n(text("the same line. "), 10)
        .flatten()
        .collect();
    let mut b = Scripted::new(vec![looping]);
    let end = turn(&mut a, &mut b, "hi", 1_000, &NO).unwrap();
    assert_eq!(
        end.stopped.as_deref(),
        Some("the reply was repeating one block")
    );
}

#[test]
fn jev_nudges_then_closes_a_stuck_turn() {
    let (read, _) = fake("fs_read", |_, n| Ok(format!("read {n}")));
    let (mut a, notes) = agent(vec![read], true);
    // Twelve different reads; two checkpoints, both "stuck".
    let mut script: Vec<Vec<u32>> = (0..12)
        .map(|i| call(&format!(r#"fs_read {{"path":"{i}.rs"}}"#)))
        .collect();
    script.push(answer("I could not find it"));
    let mut b = Scripted::new(script);
    // state: in-progress, needs-input, complete, stuck; repeating: true, false.
    let stuck = vec![0.0, 0.0, 0.0, 5.0];
    let not_repeating = vec![0.0, 5.0];
    b.judge = Some(vec![
        stuck.clone(),
        not_repeating.clone(),
        stuck,
        not_repeating,
    ]);
    let end = turn(&mut a, &mut b, "find it", 100, &NO).unwrap();
    assert_eq!(end.read.answer, "I could not find it");
    let held = b.held_text();
    assert!(held.contains("Jev checkpoint: this looks stuck or repeating"));
    assert!(held.contains("Jev checkpoint: still stuck. Tools are closed"));
    assert!(b.held.contains(&ANSWER));
    assert_eq!(
        notes
            .0
            .borrow()
            .iter()
            .filter(|n| n.starts_with("[jev] state stuck"))
            .count(),
        2
    );
}

#[test]
fn the_archive_note_asks_for_scope_once_before_an_answer() {
    let (find, _) = fake("cas_find", |_, _| {
        Ok("Scope: file paths only\n0 matches".into())
    });
    let (mut a, _) = agent(vec![find], false);
    let mut b = Scripted::new(vec![
        call(r#"cas_find {"archive":"a","pattern":"*x*"}"#),
        answer("there is no x anywhere"),
        answer("x is not in a's file names; contents not searched"),
    ]);
    let end = turn(&mut a, &mut b, "is x in the archives?", 100, &NO).unwrap();
    assert_eq!(
        end.read.answer,
        "x is not in a's file names; contents not searched"
    );
    assert_eq!(b.held_text().matches("Archive coverage check").count(), 1);
}

#[test]
fn notes_each_turn_carry_the_receipts_and_facts_about_the_chat() {
    struct Seen(Rc<RefCell<String>>);
    impl Template for Seen {
        fn render(&self, p: &Prompt<'_>) -> Result<Rendered, String> {
            *self.0.borrow_mut() = format!("{}\n{}\n{}", p.now.said, p.about, p.evidence);
            Toy.render(p)
        }
        fn stops(&self) -> &[u32] {
            Toy.stops()
        }
        fn read(&self, reply: &[u32]) -> Read {
            Toy.read(reply)
        }
        fn results(&self, e: Option<u32>, r: &[(Call, String)]) -> Result<Vec<u32>, String> {
            Toy.results(e, r)
        }
        fn note(&self, e: Option<u32>, t: &str) -> Result<Vec<u32>, String> {
            Toy.note(e, t)
        }
        fn answer_opening(&self, t: &str) -> Result<Vec<u32>, String> {
            Toy.answer_opening(t)
        }
    }
    let seen = Rc::new(RefCell::new(String::new()));
    let (read, _) = fake("fs_grep", |_, _| Ok("0 matches".into()));
    let mut tools = Toolbox::default();
    tools.push(read);
    let workspace = Workspace {
        tools,
        edits: None,
        instructions: String::new(),
        base: PathBuf::from("."),
        described: Vec::new(),
    };
    let mut a = Agent::new(
        Seen(Rc::clone(&seen)),
        Some(workspace),
        false,
        Box::new(Quiet),
    );
    a.set_about("This chat runs gpt-oss-20b; its last reply ran at 57 tokens/s.");
    let mut b = Scripted::new(vec![
        call(r#"fs_grep {"pattern":"x"}"#),
        answer("none"),
        answer("ok"),
    ]);
    turn(&mut a, &mut b, "find x", 100, &NO).unwrap();
    assert!(seen.borrow().contains("No tool calls recorded"));
    turn(&mut a, &mut b, "again", 100, &NO).unwrap();
    let notes = seen.borrow();
    assert!(notes.starts_with("It is now "));
    assert!(notes.contains("57 tokens/s"));
    assert!(notes.contains("Turn 1, fs_grep"));
}

/// The toy format, appending: an opening read once, each turn closing the last reply.
struct Appending;
const OPEN: u32 = 8;

impl Template for Appending {
    fn render(&self, p: &Prompt<'_>) -> Result<Rendered, String> {
        let mut out = Vec::new();
        if p.first {
            out = self.opening(p.instructions, p.tools)?;
        }
        if !p.history.is_empty() {
            out.push(DONE);
        }
        out.extend(text(p.about));
        out.push(USER);
        out.extend(text(p.user));
        out.push(REPLY);
        Ok(Rendered::Append(out))
    }
    fn opening(&self, instructions: &str, _tools: Option<&str>) -> Result<Vec<u32>, String> {
        let mut out = vec![OPEN];
        out.extend(text(instructions));
        Ok(out)
    }
    fn stops(&self) -> &[u32] {
        Toy.stops()
    }
    fn read(&self, reply: &[u32]) -> Read {
        Toy.read(reply)
    }
    fn results(&self, e: Option<u32>, r: &[(Call, String)]) -> Result<Vec<u32>, String> {
        Toy.results(e, r)
    }
    fn note(&self, e: Option<u32>, t: &str) -> Result<Vec<u32>, String> {
        Toy.note(e, t)
    }
    fn answer_opening(&self, t: &str) -> Result<Vec<u32>, String> {
        Toy.answer_opening(t)
    }
}

fn appending() -> Agent<'static, Appending> {
    let workspace = Workspace {
        tools: Toolbox::default(),
        edits: None,
        instructions: "rules".into(),
        base: PathBuf::from("."),
        described: Vec::new(),
    };
    Agent::new(Appending, Some(workspace), false, Box::new(Quiet))
}

#[test]
fn an_appending_format_reads_its_opening_once_and_undo_and_reset_truncate_to_it() {
    let mut a = appending();
    let mut b = Scripted::new(vec![answer("one"), answer("two"), answer("again")]);
    assert_eq!(a.prepare(&mut b).unwrap(), 6);
    assert_eq!(b.held_text(), "rules");
    turn(&mut a, &mut b, "a", 100, &NO).unwrap();
    let after_first = b.held.len();
    turn(&mut a, &mut b, "b", 100, &NO).unwrap();
    // The opening is not repeated; the first reply was closed before the second message.
    assert_eq!(b.held.iter().filter(|&&t| t == OPEN).count(), 1);
    assert_eq!(b.held[after_first], DONE);
    assert_eq!(b.held_text(), "rulesaonebtwo");
    // Undo goes back to where the second exchange began, closing token included.
    let at = a.undo().unwrap();
    assert_eq!(at, after_first);
    b.truncate(at).unwrap();
    turn(&mut a, &mut b, "c", 100, &NO).unwrap();
    assert_eq!(b.held_text(), "rulesaonecagain");
    assert_eq!(b.held[after_first], DONE);
    // Reset keeps the opening, and the next message does not close a reply.
    let keep = a.reset();
    assert_eq!(keep, 6);
    b.truncate(keep).unwrap();
    b.replies.push_back(answer("fresh"));
    turn(&mut a, &mut b, "d", 100, &NO).unwrap();
    assert_eq!(b.held[6], USER);
    assert_eq!(b.held_text(), "rulesdfresh");
}

#[test]
fn a_message_that_cannot_fit_is_refused_and_a_failed_backend_ends_the_turn() {
    let mut a = appending();
    let mut b = Scripted::new(vec![]);
    a.prepare(&mut b).unwrap();
    b.capacity = 520;
    let end = turn(&mut a, &mut b, "hello", 100, &NO).unwrap();
    assert!(end.stopped.unwrap().starts_with("the context is full"));
    assert_eq!(b.held_text(), "rules", "nothing was fed");
    assert_eq!(a.history().len(), 1);

    let mut a = appending();
    let mut b = Scripted::new(vec![]);
    a.prepare(&mut b).unwrap();
    b.fail_next = true;
    assert_eq!(
        turn(&mut a, &mut b, "hello", 100, &NO).err().as_deref(),
        Some("the GPU went away")
    );
    // The turn is in the history, so an undo removes it.
    assert_eq!(a.history().len(), 1);
    assert_eq!(a.undo(), Some(6));
}

fn budgeted(budget: Budget) -> (Agent<'static, Appending>, Rc<RefCell<usize>>) {
    let (read, reads) = fake("fs_read", |_, n| Ok(format!("contents {n}")));
    let mut tools = Toolbox::default();
    tools.push(read);
    let workspace = Workspace {
        tools,
        edits: None,
        instructions: "rules".into(),
        base: PathBuf::from("."),
        described: Vec::new(),
    };
    let mut a = Agent::new(Appending, Some(workspace), false, Box::new(Quiet));
    a.set_budget(budget);
    (a, reads)
}

#[test]
fn a_reply_cut_by_its_limit_pauses_and_continue_finishes_it() {
    let mut a = appending();
    let mut b = Scripted::new(vec![text("a long ans"), answer("wer")]);
    a.prepare(&mut b).unwrap();
    let end = turn(&mut a, &mut b, "q", 10, &NO).unwrap();
    assert_eq!(
        end.paused.as_deref(),
        Some("the reply reached its token limit")
    );
    assert!(a.pending().is_some());
    assert!(
        a.history().is_empty(),
        "a paused turn is not an exchange yet"
    );
    let end = resume(&mut a, &mut b, 10, &NO).unwrap().unwrap();
    assert_eq!(end.read.answer, "a long answer", "read as one reply");
    assert_eq!(a.history().len(), 1);
    assert!(resume(&mut a, &mut b, 10, &NO).unwrap().is_none());
}

#[test]
fn a_spent_budget_holds_the_calls_and_continue_runs_them() {
    let (mut a, reads) = budgeted(Budget {
        time: None,
        tokens: Some(1),
    });
    let mut b = Scripted::new(vec![call(r#"fs_read {"path":"a"}"#), answer("done")]);
    a.prepare(&mut b).unwrap();
    let end = turn(&mut a, &mut b, "read a", 100, &NO).unwrap();
    assert_eq!(end.paused.as_deref(), Some("the turn's 1 tokens are spent"));
    assert_eq!(*reads.borrow(), 0, "nothing ran");
    let end = resume(&mut a, &mut b, 100, &NO).unwrap().unwrap();
    assert_eq!(*reads.borrow(), 1);
    assert_eq!(end.read.answer, "done");
}

#[test]
fn ctrl_c_holds_the_calls_and_a_new_message_answers_them_as_not_run() {
    let (mut a, reads) = budgeted(Budget::default());
    let mut b = Scripted::new(vec![call(r#"fs_read {"path":"a"}"#), answer("ok")]);
    a.prepare(&mut b).unwrap();
    let stop = AtomicBool::new(false);
    // Ctrl-C arrives while the reply is written; the tools have not run.
    let end = {
        let start = b.position();
        let prompt = a.begin("read a", start).unwrap();
        let Rendered::Append(tokens) = prompt else {
            panic!()
        };
        b.feed(&tokens).unwrap();
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        drive(&mut a, &mut b, 100, &stop, Step::Generate).unwrap()
    };
    assert_eq!(end.paused.as_deref(), Some("stopped by Ctrl-C"));
    assert_eq!(*reads.borrow(), 0);
    let end = turn(&mut a, &mut b, "never mind", 100, &NO).unwrap();
    assert_eq!(end.read.answer, "ok");
    assert!(b.held_text().contains("Not run: Jay paused this turn"));
    assert_eq!(a.history().len(), 2);
    assert_eq!(a.history()[0].user, "read a");
}

#[test]
fn undo_drops_a_paused_turn() {
    let mut a = appending();
    let mut b = Scripted::new(vec![text("cut")]);
    a.prepare(&mut b).unwrap();
    turn(&mut a, &mut b, "q", 3, &NO).unwrap();
    assert_eq!(a.undo(), Some(6));
    assert!(a.pending().is_none());
    assert!(a.history().is_empty());
}

/// Text that never repeats a block, so the loop guard leaves it alone.
fn varied(n: usize) -> String {
    let mut x: u32 = 12_345;
    (0..n)
        .map(|_| {
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            char::from(b'a' + u8::try_from((x >> 16) % 26).unwrap())
        })
        .collect()
}

#[test]
fn results_that_would_cross_three_quarters_are_handed_off_and_the_context_rebuilt() {
    let (read, _) = fake("fs_read", |_, n| Ok(format!("{n}:{}", "r".repeat(200))));
    let mut tools = Toolbox::default();
    tools.push(read);
    let workspace = Workspace {
        tools,
        edits: None,
        instructions: "rules".into(),
        base: PathBuf::from("."),
        described: Vec::new(),
    };
    let notes = Notes::default();
    let mut a = Agent::new(Appending, Some(workspace), false, Box::new(notes.clone()));
    // Reads of 225 positions each: the 14th's results would cross 3,000 of 4,000 (the
    // request alone is ~650 positions in this one-token-per-character format).
    let mut script: Vec<Vec<u32>> = (1..=14)
        .map(|i| call(&format!(r#"fs_read {{"path":"{i}"}}"#)))
        .collect();
    script.push(answer(" read 1 to 14; NEXT answer"));
    // A read made before the compaction is made again: its result is gone.
    script.push(call(r#"fs_read {"path":"1"}"#));
    script.push(answer("all read"));
    let mut b = Scripted::new(script);
    b.capacity = 4_000;
    a.prepare(&mut b).unwrap();
    let end = turn(&mut a, &mut b, "read them", 100, &NO).unwrap();
    assert_eq!(end.read.answer, "all read");
    let held = b.held_text();
    // The rebuilt context: the opening, the handoff, Jay's message, the newest round.
    assert!(held.starts_with("rules"), "{held}");
    assert!(held.contains("TASK: read 1 to 14; NEXT answer"));
    assert!(held.contains("read them"));
    assert!(
        held.contains("14:rrr") && held.contains("13:rrr"),
        "the newest rounds are kept"
    );
    assert!(!held.contains("12:rrr"), "older rounds are gone");
    assert!(b.held.len() < 2_000);
    assert!(notes
        .0
        .borrow()
        .iter()
        .any(|n| n.starts_with("[context] rebuilt")));
    assert!(held.contains("15:rrr"), "the earlier read ran again");
    assert!(!held.contains("Not run: you already made"));
    // An undo now goes back to the opening.
    assert_eq!(a.undo(), Some(6));
}

#[test]
fn a_reply_cut_by_the_end_of_the_context_is_written_again_after_a_handoff() {
    let mut a = appending();
    let mut long = text(&varied(2_000));
    long.push(DONE);
    let mut b = Scripted::new(vec![
        answer("x"),
        long,
        answer(" carry on"),
        answer("short answer"),
    ]);
    b.capacity = 1_200;
    a.prepare(&mut b).unwrap();
    // Fill the context past an eighth so a compaction may happen.
    turn(&mut a, &mut b, &"m".repeat(300), 100, &NO).unwrap();
    let end = turn(&mut a, &mut b, "write a lot", 3_000, &NO).unwrap();
    assert_eq!(end.read.answer, "short answer");
    let held = b.held_text();
    assert!(held.len() < 1_000, "the cut reply was dropped");
    assert!(held.contains("TASK: carry on"));
}

#[test]
fn a_new_message_into_a_crowded_context_starts_from_a_handoff() {
    let mut a = appending();
    let mut b = Scripted::new(vec![
        answer("first"),
        answer("second"),
        answer(" earlier talk"),
        answer("third"),
    ]);
    b.capacity = 4_000;
    a.prepare(&mut b).unwrap();
    turn(&mut a, &mut b, "hello", 100, &NO).unwrap();
    turn(&mut a, &mut b, &"p".repeat(2_980), 100, &NO).unwrap();
    let end = turn(&mut a, &mut b, "next one", 100, &NO).unwrap();
    assert_eq!(end.read.answer, "third");
    let held = b.held_text();
    assert!(
        !held.contains("first") && !held.contains("second"),
        "the old context is gone"
    );
    assert!(held.contains("TASK: earlier talk") && held.contains("next one"));
    // Jay's newest 2 KiB of earlier messages are kept word for word.
    assert!(held.contains("word for word"));
    assert!(held.matches('p').count() >= 2_000);
}

#[test]
fn a_saved_chat_resumes_with_its_context_and_its_paused_turn() {
    let (mut a, _) = budgeted(Budget {
        time: None,
        tokens: Some(1),
    });
    let mut b = Scripted::new(vec![answer("one"), call(r#"fs_read {"path":"a"}"#)]);
    a.prepare(&mut b).unwrap();
    turn(&mut a, &mut b, "first", 100, &NO).unwrap();
    let end = turn(&mut a, &mut b, "read a", 100, &NO).unwrap();
    assert!(end.paused.is_some());
    let saved = a.state(&b);
    // A new process: a new agent and backend, picking the chat up.
    let (mut a2, reads) = budgeted(Budget::default());
    let mut b2 = Scripted::new(vec![answer("read it")]);
    a2.restore(&saved, &mut b2).unwrap();
    assert_eq!(b2.held, b.held);
    assert_eq!(a2.history().len(), 1);
    let end = resume(&mut a2, &mut b2, 100, &NO).unwrap().unwrap();
    assert_eq!(*reads.borrow(), 1);
    assert_eq!(end.read.answer, "read it");
    assert_eq!(a2.history()[1].user, "read a");
    // /reset goes back to the saved opening.
    assert_eq!(a2.reset(), 6);
    // Snapshots from the chat before the loop are refused.
    let mut old = saved.clone();
    old["version"] = serde_json::json!(1);
    assert!(appending()
        .restore(&old, &mut Scripted::new(vec![]))
        .is_err());
}

#[test]
fn a_turn_that_does_the_same_work_after_each_handoff_has_its_tools_closed() {
    let (read, reads) = fake("fs_read", |_, n| Ok(format!("{n}:{}", "r".repeat(800))));
    let mut tools = Toolbox::default();
    tools.push(read);
    let workspace = Workspace {
        tools,
        edits: None,
        instructions: "rules".into(),
        base: PathBuf::from("."),
        described: Vec::new(),
    };
    let notes = Notes::default();
    let mut a = Agent::new(Appending, Some(workspace), false, Box::new(notes.clone()));
    // Each interval reads the same four files (the fourth's results cross three
    // quarters), then the context is handed off; the second adds nothing new.
    let mut script = Vec::new();
    for _ in 0..2 {
        for i in 1..=4 {
            script.push(call(&format!(r#"fs_read {{"path":"{i}"}}"#)));
        }
        script.push(answer(" reading"));
    }
    script.push(answer("what I have"));
    let mut b = Scripted::new(script);
    b.capacity = 4_000;
    a.prepare(&mut b).unwrap();
    let end = turn(&mut a, &mut b, "read them", 100, &NO).unwrap();
    assert_eq!(end.read.answer, "what I have");
    assert!(notes
        .0
        .borrow()
        .iter()
        .any(|n| n.contains("the same work again after a handoff")));
    // Two intervals of four reads, the second adding nothing; then the answer.
    assert_eq!(*reads.borrow(), 8);
}
