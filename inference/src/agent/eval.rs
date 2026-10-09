//! Measuring an orchestrator before trusting it (step 4 of `docs/AGENT_LOOP.md`, kimi
//! `docs/ORCHESTRATION.md`): finished tasks, each a worker's report and an answer key,
//! classified by mechanical rules over the report, by a model answering typed questions
//! (System One, so its answer cannot wander), and by the two together.
//!
//! The classes are the digest's: `verified`, `unverified` (done, but a check did not run or
//! its result is not known), `failed`, `needs-jay` (Jay must decide, approve, push, sign or
//! test by hand). The error that matters most is false comfort: a task that needed Jay
//! reported as verified. A false alarm costs Jay one line to read.
//!
//! The cases themselves are not in this repository (they quote the private agent board);
//! the runner takes a path.

use std::fmt::Write as _;

use serde_json::{json, Value};

use crate::system_one::{answer, Answer, Calibration, LabelModel, Question, Request};

/// A digest class, least to most severe.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Class {
    Verified,
    Unverified,
    Failed,
    NeedsJay,
}

pub const CLASSES: [Class; 4] = [
    Class::Verified,
    Class::Unverified,
    Class::Failed,
    Class::NeedsJay,
];

impl Class {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Unverified => "unverified",
            Self::Failed => "failed",
            Self::NeedsJay => "needs-jay",
        }
    }

    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        CLASSES.into_iter().find(|c| c.name() == text)
    }

    const fn description(self) -> &'static str {
        match self {
            Self::Verified => {
                "verified: done, and the report names checks that ran and passed; nothing is \
                 left for Jay"
            }
            Self::Unverified => {
                "not verified: done or mostly done, but a check did not run or its result is not \
                 known yet (CI pending, not run on a device, not played, not reviewed, claims \
                 without evidence)"
            }
            Self::Failed => "failed: the work did not do what was asked, or a check failed",
            Self::NeedsJay => {
                "needs Jay: Jay must decide, approve, push, sign, test by hand or otherwise act \
                 before this is finished"
            }
        }
    }
}

/// A finished task and its answer key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Case {
    pub id: String,
    pub agent: String,
    pub request: String,
    pub report: String,
    pub class: Class,
    /// With hindsight, Jay had to act on it, or a problem in it surfaced later.
    pub attention: bool,
}

/// Reads a case file: `{"cases": [{"id", "agent", "request", "report", "class",
/// "attention"}]}`.
///
/// # Errors
/// When the file is not that shape.
pub fn load(text: &str) -> Result<Vec<Case>, String> {
    let file: Value = serde_json::from_str(text).map_err(|e| format!("case file: {e}"))?;
    file["cases"]
        .as_array()
        .ok_or("the case file has no `cases` list")?
        .iter()
        .map(|c| {
            let text = |k: &str| c[k].as_str().unwrap_or_default().to_owned();
            Ok(Case {
                id: text("id"),
                agent: text("agent"),
                request: text("request"),
                report: text("report"),
                class: Class::parse(c["class"].as_str().unwrap_or_default())
                    .ok_or_else(|| format!("case {}: unknown class", c["id"]))?,
                attention: c["attention"]
                    .as_bool()
                    .ok_or_else(|| format!("case {}: no attention", c["id"]))?,
            })
        })
        .collect()
}

/// The mechanical classification: phrases in the report, most severe first. Written once
/// from the checks `ORCHESTRATION.md` lists, before any model was scored, and not tuned to
/// the cases.
#[must_use]
pub fn mechanical(report: &str) -> Class {
    let lower = report.to_lowercase();
    let any = |phrases: &[&str]| phrases.iter().any(|p| lower.contains(p));
    if any(&[
        "jay to ",
        "jay:",
        "jay needs",
        "needs jay",
        "jay's decision",
        "jay's listening",
        "jay's approval",
        "no push",
        "not pushed",
        "not committed",
        "uncommitted",
        "to decide",
        "to choose",
        "to sign",
        "to confirm",
    ]) {
        return Class::NeedsJay;
    }
    if report.contains("FAILS")
        || any(&[
            "unable to",
            "could not",
            "couldn't",
            "write failure",
            "context full",
            "[stopped",
            "[paused",
        ])
    {
        return Class::Failed;
    }
    if any(&[
        "not run",
        "not checked",
        "not tested",
        "not playtested",
        "not played",
        "not yet seen",
        "not seen",
        "pending",
        "in progress",
        "queued",
        "not measured",
        "unmeasured",
        "macos only",
        "needs a restart",
        "restart to",
        "no external",
        " live",
    ]) {
        return Class::Unverified;
    }
    if any(&["green", "pass", "succeeded", "verified", "match"]) {
        Class::Verified
    } else {
        Class::Unverified
    }
}

/// A model's answers for one case.
#[derive(Clone, Debug)]
pub struct Judgement {
    pub class: Answer,
    /// Probability that Jay needs to look at it.
    pub attention: f32,
}

impl Judgement {
    #[must_use]
    pub fn best(&self) -> Class {
        let (label, _) = self.class.best();
        Class::parse(label).unwrap_or(Class::Unverified)
    }

    /// Whether the model flags it for Jay: any class but verified, or attention at 0.5.
    #[must_use]
    pub fn flags(&self) -> bool {
        self.best() != Class::Verified || self.attention >= 0.5
    }
}

/// The question's state: who reported what, for which request.
#[must_use]
pub fn state(case: &Case) -> String {
    format!(
        "A worker agent finished a task in Jay's workspace and reported back.\n\nAgent: {}\n\
         Jay's request: {}\n\nThe worker's report:\n{}",
        case.agent,
        if case.request.is_empty() {
            "(not recorded; the report says what was done)"
        } else {
            &case.request
        },
        case.report
    )
}

/// The two typed questions, with the options in `CLASSES` order or reversed.
fn questions(reversed: bool) -> Vec<(String, Question)> {
    let mut classes: Vec<Class> = CLASSES.to_vec();
    let mut attention = vec![
        ("yes".to_owned(), "yes".to_owned()),
        ("no".to_owned(), "no".to_owned()),
    ];
    if reversed {
        classes.reverse();
        attention.reverse();
    }
    vec![
        (
            "class".to_owned(),
            Question::Choice {
                instructions: Some(
                    "What should the digest for Jay say about this task? Judge the report \
                     itself: a claim is verified only if the report shows the check that ran."
                        .into(),
                ),
                criteria: classes
                    .iter()
                    .map(|c| (c.name().to_owned(), c.description().to_owned()))
                    .collect(),
            },
        ),
        (
            "attention".to_owned(),
            Question::Choice {
                instructions: Some(
                    "Does Jay need to look at this or act on it: decide, approve, push, sign, \
                     test by hand, or deal with a problem in the work?"
                        .into(),
                ),
                criteria: attention,
            },
        ),
    ]
}

/// A label's probability in an answer.
fn p(answer: &Answer, label: &str) -> f32 {
    answer
        .probabilities
        .iter()
        .find(|(l, _)| l == label)
        .map_or(0.0, |(_, p)| *p)
}

/// Asks `model` the two typed questions about `case`, twice, with the options in two
/// orders, and averages each option's probability: a model's preference for a letter
/// (gpt-oss for B, Kimi Linear for A, in the first run) then cancels out.
///
/// # Errors
/// When the model fails.
pub fn judge(model: &mut dyn LabelModel, case: &Case) -> Result<Judgement, String> {
    let state = state(case);
    let mut asked = Vec::new();
    for reversed in [false, true] {
        asked.push(answer(
            model,
            &Request {
                state: state.clone(),
                questions: questions(reversed),
            },
            Calibration::default(),
        )?);
    }
    let class = Answer {
        probabilities: CLASSES
            .iter()
            .map(|c| {
                let label = c.name();
                (
                    label.to_owned(),
                    (p(&asked[0][0].1, label) + p(&asked[1][0].1, label)) / 2.0,
                )
            })
            .collect(),
    };
    let attention = (p(&asked[0][1].1, "yes") + p(&asked[1][1].1, "yes")) / 2.0;
    Ok(Judgement { class, attention })
}

/// The tallies for one scorer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tally {
    pub cases: usize,
    pub correct: usize,
    /// `[key][predicted]`, in [`CLASSES`] order.
    pub confusion: [[usize; 4]; 4],
    /// Needed Jay and was flagged.
    pub caught: usize,
    /// Needed Jay and was not flagged: false comfort.
    pub missed: Vec<String>,
    /// Did not need Jay and was flagged.
    pub alarms: usize,
    pub quiet: usize,
}

impl Tally {
    pub fn add(&mut self, case: &Case, predicted: Class, flagged: bool) {
        let index = |c: Class| CLASSES.iter().position(|x| *x == c).unwrap_or(0);
        self.cases += 1;
        self.correct += usize::from(predicted == case.class);
        self.confusion[index(case.class)][index(predicted)] += 1;
        match (case.attention, flagged) {
            (true, true) => self.caught += 1,
            (true, false) => self.missed.push(case.id.clone()),
            (false, true) => self.alarms += 1,
            (false, false) => self.quiet += 1,
        }
    }

    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "cases": self.cases,
            "correct": self.correct,
            "confusion": self.confusion,
            "caught": self.caught,
            "missed": self.missed,
            "alarms": self.alarms,
            "quiet": self.quiet,
        })
    }

    /// One block of text: accuracy, attention caught and missed, the confusion matrix.
    #[must_use]
    pub fn report(&self, name: &str) -> String {
        let mut out = String::new();
        let needed = self.caught + self.missed.len();
        let _ = writeln!(
            out,
            "{name}: class {}/{} correct; needed Jay {} of {} flagged, false comfort {} {:?}; \
             false alarms {} of {}",
            self.correct,
            self.cases,
            self.caught,
            needed,
            self.missed.len(),
            self.missed,
            self.alarms,
            self.alarms + self.quiet
        );
        let _ = writeln!(
            out,
            "  key \\ predicted   verified unverified failed needs-jay"
        );
        for (c, row) in CLASSES.iter().zip(self.confusion) {
            let _ = writeln!(
                out,
                "  {:<17} {:>8} {:>10} {:>6} {:>9}",
                c.name(),
                row[0],
                row[1],
                row[2],
                row[3]
            );
        }
        out
    }
}

/// The three scorers' tallies and each case's answers.
#[derive(Default)]
pub struct Run {
    pub mechanical: Tally,
    pub model: Tally,
    pub combined: Tally,
    pub rows: Vec<Value>,
}

impl Run {
    /// Scores one case: mechanically, and with the model's judgement when there is one.
    pub fn add(&mut self, case: &Case, judgement: Option<&Judgement>) {
        let rules = mechanical(&case.report);
        self.mechanical.add(case, rules, rules != Class::Verified);
        let mut row = json!({
            "id": case.id,
            "key": case.class.name(),
            "attention": case.attention,
            "mechanical": rules.name(),
        });
        if let Some(j) = judgement {
            let model = j.best();
            self.model.add(case, model, j.flags());
            let both = model.max(rules);
            self.combined
                .add(case, both, j.flags() || rules != Class::Verified);
            row["model"] = json!(model.name());
            row["model_probabilities"] = j.class.to_json();
            row["model_attention"] = json!(j.attention);
            row["combined"] = json!(both.name());
        }
        self.rows.push(row);
    }

    #[must_use]
    pub fn to_json(&self, model: &str) -> Value {
        json!({
            "model": model,
            "mechanical": self.mechanical.to_json(),
            "model_scores": self.model.to_json(),
            "combined": self.combined.to_json(),
            "cases": self.rows,
        })
    }

    #[must_use]
    pub fn report(&self, model: &str) -> String {
        let mut out = self.mechanical.report("mechanical rules");
        if self.model.cases > 0 {
            out.push_str(&self.model.report(model));
            out.push_str(&self.combined.report(&format!("rules + {model}")));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn case(report: &str, class: Class, attention: bool) -> Case {
        Case {
            id: report.chars().take(12).collect(),
            agent: "a".into(),
            request: String::new(),
            report: report.into(),
            class,
            attention,
        }
    }

    #[test]
    fn the_rules_take_the_most_severe_phrase() {
        assert_eq!(
            mechanical("Tests pass. Open: Jay to decide which."),
            Class::NeedsJay
        );
        assert_eq!(mechanical("CI green. Not pushed."), Class::NeedsJay);
        assert_eq!(mechanical("[verify] cargo check: FAILS"), Class::Failed);
        assert_eq!(mechanical("Gates pass. CI in progress."), Class::Unverified);
        assert_eq!(mechanical("CI run green on all four."), Class::Verified);
        assert_eq!(mechanical("Wrote the docs."), Class::Unverified);
    }

    #[test]
    fn a_case_file_loads_and_refuses_unknown_classes() {
        let cases = load(
            r#"{"cases": [{"id": "x", "agent": "Kimi", "report": "r", "class": "needs-jay",
               "attention": true}]}"#,
        )
        .unwrap();
        assert_eq!(cases[0].class, Class::NeedsJay);
        assert!(load(r#"{"cases": [{"id": "x", "class": "great", "attention": true}]}"#).is_err());
        assert!(load(r#"{"cases": [{"id": "x", "class": "failed"}]}"#).is_err());
    }

    /// A label model that always prefers one letter.
    struct Always(usize);
    impl LabelModel for Always {
        fn label_logits(
            &mut self,
            _: &str,
            _: &str,
            labels: &[String],
        ) -> Result<Vec<f32>, String> {
            Ok((0..labels.len())
                .map(|i| {
                    if i == self.0.min(labels.len() - 1) {
                        5.0
                    } else {
                        0.0
                    }
                })
                .collect())
        }
    }

    #[test]
    fn false_comfort_is_counted_and_the_combination_takes_the_more_severe() {
        let cases = [
            case("CI green.", Class::Verified, false),
            case("CI green.", Class::NeedsJay, true),
            case("Not pushed.", Class::NeedsJay, true),
        ];
        let mut run = Run::default();
        // A model that always answers the first letter: asked in both orders, its
        // preference cancels out, so verified and needs-jay tie and attention is 0.5.
        let mut model = Always(0);
        for c in &cases {
            let j = judge(&mut model, c).unwrap();
            assert!((p(&j.class, "verified") - p(&j.class, "needs-jay")).abs() < 1e-6);
            assert!(p(&j.class, "failed") < 0.01);
            assert!((j.attention - 0.5).abs() < 1e-6);
            run.add(c, Some(&j));
        }
        assert_eq!(run.mechanical.correct, 2);
        assert_eq!(
            run.mechanical.missed.len(),
            1,
            "the green report that needed Jay"
        );
        // Attention at 0.5 flags everything: nothing missed, one false alarm.
        assert_eq!((run.model.caught, run.model.alarms), (2, 1));
        assert!(run.model.missed.is_empty());
        let text = run.report("toy");
        assert!(text.contains("false comfort 1"));
        assert!(run.to_json("toy")["cases"][2]["combined"] == "needs-jay");
        assert!(state(&cases[0]).contains("not recorded"));
    }
}
