use anyhow::{anyhow, bail, Context, Result};
use data::{
    generate_id,
    model_utils::now_timestamp,
    p2pmsg::{Message, TaskAccept, TaskOffer, TaskRequest, TaskResult, TaskStatus},
};
use network::{
    task_runtime::{parse_multicast_v4, parse_multicast_v6, task_network_config},
    Network,
};
use std::{
    collections::{HashMap, HashSet},
    env,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    process::Command,
    thread,
    time::{Duration, Instant},
};

struct Args {
    bind_port: u16,
    worker_node_id: String,
    reply_endpoints: Vec<String>,
    capability_tags: Vec<String>,
    estimated_duration_secs: Option<u64>,
    max_status_interval_secs: Option<u64>,
    artifact_hint: Option<String>,
    note: Option<String>,
    execute_command: String,
    result_note: Option<String>,
    listen_seconds: u64,
    ack_timeout_seconds: u64,
    multicast_v6: Vec<(Ipv6Addr, u32)>,
    multicast_v4: Vec<(Ipv4Addr, Ipv4Addr)>,
}

#[derive(Clone)]
struct OfferedRequest {
    request: TaskRequest,
    offer: TaskOffer,
}

impl Args {
    fn parse() -> Result<Self> {
        let mut bind_port = 9850u16;
        let mut worker_node_id = None;
        let mut reply_endpoints = Vec::new();
        let mut capability_tags = Vec::new();
        let mut estimated_duration_secs = None;
        let mut max_status_interval_secs = None;
        let mut artifact_hint = None;
        let mut note = None;
        let mut execute_command = None;
        let mut result_note = None;
        let mut listen_seconds = 300u64;
        let mut ack_timeout_seconds = 90u64;
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
                "--worker-node-id" => {
                    worker_node_id =
                        Some(args.next().context("missing value for --worker-node-id")?);
                }
                "--reply-endpoint" => {
                    reply_endpoints
                        .push(args.next().context("missing value for --reply-endpoint")?);
                }
                "--capability" => {
                    capability_tags.push(args.next().context("missing value for --capability")?);
                }
                "--estimated-duration-seconds" => {
                    estimated_duration_secs = Some(
                        args.next()
                            .context("missing value for --estimated-duration-seconds")?
                            .parse()
                            .context("invalid --estimated-duration-seconds")?,
                    );
                }
                "--max-status-interval-seconds" => {
                    max_status_interval_secs = Some(
                        args.next()
                            .context("missing value for --max-status-interval-seconds")?
                            .parse()
                            .context("invalid --max-status-interval-seconds")?,
                    );
                }
                "--artifact-hint" => {
                    artifact_hint = Some(args.next().context("missing value for --artifact-hint")?);
                }
                "--note" => {
                    note = Some(args.next().context("missing value for --note")?);
                }
                "--execute-command" => {
                    execute_command =
                        Some(args.next().context("missing value for --execute-command")?);
                }
                "--result-note" => {
                    result_note = Some(args.next().context("missing value for --result-note")?);
                }
                "--listen-seconds" => {
                    listen_seconds = args
                        .next()
                        .context("missing value for --listen-seconds")?
                        .parse()
                        .context("invalid --listen-seconds")?;
                }
                "--ack-timeout-seconds" => {
                    ack_timeout_seconds = args
                        .next()
                        .context("missing value for --ack-timeout-seconds")?
                        .parse()
                        .context("invalid --ack-timeout-seconds")?;
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
            worker_node_id: worker_node_id
                .ok_or_else(|| anyhow!("--worker-node-id is required"))?,
            reply_endpoints,
            capability_tags,
            estimated_duration_secs,
            max_status_interval_secs,
            artifact_hint,
            note,
            execute_command: execute_command
                .ok_or_else(|| anyhow!("--execute-command is required"))?,
            result_note,
            listen_seconds,
            ack_timeout_seconds,
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

    let config = task_network_config(args.bind_port, &args.multicast_v6, &args.multicast_v4);
    let mut network = Network::with_config(config);
    network.init()?;

    let mut responded_requests = HashSet::new();
    let mut offers = HashMap::<u64, OfferedRequest>::new();
    let deadline = Instant::now() + Duration::from_secs(args.listen_seconds);
    while Instant::now() < deadline {
        let mut captured = None;
        if network.recv_and_dispatch_p2p(&mut |source, _header, message| {
            captured = Some((source, message));
        })? {
            match captured {
                Some((source, Message::TaskRequest(request))) => {
                    if let Some(offered) =
                        handle_request(&network, &args, &mut responded_requests, source, request)?
                    {
                        offers.insert(offered.request.request_id, offered);
                    }
                }
                Some((source, Message::TaskAccept(accept))) => {
                    if let Some(offered) = offers.get(&accept.request_id).cloned() {
                        if offered.offer.offer_id == accept.offer_id
                            && accept.worker_node_id == args.worker_node_id
                        {
                            execute_assignment(&network, &args, source, &offered, accept)?;
                            return Ok(());
                        }
                    }
                }
                _ => {}
            }
        } else {
            thread::sleep(Duration::from_millis(25));
        }
    }

    println!(
        "task_worker_timeout worker_node_id={} listen_seconds={} result=no-assignment",
        args.worker_node_id, args.listen_seconds
    );
    Ok(())
}

fn handle_request(
    network: &Network,
    args: &Args,
    responded_requests: &mut HashSet<u64>,
    source: SocketAddr,
    request: TaskRequest,
) -> Result<Option<OfferedRequest>> {
    if now_timestamp() > request.expires_at {
        return Ok(None);
    }
    if !responded_requests.insert(request.request_id) {
        return Ok(None);
    }

    let capabilities_match = request
        .capability_tags
        .iter()
        .all(|tag| args.capability_tags.contains(tag));
    println!(
        "task_worker_request request_id={} submitter_node_id={} source={} capabilities_match={} summary={}",
        request.request_id,
        request.submitter_node_id,
        source,
        capabilities_match,
        request.summary
    );

    if !capabilities_match || request.reply_endpoints.is_empty() {
        return Ok(None);
    }

    let created_at = now_timestamp();
    let offer = TaskOffer {
        offer_id: generate_id(),
        request_id: request.request_id,
        worker_node_id: args.worker_node_id.clone(),
        created_at,
        expires_at: created_at.saturating_add(args.listen_seconds),
        capability_tags: args.capability_tags.clone(),
        reply_endpoints: args.reply_endpoints.clone(),
        estimated_duration_secs: args.estimated_duration_secs,
        max_status_interval_secs: args.max_status_interval_secs,
        note: args.note.clone(),
        artifact_hint: args
            .artifact_hint
            .clone()
            .or_else(|| request.artifact_hint.clone()),
    };

    let mut sent_targets = 0usize;
    for target in &request.reply_endpoints {
        if network
            .send_p2p_message(target, Message::TaskOffer(offer.clone()), true)
            .is_ok()
        {
            sent_targets += 1;
        }
    }

    if sent_targets == 0 {
        return Ok(None);
    }

    println!(
        "task_worker_offer_sent request_id={} offer_id={} worker_node_id={} targets={} expires_at={}",
        offer.request_id, offer.offer_id, offer.worker_node_id, sent_targets, offer.expires_at
    );

    Ok(Some(OfferedRequest { request, offer }))
}

fn execute_assignment(
    network: &Network,
    args: &Args,
    accept_source: SocketAddr,
    offered: &OfferedRequest,
    accept: TaskAccept,
) -> Result<()> {
    let result_target = accept
        .submitter_reply_endpoint
        .clone()
        .unwrap_or_else(|| accept_source.to_string());
    let artifact_hint = accept
        .artifact_hint
        .clone()
        .or_else(|| args.artifact_hint.clone())
        .or_else(|| offered.request.artifact_hint.clone());

    let status = TaskStatus {
        assignment_id: accept.assignment_id,
        request_id: accept.request_id,
        offer_id: accept.offer_id,
        worker_node_id: args.worker_node_id.clone(),
        status_at: now_timestamp(),
        state: "running".to_string(),
        next_check_in_by: Some(
            now_timestamp().saturating_add(accept.status_check_interval_secs.max(1)),
        ),
        note: Some("worker accepted assignment and started execution".to_string()),
        artifact_hint: artifact_hint.clone(),
    };
    network.send_p2p_message(&result_target, Message::TaskStatus(status.clone()), true)?;
    println!(
        "task_worker_status assignment_id={} state={} target={}",
        status.assignment_id, status.state, result_target
    );

    let output = run_execute_command(args, &accept, offered, artifact_hint.as_deref())?;
    let result_note = Some(if output.status.success() {
        args.result_note
            .clone()
            .unwrap_or_else(|| "worker command completed successfully".to_string())
    } else {
        format!("worker command failed: {}", output.status)
    });

    let result = TaskResult {
        assignment_id: accept.assignment_id,
        request_id: accept.request_id,
        offer_id: accept.offer_id,
        worker_node_id: args.worker_node_id.clone(),
        submitted_at: now_timestamp(),
        artifact_hint,
        note: result_note,
    };
    network.send_p2p_message(&result_target, Message::TaskResult(result.clone()), true)?;
    println!(
        "task_worker_result assignment_id={} target={} status_success={}",
        result.assignment_id,
        result_target,
        output.status.success()
    );

    let ack_deadline = Instant::now() + Duration::from_secs(args.ack_timeout_seconds);
    while Instant::now() < ack_deadline {
        let mut captured = None;
        if network.recv_and_dispatch_p2p(&mut |source, _header, message| {
            captured = Some((source, message));
        })? {
            if let Some((source, Message::TaskAck(ack))) = captured {
                if ack.assignment_id == accept.assignment_id
                    && ack.request_id == accept.request_id
                    && ack.offer_id == accept.offer_id
                {
                    println!(
                        "task_worker_ack assignment_id={} source={} accepted={} qcoin_tx_hint={} note={}",
                        ack.assignment_id,
                        source,
                        ack.accepted,
                        ack.qcoin_tx_hint.unwrap_or_default(),
                        ack.note.unwrap_or_default()
                    );
                    return Ok(());
                }
            }
        } else {
            thread::sleep(Duration::from_millis(25));
        }
    }

    bail!(
        "timed out waiting for TaskAck for assignment_id={}",
        accept.assignment_id
    )
}

fn run_execute_command(
    args: &Args,
    accept: &TaskAccept,
    offered: &OfferedRequest,
    artifact_hint: Option<&str>,
) -> Result<std::process::Output> {
    let mut command = shell_command(&args.execute_command);
    command.env("LOADNGO_TASK_REQUEST_ID", accept.request_id.to_string());
    command.env("LOADNGO_TASK_OFFER_ID", accept.offer_id.to_string());
    command.env(
        "LOADNGO_TASK_ASSIGNMENT_ID",
        accept.assignment_id.to_string(),
    );
    command.env("LOADNGO_TASK_SUBMITTER_NODE_ID", &accept.submitter_node_id);
    command.env("LOADNGO_TASK_WORKER_NODE_ID", &accept.worker_node_id);
    command.env("LOADNGO_TASK_SUMMARY", &offered.request.summary);
    if let Some(criteria) = accept
        .success_criteria
        .as_deref()
        .or(offered.request.success_criteria.as_deref())
    {
        command.env("LOADNGO_TASK_SUCCESS_CRITERIA", criteria);
    }
    if let Some(artifact_hint) = artifact_hint {
        command.env("LOADNGO_TASK_ARTIFACT_HINT", artifact_hint);
    }
    if let Some(endpoint) = accept.submitter_reply_endpoint.as_deref() {
        command.env("LOADNGO_TASK_SUBMITTER_REPLY_ENDPOINT", endpoint);
    }
    command
        .output()
        .with_context(|| format!("failed to run worker command: {}", args.execute_command))
}

fn shell_command(script: &str) -> Command {
    #[cfg(unix)]
    {
        let mut command = Command::new("sh");
        command.arg("-lc").arg(script);
        command
    }
    #[cfg(windows)]
    {
        let mut command = Command::new("cmd");
        command.arg("/C").arg(script);
        command
    }
}

fn print_usage() {
    eprintln!(
        "usage: cargo run -p network --bin task_worker -- \
         --worker-node-id <node> \
         --reply-endpoint <addr:port> \
         --execute-command <shell command> \
         --multicast-v6 <group%iface> [--multicast-v4 <group@interface>] \
         [--bind-port <port>] [--capability <tag>] [--artifact-hint <path>] \
         [--estimated-duration-seconds <n>] [--max-status-interval-seconds <n>] \
         [--note <text>] [--result-note <text>] [--listen-seconds <n>] [--ack-timeout-seconds <n>]"
    );
}
