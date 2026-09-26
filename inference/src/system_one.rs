//! Typed, probabilistic decisions from a language model: unstructured state in, a
//! probability for every allowed answer out, never free text.
//!
//! The idea follows TypeSafe's "System One" models (Jev): a question is a *type*
//! (true/false, a choice among labelled options, or an ordinal score), the model answers
//! with a probability per option, and code acts on those probabilities. This module is
//! model-independent: a [`LabelModel`] supplies the model's scores for each option label
//! as the next token, and everything else (question text, the answer distribution,
//! temperature calibration and its metrics) lives here. Nothing here generates text,
//! so an answer can never fall outside its type.
//!
//! Requests and responses use the shape of TypeSafe's `system_one` API
//! (`{"state": ..., "questions": {id: {"type": "noul" | "choice" | "score", ...}}}`),
//! so the two stay interchangeable.

use serde_json::{json, Map, Value};

/// Most options one question may have: each is a single letter label (`A`..`Z`), which
/// keeps every option one token, so one forward pass scores them all.
pub const MAX_OPTIONS: usize = 26;

/// One typed question.
#[derive(Debug, Clone, PartialEq)]
pub enum Question {
    /// True or false: is `instructions` true of the state?
    Noul { instructions: String },
    /// Exactly one of the labelled options; `criteria` is `(label, description)`.
    Choice {
        instructions: Option<String>,
        criteria: Vec<(String, String)>,
    },
    /// An ordinal score: `criteria` is `(value, description)` in increasing order.
    Score {
        instructions: Option<String>,
        criteria: Vec<(String, String)>,
    },
}

impl Question {
    /// The options as `(label, description)`, in the order they are shown.
    pub fn options(&self) -> Vec<(String, String)> {
        match self {
            Question::Noul { .. } => vec![
                ("true".into(), "true".into()),
                ("false".into(), "false".into()),
            ],
            Question::Choice { criteria, .. } | Question::Score { criteria, .. } => {
                criteria.clone()
            }
        }
    }

    /// The question as the model reads it, with a letter per option.
    pub fn prompt(&self) -> String {
        let mut text = String::new();
        match self {
            Question::Noul { instructions } => {
                text.push_str("Is this statement true of the text above?\n");
                text.push_str(instructions);
                text.push_str("\n\nA) true\nB) false\n");
            }
            Question::Choice {
                instructions,
                criteria,
            }
            | Question::Score {
                instructions,
                criteria,
            } => {
                text.push_str(
                    instructions
                        .as_deref()
                        .unwrap_or("Which option best describes the text above?"),
                );
                text.push_str("\n\n");
                for (letter, (_, description)) in letters().zip(criteria) {
                    text.push_str(&format!("{letter}) {description}\n"));
                }
            }
        }
        text.push_str("\nAnswer with the letter of one option only.");
        text
    }

    fn validate(&self, id: &str) -> Result<(), String> {
        let n = self.options().len();
        if !(2..=MAX_OPTIONS).contains(&n) {
            return Err(format!(
                "question {id:?} has {n} options; 2 to {MAX_OPTIONS} are supported"
            ));
        }
        Ok(())
    }
}

fn letters() -> impl Iterator<Item = char> {
    ('A'..='Z').take(MAX_OPTIONS)
}

/// The option letters `A`, `B`, ... for `n` options.
pub fn option_letters(n: usize) -> Vec<String> {
    letters().take(n).map(String::from).collect()
}

/// A `system_one` request: the state and its questions, in order.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub state: String,
    pub questions: Vec<(String, Question)>,
}

impl Request {
    /// Parses TypeSafe's request shape. `choice` and `score` criteria are objects of
    /// `label: description`; `score` labels are ordered numerically when they are numbers.
    ///
    /// # Errors
    /// A message naming the first thing that is missing or malformed.
    pub fn from_json(value: &Value) -> Result<Self, String> {
        let state = value
            .get("state")
            .and_then(Value::as_str)
            .ok_or("`state` must be a string")?
            .to_string();
        let questions = value
            .get("questions")
            .and_then(Value::as_object)
            .ok_or("`questions` must be an object of id: question")?;
        if questions.is_empty() {
            return Err("at least one question is required".into());
        }
        let mut out = Vec::with_capacity(questions.len());
        for (id, q) in questions {
            let kind = q.get("type").and_then(Value::as_str).unwrap_or("");
            let instructions = q
                .get("instructions")
                .and_then(Value::as_str)
                .map(str::to_string);
            let criteria = || -> Result<Vec<(String, String)>, String> {
                let map = q
                    .get("criteria")
                    .and_then(Value::as_object)
                    .ok_or(format!("question {id:?}: `criteria` must be an object"))?;
                map.iter()
                    .map(|(label, d)| {
                        d.as_str()
                            .map(|d| (label.clone(), d.to_string()))
                            .ok_or(format!("question {id:?}: criterion {label:?} is not text"))
                    })
                    .collect()
            };
            let question = match kind {
                "noul" => Question::Noul {
                    instructions: instructions
                        .ok_or(format!("question {id:?}: noul needs `instructions`"))?,
                },
                "choice" => Question::Choice {
                    instructions,
                    criteria: criteria()?,
                },
                "score" => {
                    let mut criteria = criteria()?;
                    if criteria.iter().all(|(l, _)| l.parse::<f64>().is_ok()) {
                        criteria.sort_by(|a, b| {
                            let (x, y) = (a.0.parse::<f64>(), b.0.parse::<f64>());
                            x.unwrap_or(0.0).total_cmp(&y.unwrap_or(0.0))
                        });
                    }
                    Question::Score {
                        instructions,
                        criteria,
                    }
                }
                other => return Err(format!("question {id:?}: unknown type {other:?}")),
            };
            question.validate(id)?;
            out.push((id.clone(), question));
        }
        Ok(Self {
            state,
            questions: out,
        })
    }
}

/// A probability for every option of one question.
#[derive(Debug, Clone, PartialEq)]
pub struct Answer {
    /// `(label, probability)` in option order; the probabilities sum to 1.
    pub probabilities: Vec<(String, f32)>,
}

impl Answer {
    /// The most probable option and its probability.
    pub fn best(&self) -> (&str, f32) {
        self.probabilities
            .iter()
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(l, p)| (l.as_str(), *p))
            .unwrap_or(("", 0.0))
    }

    /// For a score question: the probability-weighted value of numeric labels.
    pub fn expected_value(&self) -> Option<f64> {
        self.probabilities
            .iter()
            .map(|(l, p)| l.parse::<f64>().ok().map(|v| v * f64::from(*p)))
            .sum()
    }

    pub fn to_json(&self) -> Value {
        let mut map = Map::new();
        for (label, p) in &self.probabilities {
            map.insert(label.clone(), json!(p));
        }
        Value::Object(map)
    }
}

/// How the model's raw option scores become probabilities: `softmax(logit / T)`.
/// Temperature scaling is the standard post-hoc calibration: it keeps the ranking and
/// makes confidence match accuracy on held-out examples ([`fit_temperature`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Calibration {
    pub temperature: f32,
}

impl Default for Calibration {
    fn default() -> Self {
        Self { temperature: 1.0 }
    }
}

impl Calibration {
    pub fn probabilities(&self, logits: &[f32]) -> Vec<f32> {
        softmax(logits, self.temperature)
    }
}

/// Numerically stable softmax of `logits / temperature`.
pub fn softmax(logits: &[f32], temperature: f32) -> Vec<f32> {
    let t = f64::from(temperature.max(1e-3));
    let max = logits.iter().fold(f32::NEG_INFINITY, |m, &v| m.max(v));
    let exps: Vec<f64> = logits
        .iter()
        .map(|&v| (f64::from(v - max) / t).exp())
        .collect();
    let sum: f64 = exps.iter().sum();
    exps.iter().map(|e| (e / sum) as f32).collect()
}

/// A source of next-token scores for option labels.
pub trait LabelModel {
    /// The model's log-scores (logits) for each of `labels` as the next token of its
    /// answer, after reading `state` and then `question`. Implementations should read
    /// `state` once and reuse it for consecutive calls with the same state.
    ///
    /// # Errors
    /// Whatever stopped the model; the message is passed on.
    fn label_logits(
        &mut self,
        state: &str,
        question: &str,
        labels: &[String],
    ) -> Result<Vec<f32>, String>;
}

/// Answers every question of `request` with `model`: one scoring pass per question.
///
/// # Errors
/// The first model error.
pub fn answer(
    model: &mut dyn LabelModel,
    request: &Request,
    calibration: Calibration,
) -> Result<Vec<(String, Answer)>, String> {
    let mut answers = Vec::with_capacity(request.questions.len());
    for (id, question) in &request.questions {
        let options = question.options();
        let letters = option_letters(options.len());
        let logits = model.label_logits(&request.state, &question.prompt(), &letters)?;
        if logits.len() != options.len() {
            return Err(format!(
                "model returned {} scores for {} options",
                logits.len(),
                options.len()
            ));
        }
        let probabilities = calibration.probabilities(&logits);
        answers.push((
            id.clone(),
            Answer {
                probabilities: options
                    .into_iter()
                    .map(|(label, _)| label)
                    .zip(probabilities)
                    .collect(),
            },
        ));
    }
    Ok(answers)
}

/// The response JSON: `{"answers": {id: {label: probability}}}`.
pub fn response_json(answers: &[(String, Answer)]) -> Value {
    let mut map = Map::new();
    for (id, answer) in answers {
        map.insert(id.clone(), answer.to_json());
    }
    json!({ "answers": map })
}

/// One labelled example for calibration: the raw option logits and the correct option.
#[derive(Debug, Clone, PartialEq)]
pub struct Example {
    pub logits: Vec<f32>,
    pub correct: usize,
}

/// Mean negative log-likelihood of the correct options at `temperature`.
pub fn nll(examples: &[Example], temperature: f32) -> f64 {
    let total: f64 = examples
        .iter()
        .map(|e| -f64::from(softmax(&e.logits, temperature)[e.correct].max(1e-12)).ln())
        .sum();
    total / examples.len().max(1) as f64
}

/// The temperature that minimises [`nll`] on `examples`, searched on a log scale over
/// 0.05..=20 (golden-section search; NLL is unimodal in log T for softmax).
pub fn fit_temperature(examples: &[Example]) -> f32 {
    if examples.is_empty() {
        return 1.0;
    }
    let f = |log_t: f64| nll(examples, log_t.exp() as f32);
    let (mut a, mut b) = (0.05f64.ln(), 20f64.ln());
    let g = (5f64.sqrt() - 1.0) / 2.0;
    let (mut c, mut d) = (b - g * (b - a), a + g * (b - a));
    let (mut fc, mut fd) = (f(c), f(d));
    for _ in 0..60 {
        if fc < fd {
            b = d;
            d = c;
            fd = fc;
            c = b - g * (b - a);
            fc = f(c);
        } else {
            a = c;
            c = d;
            fc = fd;
            d = a + g * (b - a);
            fd = f(d);
        }
    }
    (((a + b) / 2.0).exp()) as f32
}

/// How well confidence matches accuracy.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CalibrationReport {
    pub accuracy: f64,
    pub mean_confidence: f64,
    /// Expected calibration error over 10 equal-width confidence bins.
    pub ece: f64,
    /// Mean Brier score over all options.
    pub brier: f64,
    pub nll: f64,
}

/// Scores `examples` at `temperature`.
pub fn calibration_report(examples: &[Example], temperature: f32) -> CalibrationReport {
    let n = examples.len().max(1) as f64;
    let mut bins = [(0usize, 0f64, 0f64); 10]; // count, confidence sum, correct sum
    let (mut correct, mut confidence, mut brier) = (0f64, 0f64, 0f64);
    for e in examples {
        let p = softmax(&e.logits, temperature);
        let (best, conf) = p
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(i, &c)| (i, f64::from(c)))
            .unwrap_or((0, 0.0));
        let hit = f64::from(u8::from(best == e.correct));
        correct += hit;
        confidence += conf;
        let bin = ((conf * 10.0) as usize).min(9);
        bins[bin].0 += 1;
        bins[bin].1 += conf;
        bins[bin].2 += hit;
        brier += p
            .iter()
            .enumerate()
            .map(|(i, &q)| {
                let y = f64::from(u8::from(i == e.correct));
                (f64::from(q) - y).powi(2)
            })
            .sum::<f64>();
    }
    let ece = bins
        .iter()
        .filter(|b| b.0 > 0)
        .map(|b| (b.0 as f64 / n) * (b.1 / b.0 as f64 - b.2 / b.0 as f64).abs())
        .sum();
    CalibrationReport {
        accuracy: correct / n,
        mean_confidence: confidence / n,
        ece,
        brier: brier / n,
        nll: nll(examples, temperature),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_parse_in_typesafe_shape_and_scores_order_numerically() {
        let request = Request::from_json(&json!({
            "state": "The drive is 98% full.",
            "questions": {
                "urgent": {"type": "noul", "instructions": "The drive needs attention soon."},
                "action": {"type": "choice", "criteria": {
                    "keep": "Leave it alone.", "archive": "Archive, then remove."}},
                "risk": {"type": "score", "criteria": {
                    "10": "Very risky.", "2": "Some risk.", "0": "No risk."}}
            }
        }))
        .unwrap();
        assert_eq!(request.questions.len(), 3);
        let risk = &request
            .questions
            .iter()
            .find(|(id, _)| id == "risk")
            .unwrap()
            .1;
        let labels: Vec<String> = risk.options().into_iter().map(|(l, _)| l).collect();
        assert_eq!(labels, ["0", "2", "10"]);
    }

    #[test]
    fn malformed_requests_are_refused_with_the_reason() {
        let bad = |v: Value| Request::from_json(&v).unwrap_err();
        assert!(bad(json!({"questions": {}})).contains("state"));
        assert!(bad(json!({"state": "s", "questions": {}})).contains("at least one"));
        assert!(
            bad(json!({"state": "s", "questions": {"q": {"type": "noul"}}}))
                .contains("instructions")
        );
        assert!(
            bad(json!({"state": "s", "questions": {"q": {"type": "choice",
            "criteria": {"only": "one"}}}}))
            .contains("2 to 26")
        );
        assert!(
            bad(json!({"state": "s", "questions": {"q": {"type": "maybe"}}}))
                .contains("unknown type")
        );
    }

    #[test]
    fn prompts_letter_every_option() {
        let q = Question::Choice {
            instructions: Some("What should happen?".into()),
            criteria: vec![
                ("keep".into(), "Leave it.".into()),
                ("drop".into(), "Remove it.".into()),
            ],
        };
        let text = q.prompt();
        assert!(text.contains("A) Leave it.\nB) Remove it."));
        assert!(text.ends_with("Answer with the letter of one option only."));
        assert_eq!(option_letters(3), ["A", "B", "C"]);
    }

    struct Fixed(Vec<f32>);
    impl LabelModel for Fixed {
        fn label_logits(
            &mut self,
            _: &str,
            _: &str,
            labels: &[String],
        ) -> Result<Vec<f32>, String> {
            Ok(self.0[..labels.len()].to_vec())
        }
    }

    #[test]
    fn answers_are_distributions_over_the_declared_labels() {
        let request = Request::from_json(&json!({"state": "s", "questions": {
            "q": {"type": "score", "criteria": {"1": "low", "2": "mid", "3": "high"}}}}))
        .unwrap();
        let answers = answer(
            &mut Fixed(vec![0.0, 1.0, 3.0]),
            &request,
            Calibration::default(),
        )
        .unwrap();
        let a = &answers[0].1;
        let sum: f32 = a.probabilities.iter().map(|p| p.1).sum();
        assert!((sum - 1.0).abs() < 1e-6);
        assert_eq!(a.best().0, "3");
        assert!(a.expected_value().unwrap() > 2.5);
        let json = response_json(&answers);
        assert!(json["answers"]["q"]["3"].as_f64().unwrap() > 0.8);
    }

    #[test]
    fn temperature_fitting_recovers_a_known_temperature() {
        // Logits are 3x too confident: the true probabilities are softmax(logits / 3).
        let mut examples = Vec::new();
        let mut seed = 7u64;
        let mut rand = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 11) as f64 / (1u64 << 53) as f64
        };
        for _ in 0..4000 {
            let logits: Vec<f32> = (0..4).map(|_| (rand() * 12.0) as f32).collect();
            let p = softmax(&logits, 3.0);
            let (mut u, mut correct) = (rand() as f32, 3);
            for (i, &q) in p.iter().enumerate() {
                if u < q {
                    correct = i;
                    break;
                }
                u -= q;
            }
            examples.push(Example { logits, correct });
        }
        let t = fit_temperature(&examples);
        assert!((t - 3.0).abs() < 0.3, "fitted {t}");
        let before = calibration_report(&examples, 1.0);
        let after = calibration_report(&examples, t);
        assert!(after.ece < before.ece && after.nll < before.nll);
        assert!(after.ece < 0.03, "ece {}", after.ece);
    }
}
