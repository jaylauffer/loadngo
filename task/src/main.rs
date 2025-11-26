//! Task application entrypoint (Rust port skeleton).

use anyhow::Result;
use data::{now_timestamp, Participant, Sync};
use network::Network;
use tracing_subscriber::EnvFilter;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let mut network = Network::new();
    network.init()?;

    let mut sync = Sync::default();
    let participant = Participant::new(1, 1, "127.0.0.1:0", now_timestamp());
    network.register_participant(&participant);
    sync.add_participant(participant);

    network.send_sync_request(0)?;

    Ok(())
}
