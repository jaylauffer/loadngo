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
    submitter_node_id: String,
    reply_endpoints: Vec<String>,
    summary: String,
    capability_tags: Vec<String>,
    requested_duration_secs: Option<u64>,
    success_criteria: Option<String>,
    artifact_hint: Option<String>,
    note: Option<String>,
    timeout_seconds: u64,
    multicast_v6: Vec<(Ipv6Addr, u32)>,
    multicast_v4: Vec<(Ipv4Addr, Ipv4Addr)>,
}

impl Args {
    fn parse() -> Result<Self> {
        let mut bind_port = 9850u16;
        let mut submitter_node_id = None;
        let mut reply_endpoints = Vec::new();
        let mut summary = None;
        let mut capability_tags = Vec::new();
        let mut requested_duration_secs = None;
        let mut success_criteria = None;
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
            submitter_node_id: submitter_node_id
                .ok_or_else(|| anyhow!("--submitter-node-id is required"))?,
            reply_endpoints,
            summary: summary.ok_or_else(|| anyhow!("--summary is required"))?,
            capability_tags,
            requested_duration_secs,
            success_criteria,
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

    let created_at = now_timestamp();
    let request = TaskRequest {
        request_id: generate_id(),
        submitter_node_id: args.submitter_node_id,
        created_at,
        expires_at: created_at.saturating_add(args.timeout_seconds),
        summary: args.summary,
        capability_tags: args.capability_tags,
        reply_endpoints: args.reply_endpoints,
        requested_duration_secs: args.requested_duration_secs,
        success_criteria: args.success_criteria,
        artifact_hint: args.artifact_hint,
        note: args.note,
    };

    let sent = network.send_p2p_multicast_message(Message::TaskRequest(request.clone()), false)?;
    println!(
        "task_request_sent request_id={} submitter_node_id={} bytes={} expires_at={}",
        request.request_id, request.submitter_node_id, sent, request.expires_at
    );

    let mut seen_offers = HashSet::new();
    let deadline = Instant::now() + Duration::from_secs(args.timeout_seconds);
    while Instant::now() < deadline {
        let mut captured = None;
        if network.recv_and_dispatch_p2p(&mut |source, _header, message| {
            captured = Some((source, message));
        })? {
            if let Some((source, Message::TaskOffer(offer))) = captured {
                handle_offer(&request, &mut seen_offers, source, offer);
            }
        } else {
            thread::sleep(Duration::from_millis(25));
        }
    }

    if seen_offers.is_empty() {
        println!(
            "task_request_timeout request_id={} result=no-offers",
            request.request_id
        );
    } else {
        println!(
            "task_request_complete request_id={} offers_seen={}",
            request.request_id,
            seen_offers.len()
        );
    }

    Ok(())
}

fn handle_offer(
    request: &TaskRequest,
    seen_offers: &mut HashSet<u64>,
    source: SocketAddr,
    offer: TaskOffer,
) {
    if offer.request_id != request.request_id {
        return;
    }

    if !seen_offers.insert(offer.offer_id) {
        return;
    }

    let offer_capability_tags = &offer.capability_tags;
    let matches_request = request
        .capability_tags
        .iter()
        .all(|tag| offer_capability_tags.contains(tag));

    println!(
        "task_request_offer request_id={} offer_id={} source={} matches_request={} worker_node_id={} estimated_duration_secs={} max_status_interval_secs={} note={}",
        request.request_id,
        offer.offer_id,
        source,
        matches_request,
        offer.worker_node_id,
        offer.estimated_duration_secs.unwrap_or_default(),
        offer.max_status_interval_secs.unwrap_or_default(),
        offer.note.unwrap_or_default()
    );
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
        "usage: cargo run -p network --bin task_request -- \
         --submitter-node-id <node> \
         --reply-endpoint <addr:port> \
         --summary <text> \
         --multicast-v6 <group%iface> [--multicast-v4 <group@interface>] \
         [--bind-port <port>] [--capability <tag>] [--requested-duration-seconds <n>] \
         [--success-criteria <text>] [--artifact-hint <path>] [--note <text>] \
         [--timeout-seconds <n>]"
    );
}
