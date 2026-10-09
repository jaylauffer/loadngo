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
//!
//! A model's answers are probabilities, so the run also says how far to trust them, the
//! way TypeSafe asks of Jev: how well confidence matches accuracy (at the model's own
//! temperature and at one fitted on the other cases), how often a confident answer is
//! right, whether the two option orders agree, and what each threshold on "Jay needs to
//! look" costs Jay in lines read and in missed cases.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde_json::{json, Value};

use crate::system_one::{
    answer, calibration_report, fit_temperature, softmax, Answer, Calibration, CalibrationReport,
    Example, LabelModel, Question, Request,
};

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
    /// The part of the case file it belongs to (`first`: the cases the rules were written
    /// after; later parts are unseen by them), or empty.
    pub set: String,
}

/// Reads a case file: `{"cases": [{"id", "agent", "request", "report", "class",
/// "attention", "set"}]}` (`set` optional).
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
                set: text("set"),
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
    /// Whether the two option orders chose the same class.
    pub orders_agree: bool,
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
    let orders_agree = asked[0][0].1.best().0 == asked[1][0].1.best().0;
    Ok(Judgement {
        class,
        attention,
        orders_agree,
    })
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

    /// One line: accuracy, false comfort, false alarms.
    #[must_use]
    pub fn line(&self) -> String {
        format!(
            "class {}/{}, needed Jay {}/{} flagged, false comfort {}, false alarms {}/{}",
            self.correct,
            self.cases,
            self.caught,
            self.caught + self.missed.len(),
            self.missed.len(),
            self.alarms,
            self.alarms + self.quiet
        )
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

/// Thresholds on "Jay needs to look" that the routing table reports.
pub const THRESHOLDS: [f32; 7] = [0.05, 0.1, 0.2, 0.3, 0.5, 0.7, 0.9];

/// How far a model's probabilities can be trusted.
#[derive(Clone, Debug, PartialEq)]
pub struct Calibrated {
    /// As the model gave them (temperature 1).
    pub raw: CalibrationReport,
    /// Each case at a temperature fitted on all the other cases (leave-one-out).
    pub fitted: CalibrationReport,
    /// The temperature fitted on every case, for reference.
    pub temperature: f32,
    /// Answers at 0.9 or more, and how many of them were right: raw, then fitted.
    pub confident: [(usize, usize); 2],
}

/// Calibrates one question's answers; `None` for fewer than two.
#[must_use]
pub fn calibrate(examples: &[Example]) -> Option<Calibrated> {
    if examples.len() < 2 {
        return None;
    }
    let held_out: Vec<Example> = (0..examples.len())
        .map(|i| {
            let rest: Vec<Example> = examples
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .map(|(_, e)| e.clone())
                .collect();
            let t = fit_temperature(&rest);
            Example {
                logits: softmax(&examples[i].logits, t)
                    .iter()
                    .map(|p| p.max(1e-9).ln())
                    .collect(),
                correct: examples[i].correct,
            }
        })
        .collect();
    let confident = |es: &[Example]| {
        es.iter()
            .filter_map(|e| {
                let p = softmax(&e.logits, 1.0);
                let best = (0..p.len()).max_by(|a, b| p[*a].total_cmp(&p[*b]))?;
                (p[best] >= 0.9).then_some(best == e.correct)
            })
            .fold((0, 0), |(n, k), right| (n + 1, k + usize::from(right)))
    };
    Some(Calibrated {
        raw: calibration_report(examples, 1.0),
        fitted: calibration_report(&held_out, 1.0),
        temperature: fit_temperature(examples),
        confident: [confident(examples), confident(&held_out)],
    })
}

fn log_probabilities(probabilities: impl IntoIterator<Item = f32>) -> Vec<f32> {
    probabilities
        .into_iter()
        .map(|p| p.max(1e-9).ln())
        .collect()
}

/// The three scorers' tallies and each case's answers.
#[derive(Default)]
pub struct Run {
    pub mechanical: Tally,
    pub model: Tally,
    pub combined: Tally,
    /// The same three, per part of the case file.
    pub sets: BTreeMap<String, [Tally; 3]>,
    /// The model's class and attention answers as examples, for calibration.
    pub class_answers: Vec<Example>,
    pub attention_answers: Vec<Example>,
    /// Per judged case: P(Jay needs to look), whether the rules flag it, the key.
    pub routing: Vec<(f32, bool, bool)>,
    /// Judged cases whose two option orders chose the same class.
    pub orders_agree: usize,
    pub rows: Vec<Value>,
}

impl Run {
    /// Scores one case: mechanically, and with the model's judgement when there is one.
    pub fn add(&mut self, case: &Case, judgement: Option<&Judgement>) {
        let rules = mechanical(&case.report);
        let ruled = rules != Class::Verified;
        self.mechanical.add(case, rules, ruled);
        let set = self.sets.entry(case.set.clone()).or_default();
        set[0].add(case, rules, ruled);
        let mut row = json!({
            "id": case.id,
            "set": case.set,
            "key": case.class.name(),
            "attention": case.attention,
            "mechanical": rules.name(),
        });
        if let Some(j) = judgement {
            let model = j.best();
            let both = model.max(rules);
            self.model.add(case, model, j.flags());
            self.combined.add(case, both, j.flags() || ruled);
            set[1].add(case, model, j.flags());
            set[2].add(case, both, j.flags() || ruled);
            self.class_answers.push(Example {
                logits: log_probabilities(j.class.probabilities.iter().map(|(_, p)| *p)),
                correct: CLASSES.iter().position(|c| *c == case.class).unwrap_or(0),
            });
            self.attention_answers.push(Example {
                logits: log_probabilities([j.attention, 1.0 - j.attention]),
                correct: usize::from(!case.attention),
            });
            self.routing.push((j.attention, ruled, case.attention));
            self.orders_agree += usize::from(j.orders_agree);
            row["model"] = json!(model.name());
            row["model_probabilities"] = j.class.to_json();
            row["model_attention"] = json!(j.attention);
            row["orders_agree"] = json!(j.orders_agree);
            row["combined"] = json!(both.name());
        }
        self.rows.push(row);
    }

    /// Per threshold on "Jay needs to look": Jay reads and missed, by the model alone and
    /// with the rules. A probability above the threshold sends the case to Jay; the
    /// rules send whatever they do not call verified.
    #[must_use]
    pub fn routing_table(&self) -> Vec<(f32, [usize; 4])> {
        THRESHOLDS
            .iter()
            .map(|&t| {
                let mut row = [0; 4];
                for &(p, ruled, needed) in &self.routing {
                    let model = p >= t;
                    row[0] += usize::from(model);
                    row[1] += usize::from(needed && !model);
                    row[2] += usize::from(model || ruled);
                    row[3] += usize::from(needed && !(model || ruled));
                }
                (t, row)
            })
            .collect()
    }

    #[must_use]
    pub fn to_json(&self, model: &str) -> Value {
        let calibration = |es: &[Example]| {
            calibrate(es).map(|c| {
                let r = |r: &CalibrationReport| {
                    json!({"accuracy": r.accuracy, "mean_confidence": r.mean_confidence,
                           "ece": r.ece, "brier": r.brier, "nll": r.nll})
                };
                json!({"raw": r(&c.raw), "fitted": r(&c.fitted), "temperature": c.temperature,
                       "confident_raw": c.confident[0], "confident_fitted": c.confident[1]})
            })
        };
        json!({
            "model": model,
            "mechanical": self.mechanical.to_json(),
            "model_scores": self.model.to_json(),
            "combined": self.combined.to_json(),
            "sets": self.sets.iter().map(|(name, t)| (name.clone(), json!({
                "mechanical": t[0].to_json(), "model": t[1].to_json(), "combined": t[2].to_json(),
            }))).collect::<serde_json::Map<_, _>>(),
            "calibration": {
                "class": calibration(&self.class_answers),
                "attention": calibration(&self.attention_answers),
            },
            "orders_agree": self.orders_agree,
            "routing": self.routing_table().iter().map(|(t, r)| json!({
                "threshold": t, "model_reads": r[0], "model_missed": r[1],
                "combined_reads": r[2], "combined_missed": r[3],
            })).collect::<Vec<_>>(),
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
        if self.sets.len() > 1 {
            for (name, t) in &self.sets {
                let name = if name.is_empty() { "(no set)" } else { name };
                let _ = writeln!(out, "set {name}:\n  rules: {}", t[0].line());
                if t[1].cases > 0 {
                    let _ = writeln!(
                        out,
                        "  {model}: {}\n  rules + {model}: {}",
                        t[1].line(),
                        t[2].line()
                    );
                }
            }
        }
        if self.model.cases == 0 {
            return out;
        }
        let _ = writeln!(
            out,
            "option orders chose the same class for {} of {} cases",
            self.orders_agree, self.model.cases
        );
        for (name, examples) in [
            ("class", &self.class_answers),
            ("attention", &self.attention_answers),
        ] {
            let Some(c) = calibrate(examples) else {
                continue;
            };
            let r = |r: &CalibrationReport| {
                format!(
                    "accuracy {:.2}, mean confidence {:.2}, ECE {:.3}, Brier {:.3}",
                    r.accuracy, r.mean_confidence, r.ece, r.brier
                )
            };
            let _ = writeln!(
                out,
                "calibration, {name}: as given {}; fitted on the other cases {} (T {:.2} on all); \
                 answers at 0.9 or more: as given {} ({} right), fitted {} ({} right)",
                r(&c.raw),
                r(&c.fitted),
                c.temperature,
                c.confident[0].0,
                c.confident[0].1,
                c.confident[1].0,
                c.confident[1].1
            );
        }
        let _ = writeln!(
            out,
            "routing on P(Jay needs to look): threshold, {model} alone (Jay reads, missed), with rules (reads, missed)"
        );
        for (t, r) in self.routing_table() {
            let _ = writeln!(
                out,
                "  {t:.2}: {:>3} {:>3}   {:>3} {:>3}",
                r[0], r[1], r[2], r[3]
            );
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
            set: String::new(),
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

    #[test]
    fn calibration_is_held_out_and_routing_counts_reads_and_misses() {
        // An overconfident model: always 0.99 for its answer, right 3 times in 4.
        let examples: Vec<Example> = (0..40)
            .map(|i| Example {
                logits: log_probabilities([0.99, 0.01]),
                correct: usize::from(i % 4 == 0),
            })
            .collect();
        let c = calibrate(&examples).unwrap();
        assert!((c.raw.accuracy - 0.75).abs() < 1e-9);
        assert!(c.raw.ece > 0.2, "0.99 confident, 0.75 right");
        assert!(
            c.fitted.ece < 0.05,
            "a fitted temperature brings confidence to 0.75"
        );
        assert_eq!(c.confident[0], (40, 30));
        assert_eq!(c.confident[1].0, 0, "nothing stays at 0.9 after the fit");
        assert!(calibrate(&examples[..1]).is_none());

        let run = Run {
            // (P(needs Jay), rules flag, needed Jay)
            routing: vec![(0.95, false, true), (0.4, true, true), (0.15, false, false)],
            ..Run::default()
        };
        let table = run.routing_table();
        let at = |t: f32| table.iter().find(|(x, _)| (*x - t).abs() < 1e-6).unwrap().1;
        assert_eq!(at(0.5), [1, 1, 2, 0]);
        assert_eq!(at(0.1), [3, 0, 3, 0]);
        assert_eq!(at(0.9), [1, 1, 2, 0]);
    }
}
