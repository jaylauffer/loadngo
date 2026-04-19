use anyhow::{anyhow, bail, Context, Result};
use data::{
    model_utils::now_timestamp,
    p2pmsg::{Message, TaskOffer},
    generate_id,
};
use network::{Config, MulticastConfig, Network};
use std::{
    collections::HashSet,
    env,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    time::{Duration, Instant},
};

struct Args {
    bind_port: u16,
    offerer_node_id: String,
    reply_endpoints: Vec<String>,
    summary: String,
    capability_tags: Vec<String>,
    artifact_hint: Option<String>,
    timeout_seconds: u64,
    multicast_v6: Vec<(Ipv6Addr, u32)>,
    multicast_v4: Vec<(Ipv4Addr, Ipv4Addr)>,
}

impl Args {
    fn parse() -> Result<Self> {
        let mut bind_port = 9850u16;
        let mut offerer_node_id = None;
        let mut reply_endpoints = Vec::new();
        let mut summary = None;
        let mut capability_tags = Vec::new();
        let mut artifact_hint = None;
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
                "--offerer-node-id" => {
                    offerer_node_id = Some(
                        args.next()
                            .context("missing value for --offerer-node-id")?,
                    );
                }
                "--reply-endpoint" => {
                    reply_endpoints.push(
                        args.next()
                            .context("missing value for --reply-endpoint")?,
                    );
                }
                "--summary" => {
                    summary = Some(args.next().context("missing value for --summary")?);
                }
                "--capability" => {
                    capability_tags.push(args.next().context("missing value for --capability")?);
                }
                "--artifact-hint" => {
                    artifact_hint = Some(args.next().context("missing value for --artifact-hint")?);
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
            offerer_node_id: offerer_node_id
                .ok_or_else(|| anyhow!("--offerer-node-id is required"))?,
            reply_endpoints,
            summary: summary.ok_or_else(|| anyhow!("--summary is required"))?,
            capability_tags,
            artifact_hint,
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
            .chain(args.multicast_v4.iter().map(|(group, interface)| MulticastConfig::V4 {
                group: *group,
                interface: *interface,
            }))
            .collect(),
        timeout: Duration::from_millis(250),
        retries: 1,
    };
    let mut network = Network::with_config(config);
    network.init()?;

    let created_at = now_timestamp();
    let offer = TaskOffer {
        offer_id: generate_id(),
        task_id: generate_id(),
        offerer_node_id: args.offerer_node_id,
        created_at,
        expires_at: created_at.saturating_add(args.timeout_seconds),
        summary: args.summary,
        capability_tags: args.capability_tags,
        reply_endpoints: args.reply_endpoints,
        artifact_hint: args.artifact_hint,
    };

    let sent = network.send_p2p_multicast_message(Message::TaskOffer(offer.clone()), false)?;
    println!(
        "task_offer_sent offer_id={} task_id={} bytes={} expires_at={}",
        offer.offer_id, offer.task_id, sent, offer.expires_at
    );

    let mut accepted_workers = HashSet::new();
    let deadline = Instant::now() + Duration::from_secs(args.timeout_seconds);
    while Instant::now() < deadline {
        let mut captured = None;
        if network.recv_and_dispatch_p2p(&mut |source, _header, message| {
            captured = Some((source, message));
        })? {
            if let Some((source, Message::TaskAccept(accept))) = captured {
                if accept.offer_id == offer.offer_id && accepted_workers.insert(accept.worker_node_id.clone()) {
                    println!(
                        "task_offer_accept offer_id={} worker_node_id={} source={} note={}",
                        accept.offer_id,
                        accept.worker_node_id,
                        source,
                        accept.note.unwrap_or_default()
                    );
                }
            }
        }
    }

    if accepted_workers.is_empty() {
        println!(
            "task_offer_timeout offer_id={} result=self-execute",
            offer.offer_id
        );
    } else {
        println!(
            "task_offer_complete offer_id={} accepted_workers={}",
            offer.offer_id,
            accepted_workers.len()
        );
    }

    Ok(())
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
         --offerer-node-id <node> \
         --reply-endpoint <addr:port> \
         --summary <text> \
         --multicast-v6 <group%iface> [--multicast-v4 <group@interface>] \
         [--bind-port <port>] [--capability <tag>] [--artifact-hint <path>] \
         [--timeout-seconds <n>]"
    );
}
