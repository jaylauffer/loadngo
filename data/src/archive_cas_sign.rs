//! Signs and verifies an Archive CAS manifest root with a post-quantum key.
//!
//! An Archive CAS proves integrity: every object is named by its BLAKE3
//! digest, so a root object pins every byte beneath it. It says nothing
//! about who vouches for that root. This binds a root to a named signer.
//!
//! The signature covers a `loadngo-anchor` frame (domain
//! `loadngo.archive-cas.root-signature`, version 1) around the JSON of the
//! signed claim: format, archive id, root object, signer and signing time.
//! Every field a verifier relies on is inside the signed bytes, and the
//! domain stops the signature being replayed as one over any other kind of
//! record.
//!
//! A signature covers the root only, not the blobs beneath it -- pair it
//! with [`crate::archive_cas`]'s own verification (or the `archive_cas_verify`
//! binary), which re-hashes every object.
//!
//! This is the library the `archive_cas_sign` CLI and the `archive_cas_browser`
//! GUI both call, so there is exactly one place that builds a signed claim.

use crate::archive_cas::{ArchiveCasStorage, ArchiveManifest, ArchiveObject};
use crate::cas::CasHash;
use anyhow::{anyhow, bail, Context, Result};
use loadngo_pq_crypto::{default_registry, PqSchemeRegistry, PrivateKey, PublicKey, Signature};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

pub const SIGNED_ROOT_FORMAT_V1: &str = "loadngo-archive-root-signature-v1";
pub const ANCHOR_DOMAIN: &str = "loadngo.archive-cas.root-signature";
pub const ANCHOR_VERSION: u16 = 1;

/// The fields a signature vouches for, in the order they are serialised.
#[derive(Serialize)]
struct Claim<'a> {
    format: &'a str,
    archive_id: &'a str,
    root_object: &'a str,
    signer_identity: &'a str,
    signed_at_unix_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedArchiveRoot {
    pub format: String,
    pub archive_id: String,
    pub root_object: String,
    pub signer_identity: String,
    pub signed_at_unix_secs: u64,
    pub signature_scheme: String,
    pub public_key_hex: String,
    pub signature_hex: String,
}

impl SignedArchiveRoot {
    pub fn signed_message(&self) -> Result<Vec<u8>> {
        claim_message(
            &self.archive_id,
            &self.root_object,
            &self.signer_identity,
            self.signed_at_unix_secs,
        )
    }
}

pub fn claim_message(
    archive_id: &str,
    root_object: &str,
    signer_identity: &str,
    signed_at_unix_secs: u64,
) -> Result<Vec<u8>> {
    let claim = Claim {
        format: SIGNED_ROOT_FORMAT_V1,
        archive_id,
        root_object,
        signer_identity,
        signed_at_unix_secs,
    };
    let payload = serde_json::to_vec(&claim).context("failed to serialise the signed claim")?;
    loadngo_anchor::anchor_frame(ANCHOR_DOMAIN, ANCHOR_VERSION, &payload)
        .context("failed to frame the signed claim")
}

pub fn sign_root(
    archive_id: &str,
    root: CasHash,
    signer_identity: &str,
    signed_at_unix_secs: u64,
    public_key: &PublicKey,
    private_key: &PrivateKey,
) -> Result<SignedArchiveRoot> {
    if signer_identity.trim().is_empty() {
        bail!("signer identity must not be empty");
    }
    if public_key.scheme != private_key.scheme {
        bail!(
            "public/private key scheme mismatch: {} vs {}",
            public_key.scheme,
            private_key.scheme
        );
    }
    let registry = default_registry();
    let scheme = registry
        .get(&private_key.scheme)
        .ok_or_else(|| anyhow!("signature scheme not registered: {}", private_key.scheme))?;
    let root_object = root.to_hex();
    let message = claim_message(
        archive_id,
        &root_object,
        signer_identity,
        signed_at_unix_secs,
    )?;
    let signature = scheme
        .sign(private_key, &message)
        .context("failed to sign the archive root")?;
    Ok(SignedArchiveRoot {
        format: SIGNED_ROOT_FORMAT_V1.to_string(),
        archive_id: archive_id.to_string(),
        root_object,
        signer_identity: signer_identity.to_string(),
        signed_at_unix_secs,
        signature_scheme: public_key.scheme.to_string(),
        public_key_hex: hex::encode(public_key.to_bytes().context("encode public key")?),
        signature_hex: hex::encode(signature.to_bytes().context("encode signature")?),
    })
}

/// Checks the signature itself and that it was made by `trusted`. Says nothing
/// about whether the root object is present; the caller checks that.
pub fn verify_signature(signed: &SignedArchiveRoot, trusted: &PublicKey) -> Result<CasHash> {
    if signed.format != SIGNED_ROOT_FORMAT_V1 {
        bail!("unsupported signature format: {}", signed.format);
    }
    let public_key = PublicKey::from_bytes(
        &hex::decode(signed.public_key_hex.trim()).context("invalid public_key_hex")?,
    )
    .context("invalid encoded public key")?;
    if public_key.to_bytes()? != trusted.to_bytes()? {
        bail!(
            "archive root was signed by {:?} with a key that is not the trusted key",
            signed.signer_identity
        );
    }
    let signature = Signature::from_bytes(
        &hex::decode(signed.signature_hex.trim()).context("invalid signature_hex")?,
    )
    .context("invalid encoded signature")?;
    if signature.scheme != public_key.scheme
        || signed.signature_scheme != public_key.scheme.to_string()
    {
        bail!(
            "signature scheme mismatch: declared {}, key {}, signature {}",
            signed.signature_scheme,
            public_key.scheme,
            signature.scheme
        );
    }
    let registry = default_registry();
    let scheme = registry
        .get(&public_key.scheme)
        .ok_or_else(|| anyhow!("signature scheme not registered: {}", public_key.scheme))?;
    scheme
        .verify(&public_key, &signed.signed_message()?, &signature)
        .context("signature does not match the signed claim")?;
    parse_root(&signed.root_object)
}

fn parse_root(hex_root: &str) -> Result<CasHash> {
    CasHash::from_slice(&hex::decode(hex_root.trim()).context("root object is not hex")?)
        .context("root object is not a 32-byte digest")
}

fn read_key<T>(path: &Path, parse: impl FnOnce(&[u8]) -> Result<T>) -> Result<T> {
    let text =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let bytes =
        hex::decode(text.trim()).with_context(|| format!("invalid hex in {}", path.display()))?;
    parse(&bytes).with_context(|| format!("invalid key in {}", path.display()))
}

pub fn read_public_key(path: &Path) -> Result<PublicKey> {
    read_key(path, |b| PublicKey::from_bytes(b).map_err(Into::into))
}

pub fn read_private_key(path: &Path) -> Result<PrivateKey> {
    read_key(path, |b| PrivateKey::from_bytes(b).map_err(Into::into))
}

/// The manifest's root digest, after confirming the root object is stored
/// and intact.
pub fn present_root(store: &ArchiveCasStorage, manifest: &ArchiveManifest) -> Result<CasHash> {
    let bytes = manifest.canonical_bytes()?;
    let object = ArchiveObject {
        hash: manifest.digest()?,
        size: u64::try_from(bytes.len()).context("manifest length exceeds u64")?,
    };
    store
        .verify_object(object)
        .context("archive manifest is not present as a verified CAS object")?;
    Ok(object.hash)
}

/// The signature file path a manifest's signature is (or would be) stored
/// at -- `{cas_root}/manifests/{archive_id}-{root_hex}.signature.json`.
pub fn signature_path(store: &ArchiveCasStorage, archive_id: &str, root: CasHash) -> PathBuf {
    store
        .manifests_root()
        .join(format!("{archive_id}-{}.signature.json", root.to_hex()))
}

/// Signs a manifest already present in `store` and writes the signature
/// file, refusing to overwrite one that already exists. This is the single
/// entry point both the CLI and the GUI browser use -- it always verifies
/// the fresh signature before writing it, and never signs a manifest whose
/// root object isn't actually present and hash-correct in the CAS.
pub fn sign_manifest_and_write(
    store: &ArchiveCasStorage,
    manifest: &ArchiveManifest,
    signer_identity: &str,
    public_key: &PublicKey,
    private_key: &PrivateKey,
    signed_at_unix_secs: u64,
) -> Result<(PathBuf, SignedArchiveRoot)> {
    let root = present_root(store, manifest)?;
    let signed = sign_root(
        &manifest.archive_id,
        root,
        signer_identity,
        signed_at_unix_secs,
        public_key,
        private_key,
    )?;
    // Never write a signature that does not verify.
    verify_signature(&signed, public_key).context("freshly made signature failed to verify")?;

    let out = signature_path(store, &manifest.archive_id, root);
    if out.exists() {
        bail!("refusing to overwrite existing signature {}", out.display());
    }
    let json = serde_json::to_vec_pretty(&signed).context("failed to serialise signature")?;
    fs::write(&out, json).with_context(|| format!("failed to write {}", out.display()))?;
    Ok((out, signed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use loadngo_pq_crypto::SignatureSchemeId;

    fn keys() -> (PublicKey, PrivateKey) {
        default_registry()
            .get(&SignatureSchemeId::Dilithium2)
            .unwrap()
            .keygen()
            .unwrap()
    }

    fn root() -> CasHash {
        CasHash::digest(b"an archive manifest")
    }

    #[test]
    fn a_signature_verifies_against_its_own_key() {
        let (public, private) = keys();
        let signed = sign_root("pudding-test", root(), "tester", 1, &public, &private).unwrap();
        assert_eq!(verify_signature(&signed, &public).unwrap(), root());
    }

    #[test]
    fn a_signature_from_another_key_is_rejected() {
        let (public, private) = keys();
        let (other_public, _) = keys();
        let signed = sign_root("pudding-test", root(), "tester", 1, &public, &private).unwrap();
        assert!(verify_signature(&signed, &other_public).is_err());
    }

    #[test]
    fn every_signed_field_is_covered_by_the_signature() {
        let (public, private) = keys();
        let signed = sign_root("pudding-test", root(), "tester", 1, &public, &private).unwrap();

        let mut tampered = signed.clone();
        tampered.root_object = CasHash::digest(b"a different manifest").to_hex();
        assert!(verify_signature(&tampered, &public).is_err());

        let mut tampered = signed.clone();
        tampered.archive_id = "pudding-other".into();
        assert!(verify_signature(&tampered, &public).is_err());

        let mut tampered = signed.clone();
        tampered.signer_identity = "impostor".into();
        assert!(verify_signature(&tampered, &public).is_err());

        let mut tampered = signed;
        tampered.signed_at_unix_secs = 2;
        assert!(verify_signature(&tampered, &public).is_err());
    }

    #[test]
    fn the_claim_is_domain_separated() {
        let message = claim_message("pudding-test", &root().to_hex(), "tester", 1).unwrap();
        assert!(message.starts_with(loadngo_anchor::FRAME_PREFIX));
        let domain_start = loadngo_anchor::FRAME_PREFIX.len() + 1;
        assert!(message[domain_start..].starts_with(ANCHOR_DOMAIN.as_bytes()));
    }
}
