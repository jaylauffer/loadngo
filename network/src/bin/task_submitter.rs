use anyhow::{anyhow, bail, Context, Result};
use data::{
    generate_id,
    model_utils::now_timestamp,
    p2pmsg::{
        Message, RewardSettlement, RewardState, RewardTerms, TaskAccept, TaskAck, TaskOffer,
        TaskRequest, TaskResult,
    },
};
use network::{
    inbox::Inbox,
    task_reward::{
        choose_reward, parse_scheme_value, settle, shell_command, SchemeCommand, SettleRequest,
        DEFAULT_SETTLE_WAIT,
    },
    task_runtime::{
        artifact_hash_hex, endpoint_host, parse_multicast_v4, parse_multicast_v6,
        reward_receipt_commitment, task_network_config, RewardReceipt, Verification,
        REWARD_RECEIPT_VERSION,
    },
    Network,
};
use serde::Serialize;
use std::{
    collections::HashMap,
    env, fs,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

struct Args {
    bind_port: u16,
    multicast_port: Option<u16>,
    submitter_node_id: String,
    reply_endpoints: Vec<String>,
    request_targets: Vec<String>,
    summary: String,
    capability_tags: Vec<String>,
    requested_duration_secs: Option<u64>,
    success_criteria: Option<String>,
    artifact_hint: Option<String>,
    note: Option<String>,
    offer_timeout_seconds: u64,
    status_check_interval_secs: u64,
    expected_duration_secs: Option<u64>,
    result_timeout_seconds: u64,
    select_worker_node_id: Option<String>,
    verify_command: String,
    artifact_copy_path: Option<PathBuf>,
    receipt_path: Option<PathBuf>,
    rewards: Vec<SchemeCommand>,
    settle_wait_seconds: u64,
    multicast_v6: Vec<(Ipv6Addr, u32)>,
    multicast_v4: Vec<(Ipv4Addr, Ipv4Addr)>,
}

#[derive(Clone)]
struct SeenOffer {
    source: SocketAddr,
    offer: TaskOffer,
}

/// Written for every result the submitter checked, accepted or not: what its check
/// printed and, for accepted work, the completion receipt, its commitment and the reward
/// settlement if one was agreed.
#[derive(Debug, Serialize)]
struct ClosureRecord {
    accepted: bool,
    receipt: Option<RewardReceipt>,
    receipt_commitment_hex: Option<String>,
    reward: Option<RewardSettlement>,
    verification: Verification,
}

impl Args {
    fn parse() -> Result<Self> {
        let mut bind_port = 9850u16;
        let mut multicast_port = None;
        let mut submitter_node_id = None;
        let mut reply_endpoints = Vec::new();
        let mut request_targets = Vec::new();
        let mut summary = None;
        let mut capability_tags = Vec::new();
        let mut requested_duration_secs = None;
        let mut success_criteria = None;
        let mut artifact_hint = None;
        let mut note = None;
        let mut offer_timeout_seconds = 15u64;
        let mut status_check_interval_secs = 30u64;
        let mut expected_duration_secs = None;
        let mut result_timeout_seconds = 180u64;
        let mut select_worker_node_id = None;
        let mut verify_command = None;
        let mut artifact_copy_path = None;
        let mut receipt_path = None;
        let mut rewards: Vec<SchemeCommand> = Vec::new();
        let mut settle_wait_seconds = DEFAULT_SETTLE_WAIT.as_secs();
        let mut multicast_v6 = Vec::new();
        let mut multicast_v4 = Vec::new();

        let mut args = env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--bind-port" => {
                    bind_port = args
                        .next()
                        .context("missing value for --bind-port")?
                        .parse()
                        .context("invalid --bind-port")?;
                }
                "--multicast-port" => {
                    multicast_port = Some(
                        args.next()
                            .context("missing value for --multicast-port")?
                            .parse()
                            .context("invalid --multicast-port")?,
                    );
                }
                "--submitter-node-id" => {
                    submitter_node_id = Some(
                        args.next()
                            .context("missing value for --submitter-node-id")?,
                    );
                }
                "--reply-endpoint" => {
                    reply_endpoints
                        .push(args.next().context("missing value for --reply-endpoint")?);
                }
                "--request-target" => {
                    request_targets
                        .push(args.next().context("missing value for --request-target")?);
                }
                "--summary" => {
                    summary = Some(args.next().context("missing value for --summary")?);
                }
                "--capability" => {
                    capability_tags.push(args.next().context("missing value for --capability")?);
                }
                "--requested-duration-seconds" => {
                    requested_duration_secs = Some(
                        args.next()
                            .context("missing value for --requested-duration-seconds")?
                            .parse()
                            .context("invalid --requested-duration-seconds")?,
                    );
                }
                "--success-criteria" => {
                    success_criteria = Some(
                        args.next()
                            .context("missing value for --success-criteria")?,
                    );
                }
                "--artifact-hint" => {
                    artifact_hint = Some(args.next().context("missing value for --artifact-hint")?);
                }
                "--note" => {
                    note = Some(args.next().context("missing value for --note")?);
                }
                "--offer-timeout-seconds" => {
                    offer_timeout_seconds = args
                        .next()
                        .context("missing value for --offer-timeout-seconds")?
                        .parse()
                        .context("invalid --offer-timeout-seconds")?;
                }
                "--status-check-interval-seconds" => {
                    status_check_interval_secs = args
                        .next()
                        .context("missing value for --status-check-interval-seconds")?
                        .parse()
                        .context("invalid --status-check-interval-seconds")?;
                }
                "--expected-duration-seconds" => {
                    expected_duration_secs = Some(
                        args.next()
                            .context("missing value for --expected-duration-seconds")?
                            .parse()
                            .context("invalid --expected-duration-seconds")?,
                    );
                }
                "--result-timeout-seconds" => {
                    result_timeout_seconds = args
                        .next()
                        .context("missing value for --result-timeout-seconds")?
                        .parse()
                        .context("invalid --result-timeout-seconds")?;
                }
                "--select-worker-node-id" => {
                    select_worker_node_id = Some(
                        args.next()
                            .context("missing value for --select-worker-node-id")?,
                    );
                }
                "--verify-command" => {
                    verify_command =
                        Some(args.next().context("missing value for --verify-command")?);
                }
                "--artifact-copy-path" => {
                    artifact_copy_path = Some(PathBuf::from(
                        args.next()
                            .context("missing value for --artifact-copy-path")?,
                    ));
                }
                "--receipt-path" => {
                    receipt_path = Some(PathBuf::from(
                        args.next().context("missing value for --receipt-path")?,
                    ));
                }
                "--reward" => {
                    let value = args.next().context("missing value for --reward")?;
                    let (scheme, command) = parse_scheme_value("--reward", &value)?;
                    if rewards.iter().any(|reward| reward.scheme == scheme) {
                        bail!("--reward given twice for scheme {scheme}");
                    }
                    rewards.push(SchemeCommand { scheme, command });
                }
                "--reward-settle-seconds" => {
                    settle_wait_seconds = args
                        .next()
                        .context("missing value for --reward-settle-seconds")?
                        .parse()
                        .context("invalid --reward-settle-seconds")?;
                }
                "--multicast-v6" => {
                    let value = args.next().context("missing value for --multicast-v6")?;
                    multicast_v6.push(parse_multicast_v6(&value)?);
                }
                "--multicast-v4" => {
                    let value = args.next().context("missing value for --multicast-v4")?;
                    multicast_v4.push(parse_multicast_v4(&value)?);
                }
                "--help" | "-h" => {
                    print_usage();
                    std::process::exit(0);
                }
                other => bail!("unknown argument: {other}"),
            }
        }

        Ok(Self {
            bind_port,
            multicast_port,
            submitter_node_id: submitter_node_id
                .ok_or_else(|| anyhow!("--submitter-node-id is required"))?,
            reply_endpoints,
            request_targets,
            summary: summary.ok_or_else(|| anyhow!("--summary is required"))?,
            capability_tags,
            requested_duration_secs,
            success_criteria,
            artifact_hint,
            note,
            offer_timeout_seconds,
            status_check_interval_secs,
            expected_duration_secs,
            result_timeout_seconds,
            select_worker_node_id,
            verify_command: verify_command
                .ok_or_else(|| anyhow!("--verify-command is required"))?,
            artifact_copy_path,
            receipt_path,
            rewards,
            settle_wait_seconds,
            multicast_v6,
            multicast_v4,
        })
    }
}

fn main() -> Result<()> {
    let args = Args::parse()?;
    if args.reply_endpoints.is_empty() {
        bail!("at least one --reply-endpoint is required");
    }
    if args.multicast_v6.is_empty() && args.multicast_v4.is_empty() {
        bail!("at least one multicast target is required");
    }

    let mut config = task_network_config(args.bind_port, &args.multicast_v6, &args.multicast_v4);
    config.multicast_target_port = args.multicast_port;
    let mut network = Network::with_config(config);
    network.init()?;
    let mut inbox = Inbox::new(&network)?;

    let created_at = now_timestamp();
    let request = TaskRequest {
        request_id: generate_id(),
        submitter_node_id: args.submitter_node_id.clone(),
        created_at,
        expires_at: created_at.saturating_add(args.offer_timeout_seconds),
        summary: args.summary.clone(),
        capability_tags: args.capability_tags.clone(),
        reply_endpoints: args.reply_endpoints.clone(),
        requested_duration_secs: args.requested_duration_secs,
        success_criteria: args.success_criteria.clone(),
        artifact_hint: args.artifact_hint.clone(),
        note: args.note.clone(),
        reward_offers: args
            .rewards
            .iter()
            .map(|reward| RewardTerms {
                scheme: reward.scheme.clone(),
                terms: None,
            })
            .collect(),
    };

    let mut sent =
        network.send_p2p_multicast_message(Message::TaskRequest(request.clone()), false)?;
    for target in &args.request_targets {
        sent += network.send_p2p_message(target, Message::TaskRequest(request.clone()), false)?;
    }
    println!(
        "task_submitter_request_sent request_id={} submitter_node_id={} bytes={} expires_at={}",
        request.request_id, request.submitter_node_id, sent, request.expires_at
    );

    let offers = collect_offers(&mut inbox, &request, &args)?;
    let selected = select_offer(&offers, args.select_worker_node_id.as_deref())?;
    let reward = choose_reward(&request.reward_offers, &selected.offer.reward_payees);
    let accepted_at = now_timestamp();
    let expected_delivery_by = args
        .expected_duration_secs
        .or(selected.offer.estimated_duration_secs)
        .map(|duration| accepted_at.saturating_add(duration));
    let accept = TaskAccept {
        assignment_id: generate_id(),
        request_id: request.request_id,
        offer_id: selected.offer.offer_id,
        submitter_node_id: args.submitter_node_id.clone(),
        worker_node_id: selected.offer.worker_node_id.clone(),
        accepted_at,
        status_check_interval_secs: args.status_check_interval_secs,
        expected_duration_secs: args
            .expected_duration_secs
            .or(selected.offer.estimated_duration_secs),
        expected_delivery_by,
        submitter_reply_endpoint: Some(args.reply_endpoints[0].clone()),
        success_criteria: args.success_criteria.clone(),
        artifact_hint: args
            .artifact_hint
            .clone()
            .or_else(|| selected.offer.artifact_hint.clone()),
        note: Some("selected for feedback task execution".to_string()),
        reward: reward.clone(),
    };

    for target in &selected.offer.reply_endpoints {
        network.send_p2p_message(target, Message::TaskAccept(accept.clone()), false)?;
    }
    println!(
        "task_submitter_accept_sent assignment_id={} request_id={} offer_id={} worker_node_id={} reward={}",
        accept.assignment_id,
        accept.request_id,
        accept.offer_id,
        accept.worker_node_id,
        reward
            .as_ref()
            .map(|reward| format!("{}:{}", reward.scheme, reward.payee))
            .unwrap_or_else(|| "none".to_string())
    );

    let result = wait_for_result(&mut inbox, &accept, selected, &args)?;
    let verify_output = run_verify_command(&args, &accept, selected, &result)?;
    let verification = Verification::new(&args.verify_command, &verify_output);
    for (stream, text) in [
        ("stdout", &verification.stdout),
        ("stderr", &verification.stderr),
    ] {
        for line in text.lines() {
            println!(
                "task_submitter_verify_output assignment_id={} stream={stream} line={line}",
                accept.assignment_id
            );
        }
    }
    println!(
        "task_submitter_verify assignment_id={} success={} exit_code={}",
        accept.assignment_id,
        verification.passed(),
        verification
            .exit_code
            .map_or_else(|| "signal".to_owned(), |code| code.to_string())
    );

    // Acceptance is verification alone. The reward, if one was agreed, is settled
    // only for accepted work, and its outcome never changes `accepted`.
    let accepted = verification.passed();
    let mut settlement = None;
    let mut receipt = None;
    if accepted {
        let completed = completion_receipt(&args, &accept, &request, &result)?;
        if let Some(reward) = accept.reward.as_ref() {
            let command = args
                .rewards
                .iter()
                .find(|configured| configured.scheme == reward.scheme)
                .map(|configured| configured.command.as_str())
                .expect("the agreed scheme is one this submitter offered");
            let settle_request = SettleRequest::new(
                reward,
                &completed,
                Duration::from_secs(args.settle_wait_seconds),
            )?;
            let outcome = settle(command, &settle_request);
            println!(
                "task_submitter_reward assignment_id={} scheme={} state={:?} reference={} note={}",
                accept.assignment_id,
                outcome.scheme,
                outcome.state,
                outcome.reference.clone().unwrap_or_default(),
                outcome.note.clone().unwrap_or_default()
            );
            settlement = Some(outcome);
        }
        receipt = Some(completed);
    }
    let ack_note = match (accepted, settlement.as_ref().map(|s| s.state)) {
        (false, _) => verification.rejection_note(),
        (true, None) => "artifact verified".to_string(),
        (true, Some(RewardState::Settled)) => "artifact verified; reward settled".to_string(),
        (true, Some(RewardState::Pending)) => "artifact verified; reward pending".to_string(),
        (true, Some(RewardState::Failed)) => "artifact verified; reward failed".to_string(),
    };
    write_closure_record(
        closure_record(receipt, settlement.clone(), verification)?,
        args.receipt_path.as_deref(),
        accept.assignment_id,
    )?;
    let ack = TaskAck {
        assignment_id: accept.assignment_id,
        request_id: accept.request_id,
        offer_id: accept.offer_id,
        submitter_node_id: args.submitter_node_id.clone(),
        acked_at: now_timestamp(),
        accepted,
        reward: settlement,
        note: Some(ack_note),
    };
    let ack_target = selected
        .offer
        .reply_endpoints
        .first()
        .cloned()
        .unwrap_or_else(|| selected.source.to_string());
    network.send_p2p_message(&ack_target, Message::TaskAck(ack.clone()), true)?;
    println!(
        "task_submitter_ack_sent assignment_id={} accepted={} target={} reward={}",
        ack.assignment_id,
        ack.accepted,
        ack_target,
        ack.reward
            .as_ref()
            .map(|reward| format!("{}:{:?}", reward.scheme, reward.state))
            .unwrap_or_else(|| "none".to_string())
    );

    if ack.accepted {
        Ok(())
    } else {
        bail!("the task result did not pass verification")
    }
}

fn collect_offers(
    inbox: &mut Inbox<'_>,
    request: &TaskRequest,
    args: &Args,
) -> Result<Vec<SeenOffer>> {
    let mut seen_offers = HashMap::<u64, SeenOffer>::new();
    let deadline = Instant::now() + Duration::from_secs(args.offer_timeout_seconds);
    while let Some((source, _header, message)) = inbox.next_until(deadline)? {
        if let Message::TaskOffer(offer) = message {
            if offer.request_id == request.request_id {
                println!(
                    "task_submitter_offer request_id={} offer_id={} worker_node_id={} source={} estimated_duration_secs={} max_status_interval_secs={} reward_schemes={} note={}",
                    request.request_id,
                    offer.offer_id,
                    offer.worker_node_id,
                    source,
                    offer.estimated_duration_secs.unwrap_or_default(),
                    offer.max_status_interval_secs.unwrap_or_default(),
                    offer
                        .reward_payees
                        .iter()
                        .map(|payee| payee.scheme.as_str())
                        .collect::<Vec<_>>()
                        .join(","),
                    offer.note.clone().unwrap_or_default()
                );
                seen_offers
                    .entry(offer.offer_id)
                    .or_insert(SeenOffer { source, offer });
            }
        }
    }

    if seen_offers.is_empty() {
        bail!(
            "no task offers received for request_id={}",
            request.request_id
        );
    }
    Ok(seen_offers.into_values().collect())
}

fn select_offer<'a>(
    offers: &'a [SeenOffer],
    worker_node_id: Option<&str>,
) -> Result<&'a SeenOffer> {
    if let Some(worker_node_id) = worker_node_id {
        offers
            .iter()
            .find(|seen| seen.offer.worker_node_id == worker_node_id)
            .ok_or_else(|| anyhow!("no offer found for selected worker node id: {worker_node_id}"))
    } else {
        offers
            .first()
            .ok_or_else(|| anyhow!("no task offers available to select"))
    }
}

fn wait_for_result(
    inbox: &mut Inbox<'_>,
    accept: &TaskAccept,
    selected: &SeenOffer,
    args: &Args,
) -> Result<TaskResult> {
    let deadline = Instant::now() + Duration::from_secs(args.result_timeout_seconds);
    while let Some((source, _header, message)) = inbox.next_until(deadline)? {
        match message {
            Message::TaskStatus(status)
                if status.assignment_id == accept.assignment_id
                    && status.request_id == accept.request_id
                    && status.offer_id == accept.offer_id =>
            {
                println!(
                    "task_submitter_status assignment_id={} worker_node_id={} source={} state={} next_check_in_by={} note={}",
                    status.assignment_id,
                    status.worker_node_id,
                    source,
                    status.state,
                    status.next_check_in_by.unwrap_or_default(),
                    status.note.unwrap_or_default()
                );
            }
            Message::TaskResult(result)
                if result.assignment_id == accept.assignment_id
                    && result.request_id == accept.request_id
                    && result.offer_id == accept.offer_id =>
            {
                println!(
                    "task_submitter_result assignment_id={} worker_node_id={} source={} artifact_hint={} note={}",
                    result.assignment_id,
                    result.worker_node_id,
                    source,
                    result.artifact_hint.clone().unwrap_or_default(),
                    result.note.clone().unwrap_or_default()
                );
                return Ok(result);
            }
            Message::TaskOffer(offer) if offer.request_id == accept.request_id => {
                println!(
                    "task_submitter_ignored_offer request_id={} offer_id={} worker_node_id={} source={}",
                    offer.request_id, offer.offer_id, offer.worker_node_id, source
                );
            }
            _ => {}
        }
    }

    bail!(
        "timed out waiting for TaskResult from worker_node_id={}",
        selected.offer.worker_node_id
    )
}

fn run_verify_command(
    args: &Args,
    accept: &TaskAccept,
    selected: &SeenOffer,
    result: &TaskResult,
) -> Result<std::process::Output> {
    let mut command = shell_command(&args.verify_command);
    command.env("LOADNGO_TASK_REQUEST_ID", accept.request_id.to_string());
    command.env("LOADNGO_TASK_OFFER_ID", accept.offer_id.to_string());
    command.env(
        "LOADNGO_TASK_ASSIGNMENT_ID",
        accept.assignment_id.to_string(),
    );
    command.env("LOADNGO_TASK_SUBMITTER_NODE_ID", &accept.submitter_node_id);
    command.env("LOADNGO_TASK_WORKER_NODE_ID", &accept.worker_node_id);
    command.env(
        "LOADNGO_TASK_WORKER_SOURCE_ADDR",
        selected.source.to_string(),
    );
    command.env(
        "LOADNGO_TASK_WORKER_REPLY_ENDPOINTS",
        selected.offer.reply_endpoints.join(","),
    );
    command.env(
        "LOADNGO_TASK_WORKER_HOST",
        endpoint_host(&selected.offer.reply_endpoints[0])?,
    );
    command.env("LOADNGO_TASK_SUMMARY", &args.summary);
    if let Some(criteria) = args.success_criteria.as_deref() {
        command.env("LOADNGO_TASK_SUCCESS_CRITERIA", criteria);
    }
    if let Some(artifact_hint) = result
        .artifact_hint
        .as_deref()
        .or(args.artifact_hint.as_deref())
    {
        command.env("LOADNGO_TASK_ARTIFACT_HINT", artifact_hint);
    }
    if let Some(path) = args.artifact_copy_path.as_deref() {
        command.env("LOADNGO_TASK_ARTIFACT_COPY_PATH", path);
    }
    command
        .output()
        .with_context(|| format!("failed to run verify command: {}", args.verify_command))
}

fn completion_receipt(
    args: &Args,
    accept: &TaskAccept,
    request: &TaskRequest,
    result: &TaskResult,
) -> Result<RewardReceipt> {
    let artifact_hash = match args.artifact_copy_path.as_deref() {
        Some(path) => artifact_hash_hex(path)?,
        None => None,
    };
    Ok(RewardReceipt {
        receipt_version: u32::from(REWARD_RECEIPT_VERSION),
        request_id: accept.request_id,
        offer_id: accept.offer_id,
        assignment_id: accept.assignment_id,
        submitter_node_id: accept.submitter_node_id.clone(),
        worker_node_id: accept.worker_node_id.clone(),
        summary: request.summary.clone(),
        success_criteria: accept
            .success_criteria
            .clone()
            .or_else(|| request.success_criteria.clone()),
        artifact_hint: result
            .artifact_hint
            .clone()
            .or_else(|| request.artifact_hint.clone()),
        artifact_copy_path: args
            .artifact_copy_path
            .as_ref()
            .map(|path| path.display().to_string()),
        artifact_hash_hex: artifact_hash,
        result_note: result.note.clone(),
        accepted_at: now_timestamp(),
        submitted_at: result.submitted_at,
        reward: accept.reward.clone(),
    })
}

fn default_receipt_path(configured: Option<&Path>, assignment_id: u64) -> PathBuf {
    configured
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(format!("task-receipts/assignment-{assignment_id}.json")))
}

fn closure_record(
    receipt: Option<RewardReceipt>,
    reward: Option<RewardSettlement>,
    verification: Verification,
) -> Result<ClosureRecord> {
    let receipt_commitment_hex = receipt
        .as_ref()
        .map(|receipt| reward_receipt_commitment(receipt).map(hex::encode))
        .transpose()?;
    Ok(ClosureRecord {
        accepted: verification.passed(),
        receipt,
        receipt_commitment_hex,
        reward,
        verification,
    })
}

fn write_closure_record(
    closure: ClosureRecord,
    configured_path: Option<&Path>,
    assignment_id: u64,
) -> Result<()> {
    let path = default_receipt_path(configured_path, assignment_id);
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create receipt dir: {}", parent.display()))?;
    }
    let bytes =
        serde_json::to_vec_pretty(&closure).context("failed to serialize closure record")?;
    fs::write(&path, bytes)
        .with_context(|| format!("failed to write closure record: {}", path.display()))
}

fn print_usage() {
    eprintln!(
        "usage: cargo run -p network --bin task_submitter -- \
         --multicast-port <port> \
         --submitter-node-id <node> \
         --reply-endpoint <addr:port> \
         [--request-target <addr:port>] \
         --summary <text> \
         --verify-command <shell command> \
         --multicast-v6 <group%iface> [--multicast-v4 <group@interface>] \
         [--bind-port <port>] [--capability <tag>] [--requested-duration-seconds <n>] \
         [--success-criteria <text>] [--artifact-hint <path>] [--note <text>] \
         [--offer-timeout-seconds <n>] [--status-check-interval-seconds <n>] \
         [--expected-duration-seconds <n>] [--result-timeout-seconds <n>] \
         [--select-worker-node-id <node>] [--artifact-copy-path <path>] \
         [--receipt-path <path>] \
         [--reward <scheme>=<settler command>]... [--reward-settle-seconds <n>, default 30]\n\
         \n\
         The closure record (--receipt-path, default task-receipts/assignment-<id>.json) is\n\
         written for every result checked, accepted or not: what the check printed (the\n\
         last 64 KiB of each stream), its exit code and, for accepted work, the receipt.\n\
         A rejection's TaskAck note carries the check's last line.\n\
         \n\
         A reward is optional: with no --reward, work is offered unrewarded. Acceptance is\n\
         verification alone; the agreed reward is settled only for accepted work, and a\n\
         slow or failing settler gives a pending or failed reward, never rejected work.\n\
         Example: --reward 'qcoin=qcoin-node task-reward settle --target 10.10.10.6:9700'"
    );
}

#[cfg(test)]
mod tests {
    use super::{closure_record, Verification};

    #[test]
    fn a_rejected_result_gets_a_closure_record_with_what_the_check_said() {
        let check = Verification::from_parts("check", Some(3), b"", b"verify: FAIL: why\n");
        let record = serde_json::to_value(closure_record(None, None, check).unwrap()).unwrap();
        assert_eq!(record["accepted"], false);
        assert!(record["receipt"].is_null() && record["receipt_commitment_hex"].is_null());
        assert_eq!(record["verification"]["exit_code"], 3);
        assert_eq!(record["verification"]["stderr"], "verify: FAIL: why\n");
    }
}
