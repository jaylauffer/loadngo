use anyhow::{anyhow, bail, Context, Result};
use data::{
    generate_id,
    model_utils::now_timestamp,
    p2pmsg::{Message, TaskOffer, TaskRequest},
};
use network::{Config, MulticastConfig, Network};
use std::{
    collections::HashSet,
    env,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
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
    timeout_seconds: u64,
    multicast_v6: Vec<(Ipv6Addr, u32)>,
    multicast_v4: Vec<(Ipv4Addr, Ipv4Addr)>,
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
        let mut timeout_seconds = 5u64;
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
                "--timeout-seconds" => {
                    timeout_seconds = args
                        .next()
                        .context("missing value for --timeout-seconds")?
                        .parse()
                        .context("invalid --timeout-seconds")?;
                }
                "--multicast-v6" => {
                    let value = args.next().context("missing value for --multicast-v6")?;
                    let (group, interface) = parse_multicast_v6(&value)?;
                    multicast_v6.push((group, interface));
                }
                "--multicast-v4" => {
                    let value = args.next().context("missing value for --multicast-v4")?;
                    let (group, interface) = parse_multicast_v4(&value)?;
                    multicast_v4.push((group, interface));
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
            timeout_seconds,
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

    let config = Config {
        bind_addr: SocketAddr::from(([0, 0, 0, 0], args.bind_port)),
        extra_bind_addrs: vec![SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 0], args.bind_port))],
        multicast: args
            .multicast_v6
            .iter()
            .map(|(group, interface)| MulticastConfig::V6 {
                group: *group,
                interface: *interface,
            })
            .chain(
                args.multicast_v4
                    .iter()
                    .map(|(group, interface)| MulticastConfig::V4 {
                        group: *group,
                        interface: *interface,
                    }),
            )
            .collect(),
        timeout: Duration::from_millis(250),
        retries: 1,
    };
    let mut network = Network::with_config(config);
    network.init()?;

    let mut responded_requests = HashSet::new();
    let mut offers_sent = 0usize;
    let deadline = Instant::now() + Duration::from_secs(args.timeout_seconds);
    while Instant::now() < deadline {
        let mut captured = None;
        if network.recv_and_dispatch_p2p(&mut |source, _header, message| {
            captured = Some((source, message));
        })? {
            if let Some((source, Message::TaskRequest(request))) = captured {
                if handle_request(&network, &args, &mut responded_requests, source, request)? {
                    offers_sent += 1;
                }
            }
        } else {
            thread::sleep(Duration::from_millis(25));
        }
    }

    println!(
        "task_offer_complete worker_node_id={} offers_sent={}",
        args.worker_node_id, offers_sent
    );

    Ok(())
}

fn handle_request(
    network: &Network,
    args: &Args,
    responded_requests: &mut HashSet<u64>,
    source: SocketAddr,
    request: TaskRequest,
) -> Result<bool> {
    if now_timestamp() > request.expires_at {
        return Ok(false);
    }
    if !responded_requests.insert(request.request_id) {
        return Ok(false);
    }

    let capabilities_match = request
        .capability_tags
        .iter()
        .all(|tag| args.capability_tags.contains(tag));
    println!(
        "task_offer_request request_id={} submitter_node_id={} source={} capabilities_match={} summary={}",
        request.request_id,
        request.submitter_node_id,
        source,
        capabilities_match,
        request.summary
    );

    if !capabilities_match || request.reply_endpoints.is_empty() {
        return Ok(false);
    }

    let created_at = now_timestamp();
    let offer = TaskOffer {
        offer_id: generate_id(),
        request_id: request.request_id,
        worker_node_id: args.worker_node_id.clone(),
        created_at,
        expires_at: created_at.saturating_add(args.timeout_seconds),
        capability_tags: args.capability_tags.clone(),
        reply_endpoints: args.reply_endpoints.clone(),
        estimated_duration_secs: args.estimated_duration_secs,
        max_status_interval_secs: args.max_status_interval_secs,
        note: args.note.clone(),
        artifact_hint: args.artifact_hint.clone(),
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

    if sent_targets > 0 {
        println!(
            "task_offer_sent request_id={} offer_id={} worker_node_id={} targets={} expires_at={}",
            offer.request_id, offer.offer_id, offer.worker_node_id, sent_targets, offer.expires_at
        );
        Ok(true)
    } else {
        Ok(false)
    }
}

fn parse_multicast_v6(value: &str) -> Result<(Ipv6Addr, u32)> {
    let (group, interface) = value
        .split_once('%')
        .ok_or_else(|| anyhow!("expected --multicast-v6 as <group>%<interface>"))?;
    Ok((
        group.parse().context("invalid IPv6 multicast group")?,
        interface.parse().context("invalid IPv6 interface index")?,
    ))
}

fn parse_multicast_v4(value: &str) -> Result<(Ipv4Addr, Ipv4Addr)> {
    let (group, interface) = value
        .split_once('@')
        .ok_or_else(|| anyhow!("expected --multicast-v4 as <group>@<interface>"))?;
    Ok((
        group.parse().context("invalid IPv4 multicast group")?,
        interface.parse().context("invalid IPv4 interface")?,
    ))
}

fn print_usage() {
    eprintln!(
        "usage: cargo run -p network --bin task_offer -- \
         --worker-node-id <node> \
         --reply-endpoint <addr:port> \
         --multicast-v6 <group%iface> [--multicast-v4 <group@interface>] \
         [--bind-port <port>] [--capability <tag>] [--estimated-duration-seconds <n>] \
         [--max-status-interval-seconds <n>] [--artifact-hint <path>] [--note <text>] \
         [--timeout-seconds <n>]"
    );
}
