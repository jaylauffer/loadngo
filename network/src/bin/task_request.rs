use anyhow::{anyhow, bail, Context, Result};
use data::{
    model_utils::now_timestamp,
    p2pmsg::{Message, TaskOffer, TaskRequest},
    generate_id,
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
                    worker_node_id = Some(
                        args.next()
                            .context("missing value for --worker-node-id")?,
                    );
                }
                "--reply-endpoint" => {
                    reply_endpoints.push(
                        args.next()
                            .context("missing value for --reply-endpoint")?,
                    );
                }
                "--capability" => {
                    capability_tags.push(args.next().context("missing value for --capability")?);
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
    let request = TaskRequest {
        request_id: generate_id(),
        worker_node_id: args.worker_node_id,
        created_at,
        expires_at: created_at.saturating_add(args.timeout_seconds),
        capability_tags: args.capability_tags,
        reply_endpoints: args.reply_endpoints,
        note: args.note,
    };

    let sent = network.send_p2p_multicast_message(Message::TaskRequest(request.clone()), false)?;
    println!(
        "task_request_sent request_id={} worker_node_id={} bytes={} expires_at={}",
        request.request_id, request.worker_node_id, sent, request.expires_at
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
    if !seen_offers.insert(offer.offer_id) {
        return;
    }

    let matches_request = offer
        .capability_tags
        .iter()
        .all(|tag| request.capability_tags.contains(tag));

    println!(
        "task_request_offer request_id={} offer_id={} task_id={} source={} matches_request={} summary={}",
        request.request_id,
        offer.offer_id,
        offer.task_id,
        source,
        matches_request,
        offer.summary
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
         --worker-node-id <node> \
         --reply-endpoint <addr:port> \
         --multicast-v6 <group%iface> [--multicast-v4 <group@interface>] \
         [--bind-port <port>] [--capability <tag>] [--note <text>] \
         [--timeout-seconds <n>]"
    );
}
