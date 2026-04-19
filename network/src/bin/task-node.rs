use anyhow::{anyhow, bail, Context, Result};
use data::{
    generate_id,
    model_utils::now_timestamp,
    p2pmsg::{Message, TaskAccept, TaskAck, TaskOffer, TaskRequest, TaskResult, TaskStatus},
};
use loadngo_proactor::{ChannelPort, CompletionKind, Proactor, ProactorHandle};
use network::{
    task_runtime::{parse_multicast_v4, parse_multicast_v6, task_network_config},
    Network,
};
use std::{
    collections::{HashMap, HashSet},
    env,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    process::Command,
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

#[derive(Clone)]
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
    ack_timeout_seconds: u64,
    idle_interval_millis: u64,
    run_seconds: Option<u64>,
    multicast_v6: Vec<(Ipv6Addr, u32)>,
    multicast_v4: Vec<(Ipv4Addr, Ipv4Addr)>,
}

#[derive(Clone)]
struct OfferedRequest {
    request: TaskRequest,
    offer: TaskOffer,
}

#[derive(Clone)]
struct ActiveAssignment {
    assignment_id: u64,
    request_id: u64,
    offer_id: u64,
}

#[derive(Default)]
struct TaskNodeState {
    responded_requests: HashSet<u64>,
    offers: HashMap<u64, OfferedRequest>,
    active_assignment: Option<ActiveAssignment>,
}

struct TaskNode {
    network: Arc<Network>,
    handle: ProactorHandle<ChannelPort>,
    args: Arc<Args>,
    idle_interval: Duration,
    state: Mutex<TaskNodeState>,
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
        let mut ack_timeout_seconds = 90u64;
        let mut idle_interval_millis = 250u64;
        let mut run_seconds = None;
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
                "--ack-timeout-seconds" => {
                    ack_timeout_seconds = args
                        .next()
                        .context("missing value for --ack-timeout-seconds")?
                        .parse()
                        .context("invalid --ack-timeout-seconds")?;
                }
                "--idle-interval-millis" => {
                    idle_interval_millis = args
                        .next()
                        .context("missing value for --idle-interval-millis")?
                        .parse()
                        .context("invalid --idle-interval-millis")?;
                }
                "--run-seconds" => {
                    run_seconds = Some(
                        args.next()
                            .context("missing value for --run-seconds")?
                            .parse()
                            .context("invalid --run-seconds")?,
                    );
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
            ack_timeout_seconds,
            idle_interval_millis,
            run_seconds,
            multicast_v6,
            multicast_v4,
        })
    }
}

impl TaskNode {
    fn start(
        network: Arc<Network>,
        handle: ProactorHandle<ChannelPort>,
        args: Arc<Args>,
    ) -> Result<Arc<Self>> {
        let idle_interval = Duration::from_millis(args.idle_interval_millis.max(1));
        let node = Arc::new(Self {
            network,
            handle,
            args,
            idle_interval,
            state: Mutex::new(TaskNodeState::default()),
        });
        node.schedule(Duration::ZERO)?;
        Ok(node)
    }

    fn schedule(self: &Arc<Self>, delay: Duration) -> Result<()> {
        let node = Arc::clone(self);
        self.handle
            .defer_for(delay, CompletionKind::Net, 0, move |_| {
                node.run();
            })?;
        Ok(())
    }

    fn run(self: Arc<Self>) {
        let drained = match self.drain_once() {
            Ok(drained) => drained,
            Err(err) => {
                eprintln!(
                    "task-node_error worker_node_id={} error={err:#}",
                    self.args.worker_node_id
                );
                let _ = self.handle.stop();
                return;
            }
        };

        if !self.handle.is_running() {
            return;
        }

        let delay = if drained == 0 {
            self.idle_interval
        } else {
            Duration::ZERO
        };
        if let Err(err) = self.schedule(delay) {
            eprintln!(
                "task-node_schedule_error worker_node_id={} error={err:#}",
                self.args.worker_node_id
            );
            let _ = self.handle.stop();
        }
    }

    fn drain_once(self: &Arc<Self>) -> Result<usize> {
        let mut first_err = None;
        let drained = self
            .network
            .drain_and_dispatch_p2p(&mut |source, _header, message| {
                if first_err.is_some() {
                    return;
                }
                if let Err(err) = self.handle_message(source, message) {
                    first_err = Some(err);
                }
            })?;
        if let Some(err) = first_err {
            return Err(err);
        }
        Ok(drained)
    }

    fn handle_message(self: &Arc<Self>, source: SocketAddr, message: Message) -> Result<()> {
        match message {
            Message::TaskRequest(request) => self.handle_request(source, request),
            Message::TaskAccept(accept) => self.handle_accept(source, accept),
            Message::TaskAck(ack) => self.handle_ack(source, ack),
            _ => Ok(()),
        }
    }

    fn handle_request(&self, source: SocketAddr, request: TaskRequest) -> Result<()> {
        if now_timestamp() > request.expires_at {
            return Ok(());
        }

        let capabilities_match = request
            .capability_tags
            .iter()
            .all(|tag| self.args.capability_tags.contains(tag));
        println!(
            "task-node_request request_id={} submitter_node_id={} source={} capabilities_match={} summary={}",
            request.request_id,
            request.submitter_node_id,
            source,
            capabilities_match,
            request.summary
        );

        if !capabilities_match || request.reply_endpoints.is_empty() {
            return Ok(());
        }

        let offer = {
            let mut state = self.state.lock().expect("task node state lock poisoned");
            if state.active_assignment.is_some()
                || !state.responded_requests.insert(request.request_id)
            {
                return Ok(());
            }

            let created_at = now_timestamp();
            TaskOffer {
                offer_id: generate_id(),
                request_id: request.request_id,
                worker_node_id: self.args.worker_node_id.clone(),
                created_at,
                expires_at: created_at.saturating_add(60),
                capability_tags: self.args.capability_tags.clone(),
                reply_endpoints: self.args.reply_endpoints.clone(),
                estimated_duration_secs: self.args.estimated_duration_secs,
                max_status_interval_secs: self.args.max_status_interval_secs,
                note: self.args.note.clone(),
                artifact_hint: self
                    .args
                    .artifact_hint
                    .clone()
                    .or_else(|| request.artifact_hint.clone()),
            }
        };

        let mut sent_targets = 0usize;
        for target in &request.reply_endpoints {
            if self
                .network
                .send_p2p_message(target, Message::TaskOffer(offer.clone()), true)
                .is_ok()
            {
                sent_targets += 1;
            }
        }

        if sent_targets == 0 {
            return Ok(());
        }

        {
            let mut state = self.state.lock().expect("task node state lock poisoned");
            state.offers.insert(
                request.request_id,
                OfferedRequest {
                    request,
                    offer: offer.clone(),
                },
            );
        }

        println!(
            "task-node_offer_sent request_id={} offer_id={} worker_node_id={} targets={} expires_at={}",
            offer.request_id, offer.offer_id, offer.worker_node_id, sent_targets, offer.expires_at
        );
        Ok(())
    }

    fn handle_accept(self: &Arc<Self>, source: SocketAddr, accept: TaskAccept) -> Result<()> {
        let offered = {
            let mut state = self.state.lock().expect("task node state lock poisoned");
            if state.active_assignment.is_some() {
                return Ok(());
            }
            let Some(offered) = state.offers.remove(&accept.request_id) else {
                return Ok(());
            };
            if offered.offer.offer_id != accept.offer_id
                || accept.worker_node_id != self.args.worker_node_id
            {
                return Ok(());
            }
            state.active_assignment = Some(ActiveAssignment {
                assignment_id: accept.assignment_id,
                request_id: accept.request_id,
                offer_id: accept.offer_id,
            });
            offered
        };

        let result_target = accept
            .submitter_reply_endpoint
            .clone()
            .unwrap_or_else(|| source.to_string());
        let artifact_hint = accept
            .artifact_hint
            .clone()
            .or_else(|| self.args.artifact_hint.clone())
            .or_else(|| offered.request.artifact_hint.clone());

        let status = TaskStatus {
            assignment_id: accept.assignment_id,
            request_id: accept.request_id,
            offer_id: accept.offer_id,
            worker_node_id: self.args.worker_node_id.clone(),
            status_at: now_timestamp(),
            state: "running".to_string(),
            next_check_in_by: Some(
                now_timestamp().saturating_add(accept.status_check_interval_secs.max(1)),
            ),
            note: Some("task node accepted assignment and started execution".to_string()),
            artifact_hint: artifact_hint.clone(),
        };
        if let Err(err) =
            self.network
                .send_p2p_message(&result_target, Message::TaskStatus(status.clone()), true)
        {
            self.clear_assignment(accept.assignment_id, accept.request_id, accept.offer_id);
            return Err(err).context("failed to send initial task status");
        }
        println!(
            "task-node_status assignment_id={} state={} target={}",
            status.assignment_id, status.state, result_target
        );

        self.spawn_execution(offered, accept, result_target, artifact_hint);
        Ok(())
    }

    fn spawn_execution(
        self: &Arc<Self>,
        offered: OfferedRequest,
        accept: TaskAccept,
        result_target: String,
        artifact_hint: Option<String>,
    ) {
        let node = Arc::clone(self);
        let handle = self.handle.clone();
        let args = Arc::clone(&self.args);
        let accept_for_completion = accept.clone();
        let result_target_for_completion = result_target.clone();
        let artifact_hint_for_completion = artifact_hint.clone();
        let accept_assignment_id = accept.assignment_id;
        let accept_request_id = accept.request_id;
        let accept_offer_id = accept.offer_id;
        thread::spawn(move || {
            let output = run_execute_command(&args, &accept, &offered, artifact_hint.as_deref());
            let node_for_completion = Arc::clone(&node);
            if let Err(err) = handle.enqueue_work(move |_| {
                if let Err(report_err) = node_for_completion.finish_execution(
                    &accept_for_completion,
                    &result_target_for_completion,
                    artifact_hint_for_completion.clone(),
                    output,
                ) {
                    eprintln!(
                        "task-node_result_error assignment_id={} error={report_err:#}",
                        accept_for_completion.assignment_id
                    );
                }
            }) {
                eprintln!(
                    "task-node_enqueue_error assignment_id={} error={err:#}",
                    accept_assignment_id
                );
                node.clear_assignment(accept_assignment_id, accept_request_id, accept_offer_id);
            }
        });
    }

    fn finish_execution(
        self: &Arc<Self>,
        accept: &TaskAccept,
        result_target: &str,
        artifact_hint: Option<String>,
        output: Result<std::process::Output>,
    ) -> Result<()> {
        {
            let state = self.state.lock().expect("task node state lock poisoned");
            let Some(active) = state.active_assignment.as_ref() else {
                return Ok(());
            };
            if !matches_assignment(
                active,
                accept.assignment_id,
                accept.request_id,
                accept.offer_id,
            ) {
                return Ok(());
            }
        }

        let (status_success, result_note) = match output {
            Ok(output) => (
                output.status.success(),
                if output.status.success() {
                    self.args
                        .result_note
                        .clone()
                        .unwrap_or_else(|| "task node command completed successfully".to_string())
                } else {
                    format!("task node command failed: {}", output.status)
                },
            ),
            Err(err) => (false, format!("task node command error: {err:#}")),
        };

        let result = TaskResult {
            assignment_id: accept.assignment_id,
            request_id: accept.request_id,
            offer_id: accept.offer_id,
            worker_node_id: self.args.worker_node_id.clone(),
            submitted_at: now_timestamp(),
            artifact_hint,
            note: Some(result_note),
        };
        self.network
            .send_p2p_message(result_target, Message::TaskResult(result.clone()), true)?;
        println!(
            "task-node_result assignment_id={} target={} status_success={}",
            result.assignment_id, result_target, status_success
        );

        let node = Arc::clone(self);
        let assignment_id = accept.assignment_id;
        let request_id = accept.request_id;
        let offer_id = accept.offer_id;
        self.handle.defer_for(
            Duration::from_secs(self.args.ack_timeout_seconds.max(1)),
            CompletionKind::Timer,
            0,
            move |_| {
                node.handle_ack_timeout(assignment_id, request_id, offer_id);
            },
        )?;
        Ok(())
    }

    fn handle_ack(&self, source: SocketAddr, ack: TaskAck) -> Result<()> {
        let mut state = self.state.lock().expect("task node state lock poisoned");
        let Some(active) = state.active_assignment.as_ref() else {
            return Ok(());
        };
        if !matches_assignment(active, ack.assignment_id, ack.request_id, ack.offer_id) {
            return Ok(());
        }

        println!(
            "task-node_ack assignment_id={} source={} accepted={} qcoin_tx_hint={} note={}",
            ack.assignment_id,
            source,
            ack.accepted,
            ack.qcoin_tx_hint.unwrap_or_default(),
            ack.note.unwrap_or_default()
        );
        state.active_assignment = None;
        Ok(())
    }

    fn handle_ack_timeout(&self, assignment_id: u64, request_id: u64, offer_id: u64) {
        if self.clear_assignment(assignment_id, request_id, offer_id) {
            eprintln!(
                "task-node_ack_timeout assignment_id={} worker_node_id={} result=no-ack",
                assignment_id, self.args.worker_node_id
            );
        }
    }

    fn clear_assignment(&self, assignment_id: u64, request_id: u64, offer_id: u64) -> bool {
        let mut state = self.state.lock().expect("task node state lock poisoned");
        let Some(active) = state.active_assignment.as_ref() else {
            return false;
        };
        if !matches_assignment(active, assignment_id, request_id, offer_id) {
            return false;
        }
        state.active_assignment = None;
        true
    }
}

fn matches_assignment(
    active: &ActiveAssignment,
    assignment_id: u64,
    request_id: u64,
    offer_id: u64,
) -> bool {
    active.assignment_id == assignment_id
        && active.request_id == request_id
        && active.offer_id == offer_id
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
        .with_context(|| format!("failed to run task node command: {}", args.execute_command))
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

fn main() -> Result<()> {
    let args = Arc::new(Args::parse()?);
    if args.reply_endpoints.is_empty() {
        bail!("at least one --reply-endpoint is required");
    }
    if args.multicast_v6.is_empty() && args.multicast_v4.is_empty() {
        bail!("at least one multicast target is required");
    }

    let config = task_network_config(args.bind_port, &args.multicast_v6, &args.multicast_v4);
    let mut network = Network::with_config(config);
    network.init()?;
    let network = Arc::new(network);

    let proactor = Proactor::new(ChannelPort::new());
    let handle = proactor.handle();
    let stop_handle = handle.clone();
    let _node = TaskNode::start(Arc::clone(&network), handle.clone(), Arc::clone(&args))?;

    if let Some(run_seconds) = args.run_seconds {
        handle.defer_for(
            Duration::from_secs(run_seconds.max(1)),
            CompletionKind::Timer,
            0,
            move |_| {
                let _ = stop_handle.stop();
            },
        )?;
    }

    println!(
        "task-node_started worker_node_id={} bind_port={} idle_interval_millis={}",
        args.worker_node_id, args.bind_port, args.idle_interval_millis
    );
    proactor.run_until_stopped()?;
    Ok(())
}

fn print_usage() {
    eprintln!(
        "usage: cargo run -p network --bin task-node -- \
         --worker-node-id <node> \
         --reply-endpoint <addr:port> \
         --execute-command <shell command> \
         --multicast-v6 <group%iface> [--multicast-v4 <group@interface>] \
         [--bind-port <port>] [--capability <tag>] [--artifact-hint <path>] \
         [--estimated-duration-seconds <n>] [--max-status-interval-seconds <n>] \
         [--note <text>] [--result-note <text>] [--ack-timeout-seconds <n>] \
         [--idle-interval-millis <n>] [--run-seconds <n>]"
    );
}

#[cfg(test)]
mod tests {
    use super::{matches_assignment, ActiveAssignment};

    #[test]
    fn assignment_match_requires_full_tuple() {
        let active = ActiveAssignment {
            assignment_id: 1,
            request_id: 2,
            offer_id: 3,
        };
        assert!(matches_assignment(&active, 1, 2, 3));
        assert!(!matches_assignment(&active, 9, 2, 3));
        assert!(!matches_assignment(&active, 1, 9, 3));
        assert!(!matches_assignment(&active, 1, 2, 9));
    }
}
