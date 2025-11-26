use anyhow::{anyhow, bail, Context, Result};
use qcoin_crypto::{
    default_registry, PqSchemeRegistry, PrivateKey, PublicKey, Signature, SignatureSchemeId,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

pub const LOADNGO_PQ_AUTH_FORMAT: &str = "loadngo-pq-auth-v1";
pub const HASH_ALG_SHA256: &str = "sha256";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedAuthToken {
    pub format: String,
    pub issuer: String,
    pub audience: String,
    pub subject: Option<String>,
    pub scopes: Vec<String>,
    pub challenge_sha256: String,
    pub hash_alg: String,
    pub nonce_hex: String,
    pub issued_at_unix_s: u64,
    pub expires_at_unix_s: u64,
    pub signature_scheme: String,
    pub public_key_hex: String,
    pub signature_hex: String,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsignedAuthToken {
    pub issuer: String,
    pub audience: String,
    pub subject: Option<String>,
    pub scopes: Vec<String>,
    pub challenge_sha256: String,
    pub nonce_hex: String,
    pub issued_at_unix_s: u64,
    pub expires_at_unix_s: u64,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VerifyPolicy {
    pub now_unix_s: Option<u64>,
    pub expected_audience: Option<String>,
    pub expected_subject: Option<String>,
    pub required_scopes: Vec<String>,
    pub expected_challenge_sha256: Option<String>,
    pub trusted_public_key: Option<PublicKey>,
}

impl UnsignedAuthToken {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        issuer: impl Into<String>,
        audience: impl Into<String>,
        subject: Option<String>,
        scopes: Vec<String>,
        challenge_sha256: impl Into<String>,
        nonce_hex: impl Into<String>,
        issued_at_unix_s: u64,
        expires_at_unix_s: u64,
        notes: Option<String>,
    ) -> Result<Self> {
        let token = Self {
            issuer: issuer.into(),
            audience: audience.into(),
            subject: sanitize_optional_string(subject),
            scopes: normalize_scopes(scopes),
            challenge_sha256: challenge_sha256.into().trim().to_ascii_lowercase(),
            nonce_hex: nonce_hex.into().trim().to_ascii_lowercase(),
            issued_at_unix_s,
            expires_at_unix_s,
            notes: sanitize_optional_string(notes),
        };
        token.validate()?;
        Ok(token)
    }

    pub fn sign(self, public_key: &PublicKey, private_key: &PrivateKey) -> Result<SignedAuthToken> {
        self.validate()?;
        if public_key.scheme != private_key.scheme {
            bail!(
                "public/private key scheme mismatch: {} vs {}",
                public_key.scheme,
                private_key.scheme
            );
        }

        let registry = default_registry();
        let pq_scheme = registry
            .get(&private_key.scheme)
            .ok_or_else(|| anyhow!("signature scheme not registered: {}", private_key.scheme))?;

        let signing_message = signing_message(&self);
        let signature = pq_scheme
            .sign(private_key, &signing_message)
            .context("failed to sign PQ auth token")?;

        Ok(SignedAuthToken {
            format: LOADNGO_PQ_AUTH_FORMAT.to_string(),
            issuer: self.issuer,
            audience: self.audience,
            subject: self.subject,
            scopes: self.scopes,
            challenge_sha256: self.challenge_sha256,
            hash_alg: HASH_ALG_SHA256.to_string(),
            nonce_hex: self.nonce_hex,
            issued_at_unix_s: self.issued_at_unix_s,
            expires_at_unix_s: self.expires_at_unix_s,
            signature_scheme: public_key.scheme.to_string(),
            public_key_hex: hex::encode(public_key.to_bytes().context("encode public key")?),
            signature_hex: hex::encode(signature.to_bytes().context("encode signature")?),
            notes: self.notes,
        })
    }

    fn validate(&self) -> Result<()> {
        validate_unsigned_fields(
            &self.issuer,
            &self.audience,
            &self.scopes,
            &self.challenge_sha256,
            &self.nonce_hex,
            self.issued_at_unix_s,
            self.expires_at_unix_s,
        )
    }
}

impl SignedAuthToken {
    pub fn verify(&self) -> Result<()> {
        self.validate_envelope()?;
        let declared_scheme = parse_scheme(&self.signature_scheme)?;
        let public_key = decode_public_key(&self.public_key_hex)?;
        let signature = decode_signature(&self.signature_hex)?;
        if public_key.scheme != declared_scheme || signature.scheme != declared_scheme {
            bail!(
                "signature scheme mismatch in token: declared {}, public key {}, signature {}",
                declared_scheme,
                public_key.scheme,
                signature.scheme
            );
        }

        let registry = default_registry();
        let pq_scheme = registry
            .get(&declared_scheme)
            .ok_or_else(|| anyhow!("signature scheme not registered: {declared_scheme}"))?;
        let signing_message = signing_message(&UnsignedAuthToken {
            issuer: self.issuer.clone(),
            audience: self.audience.clone(),
            subject: self.subject.clone(),
            scopes: self.scopes.clone(),
            challenge_sha256: self.challenge_sha256.clone(),
            nonce_hex: self.nonce_hex.clone(),
            issued_at_unix_s: self.issued_at_unix_s,
            expires_at_unix_s: self.expires_at_unix_s,
            notes: self.notes.clone(),
        });
        pq_scheme
            .verify(&public_key, &signing_message, &signature)
            .context("post-quantum auth token verification failed")?;
        Ok(())
    }

    pub fn verify_with_policy(&self, policy: &VerifyPolicy) -> Result<()> {
        self.verify()?;
        if let Some(now_unix_s) = policy.now_unix_s {
            if now_unix_s < self.issued_at_unix_s {
                bail!(
                    "token not yet valid: issued_at={} now={}",
                    self.issued_at_unix_s,
                    now_unix_s
                );
            }
            if now_unix_s > self.expires_at_unix_s {
                bail!(
                    "token expired: expires_at={} now={}",
                    self.expires_at_unix_s,
                    now_unix_s
                );
            }
        }
        if let Some(expected_audience) = policy.expected_audience.as_deref() {
            if self.audience != expected_audience {
                bail!(
                    "token audience mismatch: expected {}, got {}",
                    expected_audience,
                    self.audience
                );
            }
        }
        if let Some(expected_subject) = policy.expected_subject.as_deref() {
            if self.subject.as_deref() != Some(expected_subject) {
                bail!(
                    "token subject mismatch: expected {}, got {}",
                    expected_subject,
                    self.subject.as_deref().unwrap_or("")
                );
            }
        }
        if let Some(expected_challenge_sha256) = policy.expected_challenge_sha256.as_deref() {
            if self.challenge_sha256 != expected_challenge_sha256.trim().to_ascii_lowercase() {
                bail!(
                    "challenge digest mismatch: expected {}, got {}",
                    expected_challenge_sha256,
                    self.challenge_sha256
                );
            }
        }
        let required_scopes = normalize_scopes(policy.required_scopes.clone());
        for scope in required_scopes {
            if !self.scopes.iter().any(|candidate| candidate == &scope) {
                bail!("required scope missing from token: {scope}");
            }
        }
        if let Some(trusted_public_key) = policy.trusted_public_key.as_ref() {
            let token_public_key = decode_public_key(&self.public_key_hex)?;
            if token_public_key != *trusted_public_key {
                bail!("token signer public key does not match trusted public key");
            }
        }
        Ok(())
    }

    fn validate_envelope(&self) -> Result<()> {
        if self.format != LOADNGO_PQ_AUTH_FORMAT {
            bail!(
                "unsupported auth token format: expected {LOADNGO_PQ_AUTH_FORMAT}, got {}",
                self.format
            );
        }
        if self.hash_alg != HASH_ALG_SHA256 {
            bail!(
                "unsupported hash algorithm: expected {HASH_ALG_SHA256}, got {}",
                self.hash_alg
            );
        }
        validate_unsigned_fields(
            &self.issuer,
            &self.audience,
            &self.scopes,
            &self.challenge_sha256,
            &self.nonce_hex,
            self.issued_at_unix_s,
            self.expires_at_unix_s,
        )
    }
}

pub fn current_unix_seconds() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock before unix epoch")?
        .as_secs())
}

pub fn random_nonce_hex() -> Result<String> {
    let mut bytes = [0_u8; 16];
    getrandom::getrandom(&mut bytes)
        .map_err(|err| anyhow!("failed to generate random nonce: {err}"))?;
    Ok(hex::encode(bytes))
}

pub fn sha256_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file =
        File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0_u8; 16 * 1024];

    loop {
        let read = file
            .read(&mut buf)
            .with_context(|| format!("failed to read {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }

    Ok(hex::encode(hasher.finalize()))
}

pub fn load_token(path: &Path) -> Result<SignedAuthToken> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read auth token {}", path.display()))?;
    ron::from_str(&text).with_context(|| format!("failed to parse auth token {}", path.display()))
}

pub fn save_token(path: &Path, token: &SignedAuthToken) -> Result<()> {
    let pretty = ron::ser::PrettyConfig::new()
        .depth_limit(3)
        .separate_tuple_members(true)
        .enumerate_arrays(true);
    let serialized =
        ron::ser::to_string_pretty(token, pretty).context("failed to serialize auth token")?;
    std::fs::write(path, serialized)
        .with_context(|| format!("failed to write auth token {}", path.display()))
}

pub fn parse_scheme(value: &str) -> Result<SignatureSchemeId> {
    match value.trim().to_ascii_lowercase().as_str() {
        "dilithium2" => Ok(SignatureSchemeId::Dilithium2),
        "falcon512" => Ok(SignatureSchemeId::Falcon512),
        other => bail!("unsupported signature scheme: {other}"),
    }
}

fn validate_unsigned_fields(
    issuer: &str,
    audience: &str,
    scopes: &[String],
    challenge_sha256: &str,
    nonce_hex: &str,
    issued_at_unix_s: u64,
    expires_at_unix_s: u64,
) -> Result<()> {
    if issuer.trim().is_empty() {
        bail!("issuer must not be empty");
    }
    if audience.trim().is_empty() {
        bail!("audience must not be empty");
    }
    if scopes.is_empty() {
        bail!("at least one scope is required");
    }
    validate_hex_length(challenge_sha256, 64, "challenge_sha256")?;
    validate_hex_length(nonce_hex, 32, "nonce_hex")?;
    if expires_at_unix_s <= issued_at_unix_s {
        bail!(
            "expires_at_unix_s must be greater than issued_at_unix_s ({} <= {})",
            expires_at_unix_s,
            issued_at_unix_s
        );
    }
    Ok(())
}

fn validate_hex_length(value: &str, expected_len: usize, field: &str) -> Result<()> {
    let trimmed = value.trim();
    if trimmed.len() != expected_len {
        bail!(
            "{field} must be {expected_len} hex chars, got {}",
            trimmed.len()
        );
    }
    hex::decode(trimmed).with_context(|| format!("{field} is not valid hex"))?;
    Ok(())
}

fn sanitize_optional_string(value: Option<String>) -> Option<String> {
    value.and_then(|text| {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn normalize_scopes(scopes: Vec<String>) -> Vec<String> {
    let mut scopes: Vec<String> = scopes
        .into_iter()
        .map(|scope| scope.trim().to_ascii_lowercase())
        .filter(|scope| !scope.is_empty())
        .collect();
    scopes.sort();
    scopes.dedup();
    scopes
}

fn decode_public_key(encoded_hex: &str) -> Result<PublicKey> {
    let bytes = hex::decode(encoded_hex.trim()).context("invalid public_key_hex")?;
    PublicKey::from_bytes(&bytes).context("invalid encoded public key")
}

fn decode_signature(encoded_hex: &str) -> Result<Signature> {
    let bytes = hex::decode(encoded_hex.trim()).context("invalid signature_hex")?;
    Signature::from_bytes(&bytes).context("invalid encoded signature")
}

fn signing_message(token: &UnsignedAuthToken) -> Vec<u8> {
    let subject = token.subject.as_deref().unwrap_or("");
    let notes = token.notes.as_deref().unwrap_or("");
    let scopes = token.scopes.join(",");
    format!(
        "format={LOADNGO_PQ_AUTH_FORMAT}\nissuer={}\naudience={}\nsubject={subject}\nscopes={scopes}\nhash_alg={HASH_ALG_SHA256}\nchallenge_sha256={}\nnonce_hex={}\nissued_at_unix_s={}\nexpires_at_unix_s={}\nnotes={notes}\n",
        token.issuer,
        token.audience,
        token.challenge_sha256,
        token.nonce_hex,
        token.issued_at_unix_s,
        token.expires_at_unix_s,
    )
    .into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_unsigned_token() -> UnsignedAuthToken {
        UnsignedAuthToken::new(
            "loadngo-operator",
            "field-node-a",
            Some("jay".to_string()),
            vec!["promote".to_string(), "import".to_string()],
            sha256_bytes(b"challenge"),
            "00112233445566778899aabbccddeeff".to_string(),
            1_700_000_000,
            1_700_000_300,
            Some("field drill".to_string()),
        )
        .expect("token should be valid")
    }

    #[test]
    fn signed_auth_token_round_trip_verifies() {
        let registry = default_registry();
        let scheme = registry
            .get(&SignatureSchemeId::Dilithium2)
            .expect("scheme should exist");
        let (public_key, private_key) = scheme.keygen().expect("keygen should work");

        let token = sample_unsigned_token()
            .sign(&public_key, &private_key)
            .expect("signing should work");

        token.verify().expect("verification should succeed");
    }

    #[test]
    fn verify_policy_rejects_expired_token() {
        let registry = default_registry();
        let scheme = registry
            .get(&SignatureSchemeId::Falcon512)
            .expect("scheme should exist");
        let (public_key, private_key) = scheme.keygen().expect("keygen should work");

        let token = sample_unsigned_token()
            .sign(&public_key, &private_key)
            .expect("signing should work");
        let policy = VerifyPolicy {
            now_unix_s: Some(1_700_000_999),
            ..VerifyPolicy::default()
        };

        assert!(token.verify_with_policy(&policy).is_err());
    }

    #[test]
    fn verify_policy_rejects_wrong_audience() {
        let registry = default_registry();
        let scheme = registry
            .get(&SignatureSchemeId::Dilithium2)
            .expect("scheme should exist");
        let (public_key, private_key) = scheme.keygen().expect("keygen should work");

        let token = sample_unsigned_token()
            .sign(&public_key, &private_key)
            .expect("signing should work");
        let policy = VerifyPolicy {
            expected_audience: Some("field-node-b".to_string()),
            ..VerifyPolicy::default()
        };

        assert!(token.verify_with_policy(&policy).is_err());
    }

    #[test]
    fn verify_policy_accepts_matching_scope_and_challenge() {
        let registry = default_registry();
        let scheme = registry
            .get(&SignatureSchemeId::Dilithium2)
            .expect("scheme should exist");
        let (public_key, private_key) = scheme.keygen().expect("keygen should work");

        let token = sample_unsigned_token()
            .sign(&public_key, &private_key)
            .expect("signing should work");
        let policy = VerifyPolicy {
            now_unix_s: Some(1_700_000_100),
            expected_audience: Some("field-node-a".to_string()),
            expected_subject: Some("jay".to_string()),
            required_scopes: vec!["import".to_string()],
            expected_challenge_sha256: Some(sha256_bytes(b"challenge")),
            trusted_public_key: Some(public_key),
        };

        token
            .verify_with_policy(&policy)
            .expect("policy should accept matching token");
    }
}
