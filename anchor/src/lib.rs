//! Domain-separated, versioned hashing for records anchored into an external ledger.
//!
//! An anchor commits to a record by putting its hash in a ledger transaction. Hashing the
//! record's serialised bytes directly, as the first implementations did, leaves three
//! holes:
//!
//! - **No domain.** A reward receipt and a block payload that happen to serialise to the
//!   same bytes produce the same hash, so a commitment made for one purpose reads as a
//!   valid commitment for another.
//! - **No version.** Renaming, reordering or adding a field silently changes every future
//!   hash with nothing recording that the encoding moved. A verifier cannot tell an
//!   encoding change from a tampered record.
//! - **No length.** Concatenating variable-length pieces without lengths lets two
//!   different records frame to the same bytes.
//!
//! [`anchor_hash`] closes all three: it hashes a frame that carries a fixed prefix, the
//! domain, the version and the payload length before the payload itself. The frame is
//! the format; the payload stays whatever the caller already serialises, so adopting this
//! is a one-line change at each anchor site.
//!
//! ```text
//! frame = "loadngo-anchor-v1" 0x00 domain 0x00 version_le_u16 length_le_u64 payload
//! hash  = BLAKE3(frame)
//! ```
//!
//! Changing this framing is itself a breaking change: it would change every hash. That is
//! what the `v1` in the prefix is for, and why the golden vectors in this module's tests
//! are pinned.

#![forbid(unsafe_code)]

use serde::Serialize;

/// Marks every frame this module produces, so a hash cannot be confused with one taken
/// over a bare payload.
pub const FRAME_PREFIX: &[u8] = b"loadngo-anchor-v1";

/// A 32-byte anchor commitment.
pub type AnchorHash = [u8; 32];

/// Why a record could not be framed.
#[derive(Debug, thiserror::Error)]
pub enum AnchorError {
    #[error("an anchor domain must not be empty")]
    EmptyDomain,
    #[error("an anchor domain must not contain a NUL byte: {0:?}")]
    DomainHasNul(String),
    #[error("cannot serialise the {domain:?} payload: {source}")]
    Payload {
        domain: String,
        source: serde_json::Error,
    },
}

/// The exact bytes [`anchor_hash`] hashes. Useful for debugging a mismatch and for
/// pinning the format in tests; callers that only need the commitment use
/// [`anchor_hash`].
///
/// # Errors
///
/// Returns [`AnchorError`] when the domain is empty or contains a NUL byte, which would
/// make the domain field ambiguous.
pub fn anchor_frame(domain: &str, version: u16, payload: &[u8]) -> Result<Vec<u8>, AnchorError> {
    if domain.is_empty() {
        return Err(AnchorError::EmptyDomain);
    }
    if domain.as_bytes().contains(&0) {
        return Err(AnchorError::DomainHasNul(domain.to_owned()));
    }
    let mut frame =
        Vec::with_capacity(FRAME_PREFIX.len() + domain.len() + 2 + 2 + 8 + payload.len());
    frame.extend_from_slice(FRAME_PREFIX);
    frame.push(0);
    frame.extend_from_slice(domain.as_bytes());
    frame.push(0);
    frame.extend_from_slice(&version.to_le_bytes());
    frame.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    frame.extend_from_slice(payload);
    Ok(frame)
}

/// The anchor commitment for `payload` under `domain` at `version`.
///
/// # Errors
///
/// Returns [`AnchorError`] when the domain is empty or contains a NUL byte.
pub fn anchor_hash(domain: &str, version: u16, payload: &[u8]) -> Result<AnchorHash, AnchorError> {
    Ok(*blake3::hash(&anchor_frame(domain, version, payload)?).as_bytes())
}

/// [`anchor_hash`] over a value's JSON serialisation, which is what the existing anchor
/// sites already hash.
///
/// # Errors
///
/// Returns [`AnchorError`] when the domain is invalid or the value cannot be serialised.
pub fn anchor_hash_json<T: Serialize>(
    domain: &str,
    version: u16,
    value: &T,
) -> Result<AnchorHash, AnchorError> {
    let payload = serde_json::to_vec(value).map_err(|source| AnchorError::Payload {
        domain: domain.to_owned(),
        source,
    })?;
    anchor_hash(domain, version, &payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_frame_carries_prefix_domain_version_and_length_before_the_payload() {
        let frame = anchor_frame("a.b", 1, b"xy").unwrap();
        let mut want = Vec::new();
        want.extend_from_slice(b"loadngo-anchor-v1");
        want.push(0);
        want.extend_from_slice(b"a.b");
        want.push(0);
        want.extend_from_slice(&1_u16.to_le_bytes());
        want.extend_from_slice(&2_u64.to_le_bytes());
        want.extend_from_slice(b"xy");
        assert_eq!(frame, want);
    }

    #[test]
    fn a_hash_is_bound_to_its_domain_and_version() {
        let payload = br#"{"a":1}"#;
        let base = anchor_hash("eab.anchor.block", 1, payload).unwrap();
        assert_ne!(
            base,
            anchor_hash("loadngo.task.reward-receipt", 1, payload).unwrap()
        );
        assert_ne!(base, anchor_hash("eab.anchor.block", 2, payload).unwrap());
        // A bare BLAKE3 of the payload must not collide with a framed commitment.
        assert_ne!(base, *blake3::hash(payload).as_bytes());
    }

    #[test]
    fn the_length_prefix_stops_two_records_framing_to_the_same_bytes() {
        // Without a length, "ab" + "" and "a" + "b" would be indistinguishable once the
        // domain and payload are concatenated.
        assert_ne!(
            anchor_hash("d", 1, b"ab").unwrap(),
            anchor_hash("d", 1, b"a").unwrap()
        );
        assert_ne!(
            anchor_frame("d\u{0}x", 1, b"").ok(),
            Some(anchor_frame("d", 1, b"x").unwrap())
        );
    }

    #[test]
    fn a_domain_must_be_present_and_unambiguous() {
        assert!(matches!(
            anchor_hash("", 1, b""),
            Err(AnchorError::EmptyDomain)
        ));
        assert!(matches!(
            anchor_hash("a\u{0}b", 1, b""),
            Err(AnchorError::DomainHasNul(_))
        ));
    }

    /// These pin the v1 framing itself. A commitment already written into a ledger can
    /// only be re-verified by an encoder that reproduces these bytes, so if a change to
    /// this crate makes this test fail, that change has invalidated stored anchors and
    /// belongs behind a new version or a new prefix instead.
    #[test]
    fn the_v1_framing_is_pinned_by_golden_vectors() {
        fn hex(hash: &AnchorHash) -> String {
            hash.iter().map(|byte| format!("{byte:02x}")).collect()
        }
        let got = [
            hex(&anchor_hash("eab.anchor.block", 1, br#"{"height":1}"#).unwrap()),
            hex(&anchor_hash("loadngo.task.reward-receipt", 1, br#"{"request_id":10}"#).unwrap()),
            hex(&anchor_hash("d", 0, b"").unwrap()),
        ];
        assert_eq!(
            got,
            [
                "76b2fb626419ad24db9650f39f39ddec00acd4ee881a4308b38d1861a8a0faa4",
                "d528fe52346231978fba6e8807cd039edd3ea19c2b71d6fe40b0fd5746b73258",
                "4b7255bd3c28d5b6c53475f61f56fabe2b603bf610bc690206ff661bef6a3b42",
            ]
        );
    }

    #[test]
    fn json_anchoring_matches_hashing_the_same_json_bytes() {
        #[derive(Serialize)]
        struct Record {
            id: u32,
            note: &'static str,
        }
        let record = Record { id: 7, note: "hi" };
        let bytes = serde_json::to_vec(&record).unwrap();
        assert_eq!(
            anchor_hash_json("d", 3, &record).unwrap(),
            anchor_hash("d", 3, &bytes).unwrap()
        );
    }
}
