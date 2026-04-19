use crate::{Config, MulticastConfig};
use anyhow::{anyhow, Context, Result};
use qcoin_types::{
    Block, Hash256, Output, Transaction, TransactionCore, TransactionKind, TransactionWitness,
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6},
    path::Path,
    time::Duration,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RewardReceipt {
    pub receipt_version: u32,
    pub request_id: u64,
    pub offer_id: u64,
    pub assignment_id: u64,
    pub submitter_node_id: String,
    pub worker_node_id: String,
    pub summary: String,
    pub success_criteria: Option<String>,
    pub artifact_hint: Option<String>,
    pub artifact_copy_path: Option<String>,
    pub artifact_hash_hex: Option<String>,
    pub result_note: Option<String>,
    pub accepted_at: u64,
    pub submitted_at: u64,
}

pub fn parse_multicast_v6(value: &str) -> Result<(Ipv6Addr, u32)> {
    let (group, interface) = value
        .split_once('%')
        .ok_or_else(|| anyhow!("expected --multicast-v6 as <group>%<interface>"))?;
    Ok((
        group.parse().context("invalid IPv6 multicast group")?,
        interface.parse().context("invalid IPv6 interface index")?,
    ))
}

pub fn parse_multicast_v4(value: &str) -> Result<(Ipv4Addr, Ipv4Addr)> {
    let (group, interface) = value
        .split_once('@')
        .ok_or_else(|| anyhow!("expected --multicast-v4 as <group@interface>"))?;
    Ok((
        group.parse().context("invalid IPv4 multicast group")?,
        interface.parse().context("invalid IPv4 interface")?,
    ))
}

pub fn task_network_config(
    bind_port: u16,
    multicast_v6: &[(Ipv6Addr, u32)],
    multicast_v4: &[(Ipv4Addr, Ipv4Addr)],
) -> Config {
    Config {
        bind_addr: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, bind_port)),
        extra_bind_addrs: vec![SocketAddr::V6(SocketAddrV6::new(
            Ipv6Addr::UNSPECIFIED,
            bind_port,
            0,
            0,
        ))],
        multicast: multicast_v6
            .iter()
            .map(|(group, interface)| MulticastConfig::V6 {
                group: *group,
                interface: *interface,
            })
            .chain(
                multicast_v4
                    .iter()
                    .map(|(group, interface)| MulticastConfig::V4 {
                        group: *group,
                        interface: *interface,
                    }),
            )
            .collect(),
        multicast_target_port: None,
        timeout: Duration::from_millis(250),
        retries: 1,
    }
}

pub fn endpoint_host(endpoint: &str) -> Result<String> {
    let addr: SocketAddr = endpoint
        .parse()
        .with_context(|| format!("invalid socket endpoint: {endpoint}"))?;
    Ok(addr.ip().to_string())
}

pub fn artifact_hash_hex(path: &Path) -> Result<Option<String>> {
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(path)
        .with_context(|| format!("failed to read artifact for hashing: {}", path.display()))?;
    Ok(Some(hex::encode(blake3::hash(&bytes).as_bytes())))
}

pub fn reward_receipt_bytes(receipt: &RewardReceipt) -> Result<Vec<u8>> {
    serde_json::to_vec(receipt).context("failed to serialize reward receipt")
}

pub fn reward_metadata_hash(receipt: &RewardReceipt) -> Result<Hash256> {
    let bytes = reward_receipt_bytes(receipt)?;
    Ok(*blake3::hash(&bytes).as_bytes())
}

pub fn reward_owner_hash(worker_node_id: &str) -> Hash256 {
    *blake3::hash(worker_node_id.as_bytes()).as_bytes()
}

pub fn reward_transaction(receipt: &RewardReceipt) -> Result<Transaction> {
    Ok(Transaction {
        core: TransactionCore {
            kind: TransactionKind::Transfer,
            inputs: vec![],
            outputs: vec![Output {
                owner_script_hash: reward_owner_hash(&receipt.worker_node_id),
                assets: vec![],
                metadata_hash: Some(reward_metadata_hash(receipt)?),
            }],
        },
        witness: TransactionWitness::default(),
    })
}

pub fn tx_id_hex(tx: &Transaction) -> String {
    hex::encode(tx.tx_id())
}

pub fn block_contains_tx_id(block: &Block, tx_id: &Hash256) -> bool {
    block.transactions.iter().any(|tx| &tx.tx_id() == tx_id)
}

#[cfg(test)]
mod tests {
    use super::{
        artifact_hash_hex, block_contains_tx_id, reward_metadata_hash, reward_owner_hash,
        reward_transaction, tx_id_hex, RewardReceipt,
    };
    use qcoin_crypto::{Dilithium2Scheme, PqSignatureScheme};
    use qcoin_types::{Output, Transaction, TransactionCore, TransactionKind, TransactionWitness};
    use std::fs;

    fn sample_receipt() -> RewardReceipt {
        RewardReceipt {
            receipt_version: 1,
            request_id: 10,
            offer_id: 11,
            assignment_id: 12,
            submitter_node_id: "submitter".to_string(),
            worker_node_id: "worker".to_string(),
            summary: "Produce a feedback receipt".to_string(),
            success_criteria: Some("artifact exists".to_string()),
            artifact_hint: Some("/tmp/feedback.md".to_string()),
            artifact_copy_path: Some("artifacts/feedback.md".to_string()),
            artifact_hash_hex: Some("abcd".to_string()),
            result_note: Some("done".to_string()),
            accepted_at: 20,
            submitted_at: 18,
        }
    }

    #[test]
    fn reward_transaction_uses_metadata_only_transfer() {
        let receipt = sample_receipt();
        let tx = reward_transaction(&receipt).unwrap();
        assert_eq!(tx.core.kind, TransactionKind::Transfer);
        assert!(tx.core.inputs.is_empty());
        assert_eq!(tx.core.outputs.len(), 1);
        assert!(tx.core.outputs[0].assets.is_empty());
        assert_eq!(
            tx.core.outputs[0].owner_script_hash,
            reward_owner_hash(&receipt.worker_node_id)
        );
        assert_eq!(
            tx.core.outputs[0].metadata_hash,
            Some(reward_metadata_hash(&receipt).unwrap())
        );
    }

    #[test]
    fn tx_id_hex_is_stable_hex() {
        let receipt = sample_receipt();
        let tx = reward_transaction(&receipt).unwrap();
        let hex = tx_id_hex(&tx);
        assert_eq!(hex.len(), 64);
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn artifact_hash_matches_file_contents() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("artifact.txt");
        fs::write(&path, b"meaningful artifact").unwrap();
        let actual = artifact_hash_hex(&path).unwrap();
        let expected = Some(hex::encode(blake3::hash(b"meaningful artifact").as_bytes()));
        assert_eq!(actual, expected);
    }

    #[test]
    fn block_detection_finds_expected_transaction() {
        let receipt = sample_receipt();
        let tx = reward_transaction(&receipt).unwrap();
        let scheme = Dilithium2Scheme;
        let (public_key, private_key) = scheme.keygen().unwrap();
        let signature = scheme.sign(&private_key, b"test block").unwrap();
        let block = qcoin_types::Block {
            header: qcoin_types::BlockHeader {
                parent_hash: [0u8; 32],
                state_root: [1u8; 32],
                tx_root: [2u8; 32],
                height: 1,
                timestamp: 3,
            },
            transactions: vec![
                Transaction {
                    core: TransactionCore {
                        kind: TransactionKind::Transfer,
                        inputs: vec![],
                        outputs: vec![Output {
                            owner_script_hash: [9u8; 32],
                            assets: vec![],
                            metadata_hash: None,
                        }],
                    },
                    witness: TransactionWitness::default(),
                },
                tx.clone(),
            ],
            proposer_public_key: public_key,
            signature,
        };
        assert!(block_contains_tx_id(&block, &tx.tx_id()));
    }
}
