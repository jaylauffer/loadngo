//! Pluggable Task rewards: matching a submitter's offered schemes to a worker's
//! payees, and running the operator's settler and verifier commands.
//!
//! loadngo links no reward scheme. A scheme is an external command named by the
//! operator (`--reward <scheme>=<command>` on the submitter,
//! `--reward-verify <scheme>=<command>` on a worker); the contract is below and in
//! `docs/TASK_REWARD_FLOW.md`. Accepting work never depends on any of this.
//!
//! Settler: reads a [`SettleRequest`] as JSON on stdin, writes a
//! [`RewardSettlement`] as JSON on stdout and exits 0. It should answer within
//! `wait_seconds`, reporting `pending` if the reward is not final by then; the
//! submitter stops waiting [`SETTLER_GRACE`] after that and reports `failed`.
//!
//! Verifier: reads a [`RewardSettlement`] as JSON on stdin and exits 0 when its
//! reference is real. It may write an updated [`RewardSettlement`] on stdout, for
//! example `settled` for a reference that was `pending`.

use crate::task_runtime::{reward_receipt_commitment, RewardReceipt};
use anyhow::{anyhow, Context, Result};
pub use data::p2pmsg::{RewardPayee, RewardSettlement, RewardState, RewardTerms};
use loadngo_proactor::{ChannelPort, CompletionKind, Proactor};
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

/// How long the submitter waits for a settlement before sending `TaskAck` (Jay,
/// 2026-10-06). It must stay well inside a worker's ack timeout (`task-node`
/// default 90 s). QCoin makes a block every 5 s, so this is six blocks.
pub const DEFAULT_SETTLE_WAIT: Duration = Duration::from_secs(30);

/// Extra time a settler gets past `wait_seconds` to write its answer and exit.
pub const SETTLER_GRACE: Duration = Duration::from_secs(5);

/// How long a worker's verifier may run.
pub const VERIFY_DEADLINE: Duration = Duration::from_secs(30);

/// What a settler reads on stdin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettleRequest {
    pub scheme: String,
    pub payee: String,
    /// The completion receipt being rewarded.
    pub receipt: RewardReceipt,
    /// The receipt's anchor commitment (`reward_receipt_commitment`), hex: what a
    /// ledger records, so a settler need not know the framing.
    pub commitment_hex: String,
    /// Answer within this many seconds, `pending` if the reward is not final.
    pub wait_seconds: u64,
}

impl SettleRequest {
    pub fn new(payee: &RewardPayee, receipt: &RewardReceipt, wait: Duration) -> Result<Self> {
        Ok(Self {
            scheme: payee.scheme.clone(),
            payee: payee.payee.clone(),
            receipt: receipt.clone(),
            commitment_hex: hex::encode(reward_receipt_commitment(receipt)?),
            wait_seconds: wait.as_secs(),
        })
    }
}

/// An operator's command for one scheme, from `<scheme>=<command>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemeCommand {
    pub scheme: String,
    pub command: String,
}

/// Parses a `<scheme>=<value>` flag value (`--reward`, `--reward-payee`,
/// `--reward-verify`).
pub fn parse_scheme_value(flag: &str, value: &str) -> Result<(String, String)> {
    let (scheme, rest) = value
        .split_once('=')
        .ok_or_else(|| anyhow!("expected {flag} as <scheme>=<value>, got {value:?}"))?;
    let scheme = scheme.trim();
    if scheme.is_empty() || rest.is_empty() {
        return Err(anyhow!(
            "expected {flag} as <scheme>=<value>, got {value:?}"
        ));
    }
    Ok((scheme.to_string(), rest.to_string()))
}

/// A worker operator's reward configuration, from `--reward-payee <scheme>=<payee>`
/// and `--reward-verify <scheme>=<command>`. Empty means the worker takes only
/// unrewarded work, which it still does.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkerRewards {
    pub payees: Vec<RewardPayee>,
    pub verifiers: Vec<SchemeCommand>,
}

impl WorkerRewards {
    /// Consumes the value of `--reward-payee` or `--reward-verify`; returns false for
    /// any other flag.
    pub fn parse_flag(
        &mut self,
        flag: &str,
        args: &mut impl Iterator<Item = String>,
    ) -> Result<bool> {
        if flag != "--reward-payee" && flag != "--reward-verify" {
            return Ok(false);
        }
        let value = args
            .next()
            .ok_or_else(|| anyhow!("missing value for {flag}"))?;
        let (scheme, value) = parse_scheme_value(flag, &value)?;
        if flag == "--reward-payee" {
            if self.payees.iter().any(|payee| payee.scheme == scheme) {
                return Err(anyhow!("--reward-payee given twice for scheme {scheme}"));
            }
            self.payees.push(RewardPayee {
                scheme,
                payee: value,
            });
        } else {
            if self
                .verifiers
                .iter()
                .any(|verifier| verifier.scheme == scheme)
            {
                return Err(anyhow!("--reward-verify given twice for scheme {scheme}"));
            }
            self.verifiers.push(SchemeCommand {
                scheme,
                command: value,
            });
        }
        Ok(true)
    }

    pub fn verifier(&self, scheme: &str) -> Option<&str> {
        self.verifiers
            .iter()
            .find(|verifier| verifier.scheme == scheme)
            .map(|verifier| verifier.command.as_str())
    }
}

/// One log field for a settlement: `none`, or `<scheme>:<state>[:<reference>]`.
pub fn describe_settlement(settlement: Option<&RewardSettlement>) -> String {
    match settlement {
        None => "none".to_string(),
        Some(settlement) => {
            let state = match settlement.state {
                RewardState::Settled => "settled",
                RewardState::Pending => "pending",
                RewardState::Failed => "failed",
            };
            match settlement.reference.as_deref() {
                Some(reference) => format!("{}:{state}:{reference}", settlement.scheme),
                None => format!("{}:{state}", settlement.scheme),
            }
        }
    }
}

/// The reward for an assignment: the first scheme, in the submitter's order, that the
/// worker has a payee for. `None` means the work is unrewarded, which is still work.
pub fn choose_reward(offers: &[RewardTerms], payees: &[RewardPayee]) -> Option<RewardPayee> {
    offers.iter().find_map(|offer| {
        payees
            .iter()
            .find(|payee| payee.scheme == offer.scheme)
            .cloned()
    })
}

/// Runs the settler for an agreed reward and always returns a settlement: a settler
/// that fails, overruns or answers for another scheme gives `failed`, never an error,
/// so `TaskAck` is still sent.
pub fn settle(command: &str, request: &SettleRequest) -> RewardSettlement {
    let failed = |note: String| RewardSettlement {
        scheme: request.scheme.clone(),
        state: RewardState::Failed,
        reference: None,
        note: Some(note),
    };
    let input = match serde_json::to_vec(request) {
        Ok(input) => input,
        Err(err) => return failed(format!("could not encode the settle request: {err}")),
    };
    let deadline = Duration::from_secs(request.wait_seconds) + SETTLER_GRACE;
    let output = match run_with_deadline(command, input, deadline) {
        CommandOutcome::Finished(output) => output,
        CommandOutcome::TimedOut => {
            return failed(format!("settler gave no answer within {deadline:?}"))
        }
        CommandOutcome::NotRun(err) => return failed(format!("settler did not run: {err}")),
    };
    if !output.status.success() {
        return failed(format!(
            "settler exited with {}: {}",
            output.status,
            stderr_line(&output.stderr)
        ));
    }
    match serde_json::from_slice::<RewardSettlement>(&output.stdout) {
        Ok(settlement) if settlement.scheme == request.scheme => settlement,
        Ok(settlement) => failed(format!(
            "settler answered for scheme {:?}, not {:?}",
            settlement.scheme, request.scheme
        )),
        Err(err) => failed(format!("settler output is not a settlement: {err}")),
    }
}

/// What a worker's verifier said about a settlement it was given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verification {
    /// The verifier exited 0: the reference is real.
    pub real: bool,
    /// The settlement as the verifier now sees it, if it wrote one.
    pub updated: Option<RewardSettlement>,
    pub note: Option<String>,
}

pub fn verify(command: &str, settlement: &RewardSettlement) -> Verification {
    let not_real = |note: String| Verification {
        real: false,
        updated: None,
        note: Some(note),
    };
    let input = match serde_json::to_vec(settlement) {
        Ok(input) => input,
        Err(err) => return not_real(format!("could not encode the settlement: {err}")),
    };
    match run_with_deadline(command, input, VERIFY_DEADLINE) {
        CommandOutcome::Finished(output) => Verification {
            real: output.status.success(),
            updated: serde_json::from_slice(&output.stdout).ok(),
            note: (!output.status.success()).then(|| {
                format!(
                    "verifier exited with {}: {}",
                    output.status,
                    stderr_line(&output.stderr)
                )
            }),
        },
        CommandOutcome::TimedOut => not_real(format!(
            "verifier gave no answer within {VERIFY_DEADLINE:?}"
        )),
        CommandOutcome::NotRun(err) => not_real(format!("verifier did not run: {err}")),
    }
}

fn stderr_line(stderr: &[u8]) -> String {
    String::from_utf8_lossy(stderr)
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .to_string()
}

/// The shell the Task binaries run operator commands through.
pub fn shell_command(script: &str) -> Command {
    #[cfg(unix)]
    {
        let mut command = Command::new("sh");
        command.arg("-lc").arg(script);
        command
    }
    #[cfg(windows)]
    {
        // Raw: `arg` would quote the script and escape its quotes as `\"`, which cmd
        // does not understand, so a command containing quotes would arrive mangled.
        use std::os::windows::process::CommandExt;
        let mut command = Command::new("cmd");
        command.arg("/C").raw_arg(script);
        command
    }
}

pub struct CommandOutput {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

pub enum CommandOutcome {
    Finished(CommandOutput),
    TimedOut,
    NotRun(String),
}

/// Runs `script` with `input` on stdin, offloaded to a worker thread, and waits on a
/// proactor for whichever completes first: the command, or a deadline timer that
/// kills it. Nothing polls.
pub fn run_with_deadline(script: &str, input: Vec<u8>, deadline: Duration) -> CommandOutcome {
    let mut child = match shell_command(script)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(err) => return CommandOutcome::NotRun(err.to_string()),
    };
    let stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let child = Arc::new(Mutex::new(child));

    let proactor = Proactor::new(ChannelPort::new());
    let handle = proactor.handle();
    let outcome: Arc<Mutex<Option<CommandOutcome>>> = Arc::new(Mutex::new(None));

    {
        let child = Arc::clone(&child);
        let outcome = Arc::clone(&outcome);
        let handle_for_work = handle.clone();
        thread::spawn(move || {
            let result = collect(&child, stdin, input, stdout, stderr);
            let stop = handle_for_work.clone();
            // After a timeout the proactor is gone and this post fails; the outcome
            // is already decided.
            let _ = handle_for_work.enqueue_work(move |_| {
                let mut slot = outcome.lock().expect("command outcome lock poisoned");
                if slot.is_none() {
                    *slot = Some(match result {
                        Ok(output) => CommandOutcome::Finished(output),
                        Err(err) => CommandOutcome::NotRun(err.to_string()),
                    });
                }
                let _ = stop.stop();
            });
        });
    }
    {
        let child = Arc::clone(&child);
        let outcome = Arc::clone(&outcome);
        let stop = handle.clone();
        let armed = handle.defer_for(deadline, CompletionKind::Timer, 0, move |_| {
            let mut slot = outcome.lock().expect("command outcome lock poisoned");
            if slot.is_none() {
                *slot = Some(CommandOutcome::TimedOut);
                // The collector holds the lock only once the command has closed its
                // output; then there is nothing left worth killing.
                if let Ok(mut child) = child.try_lock() {
                    let _ = child.kill();
                }
            }
            let _ = stop.stop();
        });
        if let Err(err) = armed {
            return CommandOutcome::NotRun(format!("could not arm the deadline: {err}"));
        }
    }

    if let Err(err) = proactor.run_until_stopped() {
        return CommandOutcome::NotRun(format!("proactor failed: {err}"));
    }
    let outcome = outcome
        .lock()
        .expect("command outcome lock poisoned")
        .take();
    outcome.unwrap_or(CommandOutcome::TimedOut)
}

fn collect(
    child: &Mutex<Child>,
    stdin: Option<std::process::ChildStdin>,
    input: Vec<u8>,
    stdout: Option<std::process::ChildStdout>,
    stderr: Option<std::process::ChildStderr>,
) -> Result<CommandOutput> {
    // stdin and stderr on their own threads so a command that writes a lot before
    // reading, or fills stderr, cannot deadlock against us.
    let writer = thread::spawn(move || {
        if let Some(mut stdin) = stdin {
            // A command that exits without reading its input is not an error here.
            let _ = stdin.write_all(&input);
        }
    });
    let err_reader = thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut stderr) = stderr {
            let _ = stderr.read_to_end(&mut buf);
        }
        buf
    });
    let mut out = Vec::new();
    if let Some(mut stdout) = stdout {
        stdout
            .read_to_end(&mut out)
            .context("reading the command's stdout")?;
    }
    let _ = writer.join();
    let err = err_reader.join().unwrap_or_default();
    let status = child
        .lock()
        .expect("child lock poisoned")
        .wait()
        .context("waiting for the command")?;
    Ok(CommandOutput {
        status,
        stdout: out,
        stderr: err,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terms(scheme: &str) -> RewardTerms {
        RewardTerms {
            scheme: scheme.to_string(),
            terms: None,
        }
    }

    fn payee(scheme: &str, payee: &str) -> RewardPayee {
        RewardPayee {
            scheme: scheme.to_string(),
            payee: payee.to_string(),
        }
    }

    fn receipt() -> RewardReceipt {
        RewardReceipt {
            receipt_version: crate::task_runtime::REWARD_RECEIPT_VERSION as u32,
            request_id: 1,
            offer_id: 2,
            assignment_id: 3,
            submitter_node_id: "submitter".to_string(),
            worker_node_id: "worker".to_string(),
            summary: "s".to_string(),
            success_criteria: None,
            artifact_hint: None,
            artifact_copy_path: None,
            artifact_hash_hex: None,
            result_note: None,
            accepted_at: 5,
            submitted_at: 4,
            reward: Some(payee("test", "p")),
        }
    }

    fn request(wait_seconds: u64) -> SettleRequest {
        SettleRequest::new(
            &payee("test", "p"),
            &receipt(),
            Duration::from_secs(wait_seconds),
        )
        .unwrap()
    }

    // Commands that write a fixed line, or sleep, in each platform's shell.
    #[cfg(unix)]
    fn echo(text: &str) -> String {
        format!("cat >/dev/null; printf '%s' '{text}'")
    }
    #[cfg(windows)]
    fn echo(text: &str) -> String {
        format!("echo {text}")
    }
    #[cfg(unix)]
    const SLOW: &str = "sleep 30";
    #[cfg(windows)]
    const SLOW: &str = "ping -n 30 127.0.0.1 >nul";
    #[cfg(unix)]
    const FAIL: &str = "echo broken >&2; exit 3";
    #[cfg(windows)]
    const FAIL: &str = "echo broken 1>&2 & exit /b 3";

    #[test]
    fn scheme_values_parse_and_reject_missing_parts() {
        assert_eq!(
            parse_scheme_value("--reward", "qcoin=qcoin-node task-reward settle").unwrap(),
            (
                "qcoin".to_string(),
                "qcoin-node task-reward settle".to_string()
            )
        );
        assert!(parse_scheme_value("--reward", "qcoin").is_err());
        assert!(parse_scheme_value("--reward", "=x").is_err());
        assert!(parse_scheme_value("--reward", "qcoin=").is_err());
    }

    #[test]
    fn worker_reward_flags_parse_and_refuse_duplicates() {
        let mut rewards = WorkerRewards::default();
        let mut rest = vec!["qcoin=ab".to_string(), "qcoin=verify-it".to_string()].into_iter();
        assert!(rewards.parse_flag("--reward-payee", &mut rest).unwrap());
        assert!(rewards.parse_flag("--reward-verify", &mut rest).unwrap());
        assert!(!rewards.parse_flag("--note", &mut rest).unwrap());
        assert_eq!(rewards.payees, [payee("qcoin", "ab")]);
        assert_eq!(rewards.verifier("qcoin"), Some("verify-it"));
        assert_eq!(rewards.verifier("other"), None);
        let mut again = vec!["qcoin=cd".to_string()].into_iter();
        assert!(rewards.parse_flag("--reward-payee", &mut again).is_err());
    }

    #[test]
    fn reward_is_the_first_offered_scheme_the_worker_takes() {
        let offers = [terms("credit"), terms("qcoin")];
        let payees = [payee("qcoin", "a"), payee("credit", "b")];
        assert_eq!(choose_reward(&offers, &payees), Some(payee("credit", "b")));
        assert_eq!(
            choose_reward(&offers[1..], &payees),
            Some(payee("qcoin", "a"))
        );
        // No overlap, or either side with nothing configured, is unrewarded work.
        assert_eq!(choose_reward(&[terms("other")], &payees), None);
        assert_eq!(choose_reward(&[], &payees), None);
        assert_eq!(choose_reward(&offers, &[]), None);
    }

    #[test]
    fn settler_answer_is_returned() {
        let settlement = settle(
            &echo(r#"{"scheme":"test","state":"pending","reference":"ref-1"}"#),
            &request(1),
        );
        assert_eq!(settlement.state, RewardState::Pending);
        assert_eq!(settlement.reference.as_deref(), Some("ref-1"));
    }

    #[cfg(unix)]
    #[test]
    fn settler_reads_the_request_on_stdin() {
        let dir = tempfile::tempdir().unwrap();
        let seen = dir.path().join("request.json");
        let command = format!(
            "cat > '{}'; printf '%s' '{{\"scheme\":\"test\",\"state\":\"settled\"}}'",
            seen.display()
        );
        let request = request(7);
        let settlement = settle(&command, &request);
        assert_eq!(settlement.state, RewardState::Settled, "{settlement:?}");
        let seen: SettleRequest = serde_json::from_slice(&std::fs::read(&seen).unwrap()).unwrap();
        assert_eq!(seen, request);
        assert_eq!(seen.wait_seconds, 7);
        assert_eq!(seen.commitment_hex.len(), 64);
    }

    #[test]
    fn a_failing_settler_gives_failed_not_an_error() {
        let settlement = settle(FAIL, &request(1));
        assert_eq!(settlement.state, RewardState::Failed);
        assert!(settlement.note.unwrap().contains("broken"));
    }

    #[test]
    fn a_settler_answering_for_another_scheme_is_failed() {
        let settlement = settle(
            &echo(r#"{"scheme":"other","state":"settled"}"#),
            &request(1),
        );
        assert_eq!(settlement.state, RewardState::Failed);
    }

    #[test]
    fn garbage_settler_output_is_failed() {
        let settlement = settle(&echo("not json"), &request(1));
        assert_eq!(settlement.state, RewardState::Failed);
    }

    #[test]
    fn a_slow_settler_is_stopped_at_its_deadline() {
        let started = std::time::Instant::now();
        let outcome = run_with_deadline(SLOW, Vec::new(), Duration::from_millis(300));
        assert!(matches!(outcome, CommandOutcome::TimedOut));
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn verify_reports_real_and_the_updated_settlement() {
        let pending = RewardSettlement {
            scheme: "test".to_string(),
            state: RewardState::Pending,
            reference: Some("ref-1".to_string()),
            note: None,
        };
        let verified = verify(
            &echo(r#"{"scheme":"test","state":"settled","reference":"ref-1"}"#),
            &pending,
        );
        assert!(verified.real);
        assert_eq!(verified.updated.unwrap().state, RewardState::Settled);

        let refused = verify(FAIL, &pending);
        assert!(!refused.real);
        assert!(refused.note.unwrap().contains("broken"));
    }
}
