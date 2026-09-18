//! Sign an Archive CAS root with a post-quantum key, or verify such a signature.
//!
//! Thin CLI over `data::archive_cas_sign`, which is also what
//! `archive_cas_browser` calls directly for its in-GUI "Sign" action -- there
//! is exactly one place that builds a signed claim, see that module for the
//! format and reasoning.

use anyhow::{anyhow, bail, Context, Result};
use data::archive_cas::ArchiveCasStorage;
use data::archive_cas_sign::{
    read_private_key, read_public_key, sign_manifest_and_write, verify_signature, SignedArchiveRoot,
};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

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

    let signed_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_secs();
    let (out, signed) = sign_manifest_and_write(
        &store,
        &manifest,
        signer_identity,
        &public_key,
        &private_key,
        signed_at,
    )?;

    println!("Signed archive root: {}", signed.root_object);
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
    let present = data::archive_cas_sign::present_root(&store, &manifest)?;
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
