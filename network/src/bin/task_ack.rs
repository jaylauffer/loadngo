use anyhow::{anyhow, bail, Context, Result};
use data::{
    model_utils::now_timestamp,
    p2pmsg::{Message, TaskAck},
};
use network::{Config, Network};
use std::env;

struct Args {
    bind_port: u16,
    target: String,
    submitter_node_id: String,
    request_id: u64,
    offer_id: u64,
    assignment_id: u64,
    accepted: bool,
    qcoin_tx_hint: Option<String>,
    note: Option<String>,
}

impl Args {
    fn parse() -> Result<Self> {
        let mut bind_port = 9850u16;
        let mut target = None;
        let mut submitter_node_id = None;
        let mut request_id = None;
        let mut offer_id = None;
        let mut assignment_id = None;
        let mut accepted = None;
        let mut qcoin_tx_hint = None;
        let mut note = None;

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
                "--target" => {
                    target = Some(args.next().context("missing value for --target")?);
                }
                "--submitter-node-id" => {
                    submitter_node_id = Some(
                        args.next()
                            .context("missing value for --submitter-node-id")?,
                    );
                }
                "--request-id" => {
                    request_id = Some(
                        args.next()
                            .context("missing value for --request-id")?
                            .parse()
                            .context("invalid --request-id")?,
                    );
                }
                "--offer-id" => {
                    offer_id = Some(
                        args.next()
                            .context("missing value for --offer-id")?
                            .parse()
                            .context("invalid --offer-id")?,
                    );
                }
                "--assignment-id" => {
                    assignment_id = Some(
                        args.next()
                            .context("missing value for --assignment-id")?
                            .parse()
                            .context("invalid --assignment-id")?,
                    );
                }
                "--accepted" => {
                    accepted = Some(
                        args.next()
                            .context("missing value for --accepted")?
                            .parse()
                            .context("invalid --accepted, expected true or false")?,
                    );
                }
                "--qcoin-tx-hint" => {
                    qcoin_tx_hint = Some(args.next().context("missing value for --qcoin-tx-hint")?);
                }
                "--note" => {
                    note = Some(args.next().context("missing value for --note")?);
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
            target: target.ok_or_else(|| anyhow!("--target is required"))?,
            submitter_node_id: submitter_node_id
                .ok_or_else(|| anyhow!("--submitter-node-id is required"))?,
            request_id: request_id.ok_or_else(|| anyhow!("--request-id is required"))?,
            offer_id: offer_id.ok_or_else(|| anyhow!("--offer-id is required"))?,
            assignment_id: assignment_id.ok_or_else(|| anyhow!("--assignment-id is required"))?,
            accepted: accepted.ok_or_else(|| anyhow!("--accepted is required"))?,
            qcoin_tx_hint,
            note,
        })
    }
}

fn main() -> Result<()> {
    let args = Args::parse()?;
    let mut network = Network::with_config(Config::dual_stack(args.bind_port));
    network.init()?;

    let ack = TaskAck {
        assignment_id: args.assignment_id,
        request_id: args.request_id,
        offer_id: args.offer_id,
        submitter_node_id: args.submitter_node_id,
        acked_at: now_timestamp(),
        accepted: args.accepted,
        qcoin_tx_hint: args.qcoin_tx_hint,
        note: args.note,
    };

    network.send_p2p_message(&args.target, Message::TaskAck(ack.clone()), true)?;
    println!(
        "task_ack_sent assignment_id={} request_id={} offer_id={} target={} accepted={} qcoin_tx_hint={}",
        ack.assignment_id,
        ack.request_id,
        ack.offer_id,
        args.target,
        ack.accepted,
        ack.qcoin_tx_hint.unwrap_or_default()
    );
    Ok(())
}

fn print_usage() {
    eprintln!(
        "usage: cargo run -p network --bin task_ack -- \
         --target <addr:port> \
         --submitter-node-id <node> \
         --request-id <id> \
         --offer-id <id> \
         --assignment-id <id> \
         --accepted <true|false> \
         [--bind-port <port>] [--qcoin-tx-hint <text>] [--note <text>]"
    );
}
