use anyhow::{anyhow, bail, Context, Result};
use loadngo_pq_auth::{
    current_unix_seconds, load_token, parse_scheme, random_nonce_hex, save_token, sha256_file,
    UnsignedAuthToken, VerifyPolicy,
};
use qcoin_crypto::{default_registry, PqSchemeRegistry, PrivateKey, PublicKey};
use std::env;
use std::path::{Path, PathBuf};

fn main() {
    if let Err(err) = run() {
        eprintln!("loadngo_pq_auth: {err:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let mut args = env::args().skip(1);
    let Some(command) = args.next() else {
        print_usage();
        bail!("missing command");
    };

    match command.as_str() {
        "keygen" => command_keygen(args.collect()),
        "issue" => command_issue(args.collect()),
        "verify" => command_verify(args.collect()),
        "--help" | "-h" | "help" => {
            print_usage();
            Ok(())
        }
        other => bail!("unknown command: {other}"),
    }
}

fn command_keygen(args: Vec<String>) -> Result<()> {
    let quiet = has_flag(&args, "--quiet");
    let scheme = parse_flag_value(&args, "--scheme")
        .map(parse_scheme)
        .transpose()?
        .unwrap_or(qcoin_crypto::SignatureSchemeId::Dilithium2);
    let public_key_path = required_path(&args, "--public-key")?;
    let private_key_path = required_path(&args, "--private-key")?;

    let registry = default_registry();
    let pq_scheme = registry
        .get(&scheme)
        .ok_or_else(|| anyhow!("signature scheme not registered: {scheme}"))?;
    let (public_key, private_key) = pq_scheme.keygen().context("keygen failed")?;

    write_hex_file(
        &public_key_path,
        &public_key.to_bytes().context("encode public key")?,
    )?;
    write_hex_file(
        &private_key_path,
        &private_key.to_bytes().context("encode private key")?,
    )?;

    if quiet {
        println!(
            "keygen ok scheme={} public={} private={}",
            scheme,
            public_key_path.display(),
            private_key_path.display()
        );
    } else {
        println!(
            "Generated {} auth keypair:\n  public: {}\n  private: {}",
            scheme,
            public_key_path.display(),
            private_key_path.display()
        );
    }
    Ok(())
}

fn command_issue(args: Vec<String>) -> Result<()> {
    let quiet = has_flag(&args, "--quiet");
    let challenge_path = required_path(&args, "--challenge")?;
    let public_key_path = required_path(&args, "--public-key")?;
    let private_key_path = required_path(&args, "--private-key")?;
    let out_path = required_path(&args, "--out")?;
    let issuer = required_value(&args, "--issuer")?;
    let audience = required_value(&args, "--audience")?;
    let subject = parse_flag_value(&args, "--subject").map(ToOwned::to_owned);
    let scopes = repeated_flag_values(&args, "--scope");
    let ttl_seconds = parse_flag_value(&args, "--ttl-seconds")
        .map(|value| value.parse::<u64>().context("invalid --ttl-seconds"))
        .transpose()?
        .unwrap_or(300);
    let issued_at_unix_s = parse_flag_value(&args, "--issued-at")
        .map(|value| value.parse::<u64>().context("invalid --issued-at"))
        .transpose()?
        .unwrap_or(current_unix_seconds()?);
    let expires_at_unix_s = issued_at_unix_s
        .checked_add(ttl_seconds)
        .ok_or_else(|| anyhow!("issued-at + ttl overflow"))?;
    let nonce_hex = parse_flag_value(&args, "--nonce")
        .map(ToOwned::to_owned)
        .unwrap_or(random_nonce_hex()?);
    let notes = parse_flag_value(&args, "--notes").map(ToOwned::to_owned);

    let challenge_sha256 = sha256_file(&challenge_path)?;
    let public_key = read_public_key(&public_key_path)?;
    let private_key = read_private_key(&private_key_path)?;
    let token = UnsignedAuthToken::new(
        issuer,
        audience,
        subject,
        scopes,
        challenge_sha256,
        nonce_hex,
        issued_at_unix_s,
        expires_at_unix_s,
        notes,
    )?
    .sign(&public_key, &private_key)?;
    save_token(&out_path, &token)?;

    if quiet {
        println!(
            "issue ok token={} audience={} subject={} scopes={} expires_at_unix_s={} scheme={}",
            out_path.display(),
            token.audience,
            token.subject.as_deref().unwrap_or("<none>"),
            token.scopes.join(","),
            token.expires_at_unix_s,
            token.signature_scheme
        );
    } else {
        println!(
            "Issued PQ auth token:\n  token: {}\n  audience: {}\n  subject: {}\n  expires_at_unix_s: {}\n  scheme: {}",
            out_path.display(),
            token.audience,
            token.subject.as_deref().unwrap_or("<none>"),
            token.expires_at_unix_s,
            token.signature_scheme
        );
    }
    Ok(())
}

fn command_verify(args: Vec<String>) -> Result<()> {
    let quiet = has_flag(&args, "--quiet");
    let token_path = required_path(&args, "--token")?;
    let token = load_token(&token_path)?;
    let expected_challenge_sha256 =
        if let Some(challenge_path) = parse_flag_value(&args, "--challenge") {
            Some(sha256_file(Path::new(challenge_path))?)
        } else {
            None
        };
    let trusted_public_key = if let Some(path) = parse_flag_value(&args, "--trusted-public-key") {
        Some(read_public_key(Path::new(path))?)
    } else {
        None
    };
    let policy = VerifyPolicy {
        now_unix_s: parse_flag_value(&args, "--now")
            .map(|value| value.parse::<u64>().context("invalid --now"))
            .transpose()?,
        expected_audience: parse_flag_value(&args, "--audience").map(ToOwned::to_owned),
        expected_subject: parse_flag_value(&args, "--subject").map(ToOwned::to_owned),
        required_scopes: repeated_flag_values(&args, "--require-scope"),
        expected_challenge_sha256,
        trusted_public_key,
    };
    token.verify_with_policy(&policy)?;

    if quiet {
        println!(
            "verify ok token={} issuer={} audience={} subject={} scopes={} expires_at_unix_s={}",
            token_path.display(),
            token.issuer,
            token.audience,
            token.subject.as_deref().unwrap_or("<none>"),
            token.scopes.join(","),
            token.expires_at_unix_s
        );
    } else {
        println!(
            "Verified PQ auth token:\n  token: {}\n  issuer: {}\n  audience: {}\n  subject: {}\n  scopes: {}\n  expires_at_unix_s: {}",
            token_path.display(),
            token.issuer,
            token.audience,
            token.subject.as_deref().unwrap_or("<none>"),
            token.scopes.join(","),
            token.expires_at_unix_s
        );
    }
    Ok(())
}

fn read_public_key(path: &Path) -> Result<PublicKey> {
    let bytes = read_hex_file(path)?;
    PublicKey::from_bytes(&bytes).with_context(|| format!("invalid public key {}", path.display()))
}

fn read_private_key(path: &Path) -> Result<PrivateKey> {
    let bytes = read_hex_file(path)?;
    PrivateKey::from_bytes(&bytes)
        .with_context(|| format!("invalid private key {}", path.display()))
}

fn read_hex_file(path: &Path) -> Result<Vec<u8>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    hex::decode(text.trim()).with_context(|| format!("invalid hex in {}", path.display()))
}

fn write_hex_file(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    std::fs::write(path, format!("{}\n", hex::encode(bytes)))
        .with_context(|| format!("failed to write {}", path.display()))
}

fn parse_flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.windows(2)
        .find_map(|window| (window[0] == flag).then_some(window[1].as_str()))
}

fn repeated_flag_values(args: &[String], flag: &str) -> Vec<String> {
    args.windows(2)
        .filter_map(|window| (window[0] == flag).then_some(window[1].clone()))
        .collect()
}

fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|arg| arg == flag)
}

fn required_path(args: &[String], flag: &str) -> Result<PathBuf> {
    parse_flag_value(args, flag)
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("missing required flag: {flag}"))
}

fn required_value<'a>(args: &'a [String], flag: &str) -> Result<&'a str> {
    parse_flag_value(args, flag).ok_or_else(|| anyhow!("missing required flag: {flag}"))
}

fn print_usage() {
    eprintln!(
        "Usage:
  cargo run -p loadngo-pq-auth --bin loadngo_pq_auth -- keygen --scheme <dilithium2|falcon512> --public-key <path> --private-key <path> [--quiet]
  cargo run -p loadngo-pq-auth --bin loadngo_pq_auth -- issue --challenge <path> --issuer <name> --audience <name> --public-key <path> --private-key <path> --out <token.ron> [--subject <name>] [--scope <scope>]... [--ttl-seconds <seconds>] [--issued-at <unix-seconds>] [--nonce <32-hex>] [--notes <text>] [--quiet]
  cargo run -p loadngo-pq-auth --bin loadngo_pq_auth -- verify --token <token.ron> [--challenge <path>] [--audience <name>] [--subject <name>] [--require-scope <scope>]... [--trusted-public-key <path>] [--now <unix-seconds>] [--quiet]"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_flag_detects_quiet_without_treating_values_as_flags() {
        let args = vec![
            "--scope".to_string(),
            "netbsd-deploy".to_string(),
            "--quiet".to_string(),
        ];
        assert!(has_flag(&args, "--quiet"));
        assert!(!has_flag(&args, "--verbose"));
    }
}
