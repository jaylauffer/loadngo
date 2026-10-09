//! A request as the model reads it, and fitted to its window.
//!
//! The state and each question are rendered and tokenized apart; the model reads the
//! state's tokens followed by one question's:
//!
//! ```text
//! <state>
//! Help! My payouts have been failing for 3 days.
//! </state>
//! <question type="choice">
//! Select exactly one option.
//! Which team should handle this?
//! <options>
//! 1. billing — payments, invoices, payouts
//! 2. technical — bugs, outages, API errors
//! </options>
//! </question>
//! <answer>
//! ```
//!
//! The head reads the hidden state at the question's last token (`<answer>`) and at the
//! last token of each option's line. A yes/no question (`noul`) is the two options
//! `false` and `true`; a score's options are its levels, numbered from 0.
//!
//! **Fitting.** The window is `max_length` tokens. The questions are reserved room first,
//! up to three quarters of the window: a question longer than that loses its beginning,
//! never its options or `<answer>`. The state gets the rest and loses its end. At most
//! [`MAX_BATCH`] questions are fitted together; a request with more is fitted in runs of
//! that many, in order.
//!
//! All text is put in Unicode NFC before rendering, so the tokenizer's offsets and the
//! option spans count the same characters.

use std::ops::Range;

use icu_normalizer::ComposingNormalizerBorrowed;
use loadngo_inference::system_one::Question;

use crate::tokenizer::Tokenizer;

/// Questions fitted to the window together.
pub const MAX_BATCH: usize = 32;

/// The share of the window the questions may claim before the state is cut.
pub const QUESTION_SHARE: f64 = 0.75;

/// The kind of question, which picks its header and its temperature.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Noul,
    Choice,
    Score,
}

impl Kind {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Noul => "noul",
            Self::Choice => "choice",
            Self::Score => "score",
        }
    }
}

/// One question, rendered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rendered {
    pub text: String,
    pub kind: Kind,
    /// Each option's line, `[start, end)` in characters of `text`, in the order shown.
    pub option_spans: Vec<(usize, usize)>,
}

/// Python's `str.isspace`, which the reference uses to strip and collapse text:
/// Unicode `White_Space` and the four information separators U+001C..U+001F.
fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

fn strip(text: &str) -> &str {
    text.trim_matches(is_space)
}

fn collapse(text: &str) -> String {
    text.split(is_space)
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn nfc(text: &str) -> String {
    ComposingNormalizerBorrowed::new_nfc()
        .normalize(text)
        .into_owned()
}

/// The shared beginning of every question's prompt.
#[must_use]
pub fn render_state(state: &str) -> String {
    format!("<state>\n{}\n</state>\n", strip(&nfc(state)))
}

/// The two options of a yes/no question, as the decider was trained to read them.
pub const NOUL_OPTIONS: [(&str, &str); 2] = [
    ("false", "the statement does not hold for this state"),
    ("true", "the statement holds for this state"),
];

/// Renders one question.
#[must_use]
pub fn render_question(question: &Question) -> Rendered {
    let (kind, header, instructions, pairs): (Kind, &str, String, Vec<(String, String)>) =
        match question {
            Question::Noul { instructions } => (
                Kind::Noul,
                "Decide whether the statement is true of the state.",
                instructions.clone(),
                NOUL_OPTIONS
                    .iter()
                    .map(|(l, d)| ((*l).to_owned(), (*d).to_owned()))
                    .collect(),
            ),
            Question::Choice {
                instructions,
                criteria,
            } => (
                Kind::Choice,
                "Select exactly one option.",
                instructions.clone().unwrap_or_default(),
                criteria
                    .iter()
                    .map(|(l, d)| (nfc(l), strip(&nfc(d)).to_owned()))
                    .collect(),
            ),
            Question::Score {
                instructions,
                criteria,
            } => (
                Kind::Score,
                "Rate the state against the ordered levels below (lowest first).",
                instructions.clone().unwrap_or_default(),
                criteria
                    .iter()
                    .enumerate()
                    .map(|(i, (_, d))| (i.to_string(), nfc(d)))
                    .collect(),
            ),
        };
    let prefix = format!(
        "<question type=\"{}\">\n{header}\n{}\n<options>\n",
        kind.name(),
        strip(&nfc(&instructions))
    );
    let mut text = prefix.clone();
    let mut option_spans = Vec::with_capacity(pairs.len());
    let mut cursor = prefix.chars().count();
    for (i, (name, description)) in pairs.iter().enumerate() {
        if i > 0 {
            text.push('\n');
            cursor += 1;
        }
        let description = collapse(description);
        let line = if description.is_empty() {
            format!("{}. {name}", i + 1)
        } else {
            format!("{}. {name} \u{2014} {description}", i + 1)
        };
        let len = line.chars().count();
        option_spans.push((cursor, cursor + len));
        cursor += len;
        text.push_str(&line);
    }
    text.push_str("\n</options>\n</question>\n<answer>");
    Rendered {
        text,
        kind,
        option_spans,
    }
}

/// A run of questions fitted to the window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fitted {
    /// Which of the request's questions, in order.
    pub questions: Range<usize>,
    /// The state's tokens.
    pub state: Vec<u32>,
    /// Each question's tokens.
    pub question_ids: Vec<Vec<u32>>,
    /// Each question's option positions, relative to the start of its tokens.
    pub options: Vec<Vec<usize>>,
}

/// Why a request could not be fitted.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FitError {
    #[error("question {0}: an option's line lost all its tokens when the question was cut to fit")]
    OptionCut(usize),
}

/// Fits `state` (as [`render_state`] gives it) and every question to a window of
/// `max_length` tokens, in runs of at most [`MAX_BATCH`].
///
/// # Errors
/// When a question is so long that cutting it removed an option's line.
pub fn fit(
    tokenizer: &Tokenizer,
    state: &str,
    rendered: &[Rendered],
    max_length: usize,
) -> Result<Vec<Fitted>, FitError> {
    let mut out = Vec::new();
    for first in (0..rendered.len()).step_by(MAX_BATCH) {
        let run = &rendered[first..rendered.len().min(first + MAX_BATCH)];
        let tokens: Vec<_> = run.iter().map(|r| tokenizer.encode(&r.text)).collect();
        let longest = tokens.iter().map(Vec::len).max().unwrap_or(0);
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss
        )]
        let share = ((max_length as f64) * QUESTION_SHARE) as usize;
        let reserve = longest.min(share.max(1));
        let budget = max_length.saturating_sub(reserve).max(1);
        let mut state_ids = tokenizer.ids(state);
        state_ids.truncate(budget);
        let mut question_ids = Vec::with_capacity(run.len());
        let mut options = Vec::with_capacity(run.len());
        for (k, (question, tokens)) in run.iter().zip(&tokens).enumerate() {
            let tokens = &tokens[tokens.len().saturating_sub(reserve)..];
            let positions = question
                .option_spans
                .iter()
                .map(|&(a, b)| {
                    tokens
                        .iter()
                        .rposition(|t| t.end > t.start && t.start >= a && t.end <= b)
                        .ok_or(FitError::OptionCut(first + k))
                })
                .collect::<Result<Vec<_>, _>>()?;
            question_ids.push(tokens.iter().map(|t| t.id).collect());
            options.push(positions);
        }
        out.push(Fitted {
            questions: first..first + run.len(),
            state: state_ids,
            question_ids,
            options,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_choice_renders_with_numbered_lines_and_their_spans() {
        let r = render_question(&Question::Choice {
            instructions: Some("  Which team?\n".into()),
            criteria: vec![
                ("billing".into(), "  payments,\n invoices ".into()),
                ("sales".into(), String::new()),
            ],
        });
        assert_eq!(
            r.text,
            "<question type=\"choice\">\nSelect exactly one option.\nWhich team?\n<options>\n\
             1. billing \u{2014} payments, invoices\n2. sales\n</options>\n</question>\n<answer>"
        );
        let chars: Vec<char> = r.text.chars().collect();
        let line = |(a, b): (usize, usize)| chars[a..b].iter().collect::<String>();
        assert_eq!(
            line(r.option_spans[0]),
            "1. billing \u{2014} payments, invoices"
        );
        assert_eq!(line(r.option_spans[1]), "2. sales");
    }

    #[test]
    fn a_noul_is_false_then_true_and_a_score_numbers_its_levels() {
        let r = render_question(&Question::Noul {
            instructions: "It is urgent.".into(),
        });
        assert!(r
            .text
            .contains("1. false \u{2014} the statement does not hold"));
        assert!(r.text.contains("2. true \u{2014} the statement holds"));
        let r = render_question(&Question::Score {
            instructions: None,
            criteria: vec![("1".into(), "calm".into()), ("5".into(), "angry".into())],
        });
        assert!(r.text.contains(
            "Rate the state against the ordered levels below (lowest first).\n\n<options>"
        ));
        assert!(r.text.contains("1. 0 \u{2014} calm\n2. 1 \u{2014} angry"));
    }
}
