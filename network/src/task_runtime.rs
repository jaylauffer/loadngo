use crate::{Config, MulticastConfig};
use anyhow::{anyhow, Context, Result};
use data::p2pmsg::RewardPayee;
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
    /// The scheme and payee agreed in `TaskAccept`; `None` for unrewarded work.
    /// Added in receipt version 2.
    #[serde(default)]
    pub reward: Option<RewardPayee>,
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

/// The anchor domain and version this receipt's commitment is bound to. Bump the version
/// with any change to `RewardReceipt`'s fields or their order: the hash a ledger already
/// carries stays verifiable only against the encoding that produced it. Version 2 added
/// `reward`.
pub const REWARD_RECEIPT_DOMAIN: &str = "loadngo.task.reward-receipt";
pub const REWARD_RECEIPT_VERSION: u16 = 2;

/// The receipt's anchor commitment: what a reward scheme records, for example as a
/// QCoin output's `metadata_hash`.
pub fn reward_receipt_commitment(receipt: &RewardReceipt) -> Result<[u8; 32]> {
    let bytes = reward_receipt_bytes(receipt)?;
    loadngo_anchor::anchor_hash(REWARD_RECEIPT_DOMAIN, REWARD_RECEIPT_VERSION, &bytes)
        .context("failed to frame the reward receipt for anchoring")
}

/// Bytes of each stream of a check's output that a closure record keeps: the end of it,
/// where a failing check says why.
pub const CHECK_OUTPUT_KEPT: usize = 64 * 1024;
/// Characters of the check's last line that a rejection's `TaskAck` note carries.
const NOTE_LINE_CHARS: usize = 200;

/// What the submitter's check printed and how it exited, kept in the closure record for
/// every result it checked: the proof of why work was accepted or rejected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verification {
    pub command: String,
    /// The exit code; `None` when a signal ended the check.
    pub exit_code: Option<i32>,
    /// The end of stdout, at most [`CHECK_OUTPUT_KEPT`] bytes.
    pub stdout: String,
    /// Bytes dropped from the start of stdout.
    pub stdout_dropped_bytes: usize,
    pub stderr: String,
    pub stderr_dropped_bytes: usize,
}

impl Verification {
    #[must_use]
    pub fn new(command: &str, output: &std::process::Output) -> Self {
        Self::from_parts(
            command,
            output.status.code(),
            &output.stdout,
            &output.stderr,
        )
    }

    #[must_use]
    pub fn from_parts(command: &str, exit_code: Option<i32>, stdout: &[u8], stderr: &[u8]) -> Self {
        let (stdout, stdout_dropped_bytes) = tail(stdout, CHECK_OUTPUT_KEPT);
        let (stderr, stderr_dropped_bytes) = tail(stderr, CHECK_OUTPUT_KEPT);
        Self {
            command: command.to_owned(),
            exit_code,
            stdout,
            stdout_dropped_bytes,
            stderr,
            stderr_dropped_bytes,
        }
    }

    /// The check passed: it exited with 0.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.exit_code == Some(0)
    }

    /// The `TaskAck` note for a rejection: how the check exited and the last line it
    /// printed (stderr first), so the worker learns why.
    #[must_use]
    pub fn rejection_note(&self) -> String {
        let exit = self.exit_code.map_or_else(
            || "ended by a signal".to_owned(),
            |code| format!("exit {code}"),
        );
        let last = [&self.stderr, &self.stdout]
            .iter()
            .find_map(|text| text.lines().rev().map(str::trim).find(|l| !l.is_empty()));
        match last {
            Some(line) => format!(
                "verification failed ({exit}): {}",
                line.chars().take(NOTE_LINE_CHARS).collect::<String>()
            ),
            None => format!("verification failed ({exit})"),
        }
    }
}

/// The end of `bytes`, at most `limit` of them, starting on a character boundary: the
/// text, and how many bytes came before it.
fn tail(bytes: &[u8], limit: usize) -> (String, usize) {
    let mut start = bytes.len().saturating_sub(limit);
    while start < bytes.len() && bytes[start] & 0xC0 == 0x80 {
        start += 1;
    }
    (String::from_utf8_lossy(&bytes[start..]).into_owned(), start)
}

#[cfg(test)]
mod tests {
    use super::{
        artifact_hash_hex, reward_receipt_bytes, reward_receipt_commitment, tail, RewardReceipt,
        Verification, CHECK_OUTPUT_KEPT, REWARD_RECEIPT_DOMAIN, REWARD_RECEIPT_VERSION,
    };
    use data::p2pmsg::RewardPayee;
    use std::fs;

    fn sample_receipt() -> RewardReceipt {
        RewardReceipt {
            receipt_version: REWARD_RECEIPT_VERSION as u32,
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
            reward: Some(RewardPayee {
                scheme: "qcoin".to_string(),
                payee: "cd".repeat(32),
            }),
        }
    }

    #[test]
    fn commitment_is_the_framed_receipt_hash() {
        let receipt = sample_receipt();
        let expected = loadngo_anchor::anchor_hash(
            REWARD_RECEIPT_DOMAIN,
            REWARD_RECEIPT_VERSION,
            &reward_receipt_bytes(&receipt).unwrap(),
        )
        .unwrap();
        assert_eq!(reward_receipt_commitment(&receipt).unwrap(), expected);
    }

    #[test]
    fn commitment_binds_the_agreed_payee() {
        let receipt = sample_receipt();
        let mut other = receipt.clone();
        other.reward = Some(RewardPayee {
            scheme: "qcoin".to_string(),
            payee: "ef".repeat(32),
        });
        assert_ne!(
            reward_receipt_commitment(&receipt).unwrap(),
            reward_receipt_commitment(&other).unwrap()
        );
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
    fn a_check_output_keeps_its_end_from_a_character_boundary() {
        assert_eq!(tail(b"short", 64), ("short".to_owned(), 0));
        // 'é' is two bytes; a cut through it moves forward to the next character.
        let text = "aé\nlast line";
        assert_eq!(tail(text.as_bytes(), 11), ("\nlast line".to_owned(), 3));
        assert_eq!(tail(text.as_bytes(), 12), ("é\nlast line".to_owned(), 1));

        let long = "x".repeat(CHECK_OUTPUT_KEPT + 10) + "\nverify: FAIL: why";
        let kept = Verification::from_parts("check", Some(1), b"", long.as_bytes());
        assert_eq!(kept.stderr.len(), CHECK_OUTPUT_KEPT);
        assert_eq!(kept.stderr_dropped_bytes, long.len() - CHECK_OUTPUT_KEPT);
        assert!(kept.stderr.ends_with("verify: FAIL: why"));
    }

    #[test]
    fn a_rejection_note_carries_how_the_check_exited_and_its_last_line() {
        let failed = Verification::from_parts(
            "check",
            Some(3),
            b"checking\n",
            b"verify: FAIL: no commit\n\n",
        );
        assert!(!failed.passed());
        assert_eq!(
            failed.rejection_note(),
            "verification failed (exit 3): verify: FAIL: no commit"
        );
        // Nothing on stderr: the last line of stdout.
        let quiet = Verification::from_parts("check", Some(1), b"one\ntwo\n", b"");
        assert_eq!(quiet.rejection_note(), "verification failed (exit 1): two");
        let killed = Verification::from_parts("check", None, b"", b"");
        assert_eq!(
            killed.rejection_note(),
            "verification failed (ended by a signal)"
        );
        let long = Verification::from_parts("check", Some(1), b"", "y".repeat(500).as_bytes());
        assert_eq!(
            long.rejection_note().len(),
            "verification failed (exit 1): ".len() + 200
        );
        assert!(Verification::from_parts("check", Some(0), b"ok", b"").passed());
    }
}
