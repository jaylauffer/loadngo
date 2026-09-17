//! Sign an Archive CAS root with a post-quantum key, or verify such a signature.
//!
//! An Archive CAS proves integrity: every object is named by its BLAKE3 digest,
//! so a root object pins every byte beneath it. It says nothing about who vouches
//! for that root. This binds a root to a named signer.
//!
//! The signature covers a `loadngo-anchor` frame (domain
//! `loadngo.archive-cas.root-signature`, version 1) around the JSON of the signed
//! claim: format, archive id, root object, signer and signing time. Every field a
//! verifier relies on is inside the signed bytes, and the domain stops the
//! signature being replayed as one over any other kind of record.
//!
//! A signature covers the root only. Pair it with `archive_cas_verify`, which
//! re-hashes every object beneath the root.

use anyhow::{anyhow, bail, Context, Result};
use data::archive_cas::{ArchiveCasStorage, ArchiveObject};
use data::cas::CasHash;
use loadngo_pq_crypto::{default_registry, PqSchemeRegistry, PrivateKey, PublicKey, Signature};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const SIGNED_ROOT_FORMAT_V1: &str = "loadngo-archive-root-signature-v1";
const ANCHOR_DOMAIN: &str = "loadngo.archive-cas.root-signature";
const ANCHOR_VERSION: u16 = 1;

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
struct SignedArchiveRoot {
    format: String,
    archive_id: String,
    root_object: String,
    signer_identity: String,
    signed_at_unix_secs: u64,
    signature_scheme: String,
    public_key_hex: String,
    signature_hex: String,
}

impl SignedArchiveRoot {
    fn signed_message(&self) -> Result<Vec<u8>> {
        claim_message(
            &self.archive_id,
            &self.root_object,
            &self.signer_identity,
            self.signed_at_unix_secs,
        )
    }
}

fn claim_message(
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

fn sign_root(
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
fn verify_signature(signed: &SignedArchiveRoot, trusted: &PublicKey) -> Result<CasHash> {
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

fn read_public_key(path: &Path) -> Result<PublicKey> {
    read_key(path, |b| PublicKey::from_bytes(b).map_err(Into::into))
}

fn read_private_key(path: &Path) -> Result<PrivateKey> {
    read_key(path, |b| PrivateKey::from_bytes(b).map_err(Into::into))
}

fn main() {
    if let Err(error) = run() {
        eprintln!("archive_cas_sign: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let command = args.next().unwrap_or_default();
    let flags: Vec<String> = args.collect();
    match command.as_str() {
        "sign" => command_sign(&flags),
        "verify" => command_verify(&flags),
        "--help" | "-h" | "help" => {
            print_usage();
            Ok(())
        }
        other => {
            print_usage();
            bail!("unknown command {other:?}")
        }
    }
}

fn command_sign(flags: &[String]) -> Result<()> {
    let cas_root = required(flags, "--cas-root")?;
    let manifest_path = required(flags, "--manifest")?;
    let signer_identity = flag(flags, "--signer-identity")
        .ok_or_else(|| anyhow!("missing --signer-identity <name>"))?;
    let public_key = read_public_key(&required(flags, "--public-key")?)?;
    let private_key = read_private_key(&required(flags, "--private-key")?)?;

    let store = ArchiveCasStorage::new(&cas_root)?;
    let manifest = store.read_manifest(&manifest_path)?;
    let root = present_root(&store, &manifest)?;

    let signed_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_secs();
    let signed = sign_root(
        &manifest.archive_id,
        root,
        signer_identity,
        signed_at,
        &public_key,
        &private_key,
    )?;
    // Never write a signature that does not verify.
    verify_signature(&signed, &public_key).context("freshly made signature failed to verify")?;

    let out = store.manifests_root().join(format!(
        "{}-{}.signature.json",
        manifest.archive_id,
        root.to_hex()
    ));
    if out.exists() {
        bail!("refusing to overwrite existing signature {}", out.display());
    }
    let json = serde_json::to_vec_pretty(&signed).context("failed to serialise signature")?;
    fs::write(&out, json).with_context(|| format!("failed to write {}", out.display()))?;

    println!("Signed archive root: {}", root.to_hex());
    println!("Archive id: {}", manifest.archive_id);
    println!("Signer: {signer_identity} ({})", signed.signature_scheme);
    println!("Signature: {}", out.display());
    Ok(())
}

fn command_verify(flags: &[String]) -> Result<()> {
    let cas_root = required(flags, "--cas-root")?;
    let signature_path = required(flags, "--signature")?;
    let trusted = read_public_key(&required(flags, "--trusted-public-key")?)?;

    let text = fs::read(&signature_path)
        .with_context(|| format!("failed to read {}", signature_path.display()))?;
    let signed: SignedArchiveRoot =
        serde_json::from_slice(&text).context("signature file is not a signed archive root")?;
    let root = verify_signature(&signed, &trusted)?;

    // The signed root must name a real, intact manifest object in this CAS.
    let store = ArchiveCasStorage::new(&cas_root)?;
    let manifest_path = store
        .manifests_root()
        .join(format!("{}-{}.json", signed.archive_id, signed.root_object));
    let manifest = store
        .read_manifest(&manifest_path)
        .with_context(|| format!("signed root has no manifest at {}", manifest_path.display()))?;
    if manifest.archive_id != signed.archive_id {
        bail!(
            "manifest archive id {:?} does not match signed id {:?}",
            manifest.archive_id,
            signed.archive_id
        );
    }
    let present = present_root(&store, &manifest)?;
    if present != root {
        bail!(
            "manifest digests to {}, but the signature covers {}",
            present.to_hex(),
            root.to_hex()
        );
    }

    println!("Verified signature over archive root: {}", root.to_hex());
    println!("Archive id: {}", signed.archive_id);
    println!(
        "Signer: {} ({}), signed at unix {}",
        signed.signer_identity, signed.signature_scheme, signed.signed_at_unix_secs
    );
    println!("Root manifest object: present and hash-verified");
    println!("Blob verification: not performed here; run archive_cas_verify");
    Ok(())
}

/// The manifest's root digest, after confirming the root object is stored and intact.
fn present_root(
    store: &ArchiveCasStorage,
    manifest: &data::archive_cas::ArchiveManifest,
) -> Result<CasHash> {
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

fn flag<'a>(flags: &'a [String], name: &str) -> Option<&'a str> {
    flags
        .windows(2)
        .find_map(|pair| (pair[0] == name).then_some(pair[1].as_str()))
}

fn required(flags: &[String], name: &str) -> Result<PathBuf> {
    flag(flags, name)
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("missing {name} <path>"))
}

fn print_usage() {
    eprintln!(
        "Usage:\n  archive_cas_sign sign --cas-root <dir> --manifest <manifest.json> --signer-identity <name> --public-key <hex-file> --private-key <hex-file>\n  archive_cas_sign verify --cas-root <dir> --signature <signature.json> --trusted-public-key <hex-file>"
    );
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
