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
    task_reward::{
        choose_reward, parse_scheme_value, settle, shell_command, SchemeCommand, SettleRequest,
        DEFAULT_SETTLE_WAIT,
    },
    task_runtime::{
        artifact_hash_hex, endpoint_host, parse_multicast_v4, parse_multicast_v6,
        reward_receipt_commitment, task_network_config, RewardReceipt, REWARD_RECEIPT_VERSION,
    },
    Network,
};
use serde::Serialize;
use std::{
    collections::HashMap,
    env, fs,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    path::{Path, PathBuf},
    thread,
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

/// Written for accepted work: the completion receipt, its commitment, and the reward
/// settlement if one was agreed.
#[derive(Debug, Serialize)]
struct ClosureRecord {
    receipt: RewardReceipt,
    receipt_commitment_hex: String,
    reward: Option<RewardSettlement>,
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

    let offers = collect_offers(&network, &request, &args)?;
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

    let result = wait_for_result(&network, &accept, selected, &args)?;
    let verify_output = run_verify_command(&args, &accept, selected, &result)?;
    let verification_ok = verify_output.status.success();
    println!(
        "task_submitter_verify assignment_id={} success={}",
        accept.assignment_id, verification_ok
    );

    // Acceptance is verification alone. The reward, if one was agreed, is settled
    // only for accepted work, and its outcome never changes `accepted`.
    let accepted = verification_ok;
    let mut settlement = None;
    if accepted {
        let receipt = completion_receipt(&args, &accept, &request, &result)?;
        if let Some(reward) = accept.reward.as_ref() {
            let command = args
                .rewards
                .iter()
                .find(|configured| configured.scheme == reward.scheme)
                .map(|configured| configured.command.as_str())
                .expect("the agreed scheme is one this submitter offered");
            let settle_request = SettleRequest::new(
                reward,
                &receipt,
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
        write_closure_record(&receipt, settlement.clone(), args.receipt_path.as_deref())?;
    }

    let ack_note = match (accepted, settlement.as_ref().map(|s| s.state)) {
        (false, _) => format!("verification failed: {}", verify_output.status),
        (true, None) => "artifact verified".to_string(),
        (true, Some(RewardState::Settled)) => "artifact verified; reward settled".to_string(),
        (true, Some(RewardState::Pending)) => "artifact verified; reward pending".to_string(),
        (true, Some(RewardState::Failed)) => "artifact verified; reward failed".to_string(),
    };
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

fn collect_offers(network: &Network, request: &TaskRequest, args: &Args) -> Result<Vec<SeenOffer>> {
    let mut seen_offers = HashMap::<u64, SeenOffer>::new();
    let deadline = Instant::now() + Duration::from_secs(args.offer_timeout_seconds);
    while Instant::now() < deadline {
        let mut captured = None;
        if network.recv_and_dispatch_p2p(&mut |source, _header, message| {
            captured = Some((source, message));
        })? {
            if let Some((source, Message::TaskOffer(offer))) = captured {
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
        } else {
            thread::sleep(Duration::from_millis(25));
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
    network: &Network,
    accept: &TaskAccept,
    selected: &SeenOffer,
    args: &Args,
) -> Result<TaskResult> {
    let deadline = Instant::now() + Duration::from_secs(args.result_timeout_seconds);
    while Instant::now() < deadline {
        let mut captured = None;
        if network.recv_and_dispatch_p2p(&mut |source, _header, message| {
            captured = Some((source, message));
        })? {
            match captured {
                Some((source, Message::TaskStatus(status)))
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
                Some((source, Message::TaskResult(result)))
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
                Some((source, Message::TaskOffer(offer)))
                    if offer.request_id == accept.request_id =>
                {
                    println!(
                        "task_submitter_ignored_offer request_id={} offer_id={} worker_node_id={} source={}",
                        offer.request_id, offer.offer_id, offer.worker_node_id, source
                    );
                }
                _ => {}
            }
        } else {
            thread::sleep(Duration::from_millis(25));
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

fn write_closure_record(
    receipt: &RewardReceipt,
    reward: Option<RewardSettlement>,
    configured_path: Option<&Path>,
) -> Result<()> {
    let path = default_receipt_path(configured_path, receipt.assignment_id);
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create receipt dir: {}", parent.display()))?;
    }
    let closure = ClosureRecord {
        receipt: receipt.clone(),
        receipt_commitment_hex: hex::encode(reward_receipt_commitment(receipt)?),
        reward,
    };
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
         A reward is optional: with no --reward, work is offered unrewarded. Acceptance is\n\
         verification alone; the agreed reward is settled only for accepted work, and a\n\
         slow or failing settler gives a pending or failed reward, never rejected work.\n\
         Example: --reward 'qcoin=qcoin-node task-reward settle --target 10.10.10.6:9700'"
    );
}
