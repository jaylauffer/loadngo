use crate::cas::CasHash;
use anyhow::{anyhow, bail, Context, Result};
use qcoin_crypto::{
    default_registry, PqSchemeRegistry, PrivateKey, PublicKey, Signature, SignatureSchemeId,
};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

pub const WORKSPACE_CONFIG_FORMAT_V1: &str = "pudding-workspace-v1";
pub const WORKSPACE_MANIFEST_FORMAT_V1: &str = "pudding-workspace-cas-v1";
pub const ROOT_MANIFEST_FORMAT_V1: &str = "pudding-root-manifest-v1";
pub const SIGNED_ROOT_MANIFEST_FORMAT_V1: &str = "pudding-root-manifest-envelope-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DigestAlgorithm {
    Blake3_256,
    Sha3_256,
    Shake256_512,
}

impl DigestAlgorithm {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Blake3_256 => "blake3-256",
            Self::Sha3_256 => "sha3-256",
            Self::Shake256_512 => "shake256-512",
        }
    }

    pub fn expected_hex_len(self) -> usize {
        match self {
            Self::Blake3_256 | Self::Sha3_256 => 64,
            Self::Shake256_512 => 128,
        }
    }
}

impl fmt::Display for DigestAlgorithm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for DigestAlgorithm {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "blake3-256" => Ok(Self::Blake3_256),
            "sha3-256" => Ok(Self::Sha3_256),
            "shake256-512" => Ok(Self::Shake256_512),
            other => bail!("unsupported digest algorithm: {other}"),
        }
    }
}

impl Serialize for DigestAlgorithm {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for DigestAlgorithm {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::from_str(&value).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DigestRef {
    pub algorithm: DigestAlgorithm,
    pub hex: String,
}

impl DigestRef {
    pub fn new(algorithm: DigestAlgorithm, hex: impl Into<String>) -> Result<Self> {
        let hex = hex.into().trim().to_ascii_lowercase();
        let expected_len = algorithm.expected_hex_len();
        if hex.len() != expected_len {
            bail!(
                "digest length mismatch for {}: expected {} hex chars, got {}",
                algorithm,
                expected_len,
                hex.len()
            );
        }
        if !hex.as_bytes().iter().all(u8::is_ascii_hexdigit) {
            bail!("digest contains non-hex characters");
        }
        Ok(Self { algorithm, hex })
    }

    pub fn parse_tagged(value: &str) -> Result<Self> {
        let (algorithm, hex) = value
            .split_once(':')
            .ok_or_else(|| anyhow!("digest ref must be formatted as <algorithm>:<hex>"))?;
        Self::new(DigestAlgorithm::from_str(algorithm)?, hex)
    }

    pub fn tagged_string(&self) -> String {
        format!("{}:{}", self.algorithm, self.hex)
    }

    pub fn blake3_256(bytes: &[u8]) -> Self {
        Self {
            algorithm: DigestAlgorithm::Blake3_256,
            hex: hex::encode(blake3::hash(bytes).as_bytes()),
        }
    }

    pub fn from_cas_hash(hash: CasHash) -> Self {
        Self {
            algorithm: DigestAlgorithm::Blake3_256,
            hex: hash.to_hex(),
        }
    }
}

impl fmt::Display for DigestRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.tagged_string())
    }
}

impl FromStr for DigestRef {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        Self::parse_tagged(value)
    }
}

impl Serialize for DigestRef {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.tagged_string())
    }
}

impl<'de> Deserialize<'de> for DigestRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::from_str(&value).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct WorkspaceConfig {
    pub format: String,
    pub children: Vec<WorkspaceChildConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WorkspaceChildConfig {
    pub name: String,
    pub path: PathBuf,
    #[serde(default = "default_required_child")]
    pub required: bool,
    #[serde(default)]
    pub include: WorkspaceChildInclude,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub enum WorkspaceChildInclude {
    #[default]
    GitVisible,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileSnapshotState {
    pub size_bytes: u64,
    pub modified_unix_millis: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRepoState {
    pub name: String,
    pub path: String,
    pub branch: String,
    pub head: String,
    pub status_short: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceFileManifestEntry {
    pub path: String,
    pub hash: String,
    pub size: u32,
    pub inserted: bool,
    pub captured: FileSnapshotState,
    pub verified: FileSnapshotState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceManifest {
    pub format: String,
    pub workspace_root: String,
    pub cas_root: String,
    pub workspace_config: String,
    pub created_at_unix_secs: u64,
    pub files: Vec<WorkspaceFileManifestEntry>,
    pub repos: Vec<WorkspaceRepoState>,
}

impl WorkspaceManifest {
    pub fn new(
        workspace_root: impl Into<String>,
        cas_root: impl Into<String>,
        workspace_config: impl Into<String>,
        created_at_unix_secs: u64,
        files: Vec<WorkspaceFileManifestEntry>,
        repos: Vec<WorkspaceRepoState>,
    ) -> Self {
        Self {
            format: WORKSPACE_MANIFEST_FORMAT_V1.to_string(),
            workspace_root: workspace_root.into(),
            cas_root: cas_root.into(),
            workspace_config: workspace_config.into(),
            created_at_unix_secs,
            files,
            repos,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootChildRepoEntry {
    pub name: String,
    pub path: String,
    pub branch: Option<String>,
    pub local_head: Option<String>,
    pub file_manifest_root: Option<DigestRef>,
    pub local_divergence_summary: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootManifest {
    pub format: String,
    pub repository_id: String,
    pub created_at_unix_secs: u64,
    pub ancestor_manifest: Option<DigestRef>,
    pub content_root: DigestRef,
    pub content_root_kind: String,
    pub workspace_config_path: Option<String>,
    pub child_repos: Vec<RootChildRepoEntry>,
    pub notes: Vec<String>,
}

impl RootManifest {
    pub fn new(
        repository_id: impl Into<String>,
        created_at_unix_secs: u64,
        ancestor_manifest: Option<DigestRef>,
        content_root: DigestRef,
        content_root_kind: impl Into<String>,
        workspace_config_path: Option<String>,
        child_repos: Vec<RootChildRepoEntry>,
        notes: Vec<String>,
    ) -> Self {
        Self {
            format: ROOT_MANIFEST_FORMAT_V1.to_string(),
            repository_id: repository_id.into(),
            created_at_unix_secs,
            ancestor_manifest,
            content_root,
            content_root_kind: content_root_kind.into(),
            workspace_config_path,
            child_repos,
            notes,
        }
    }

    pub fn from_workspace_manifest(
        repository_id: impl Into<String>,
        workspace_manifest_ref: DigestRef,
        workspace_manifest: &WorkspaceManifest,
        ancestor_manifest: Option<DigestRef>,
        notes: Vec<String>,
    ) -> Self {
        let child_repos = workspace_manifest
            .repos
            .iter()
            .map(|repo| RootChildRepoEntry {
                name: repo.name.clone(),
                path: repo.path.clone(),
                branch: Some(repo.branch.clone()),
                local_head: Some(repo.head.clone()),
                file_manifest_root: None,
                local_divergence_summary: repo.status_short.clone(),
            })
            .collect();

        Self::new(
            repository_id,
            workspace_manifest.created_at_unix_secs,
            ancestor_manifest,
            workspace_manifest_ref,
            "workspace-manifest",
            Some(workspace_manifest.workspace_config.clone()),
            child_repos,
            notes,
        )
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        serde_json::to_vec(self).context("failed to serialize root manifest")
    }

    pub fn digest(&self) -> Result<DigestRef> {
        Ok(DigestRef::blake3_256(&self.canonical_bytes()?))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedRootManifest {
    pub format: String,
    pub manifest: RootManifest,
    pub manifest_digest: DigestRef,
    pub signer_identity: String,
    pub signature_scheme: String,
    pub public_key_hex: String,
    pub signature_hex: String,
}

impl SignedRootManifest {
    pub fn sign(
        manifest: RootManifest,
        signer_identity: impl Into<String>,
        public_key: &PublicKey,
        private_key: &PrivateKey,
    ) -> Result<Self> {
        let signer_identity = signer_identity.into();
        if signer_identity.trim().is_empty() {
            bail!("signer_identity must not be empty");
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

        let manifest_bytes = manifest.canonical_bytes()?;
        let manifest_digest = DigestRef::blake3_256(&manifest_bytes);
        let signature = scheme
            .sign(private_key, &manifest_bytes)
            .context("failed to sign root manifest")?;

        Ok(Self {
            format: SIGNED_ROOT_MANIFEST_FORMAT_V1.to_string(),
            manifest,
            manifest_digest,
            signer_identity,
            signature_scheme: public_key.scheme.to_string(),
            public_key_hex: hex::encode(public_key.to_bytes().context("encode public key")?),
            signature_hex: hex::encode(signature.to_bytes().context("encode signature")?),
        })
    }

    pub fn verify(&self) -> Result<()> {
        if self.signer_identity.trim().is_empty() {
            bail!("signer_identity must not be empty");
        }

        let manifest_bytes = self.manifest.canonical_bytes()?;
        let expected_digest = DigestRef::blake3_256(&manifest_bytes);
        if self.manifest_digest != expected_digest {
            bail!(
                "manifest digest mismatch: declared {}, computed {}",
                self.manifest_digest,
                expected_digest
            );
        }

        let declared_scheme = parse_signature_scheme(&self.signature_scheme)?;
        let public_key = PublicKey::from_bytes(
            &hex::decode(self.public_key_hex.trim()).context("invalid public_key_hex")?,
        )
        .context("invalid encoded public key")?;
        let signature = Signature::from_bytes(
            &hex::decode(self.signature_hex.trim()).context("invalid signature_hex")?,
        )
        .context("invalid encoded signature")?;

        if public_key.scheme != declared_scheme || signature.scheme != declared_scheme {
            bail!(
                "signature scheme mismatch in signed root manifest: declared {}, public key {}, signature {}",
                declared_scheme,
                public_key.scheme,
                signature.scheme
            );
        }

        let registry = default_registry();
        let scheme = registry
            .get(&declared_scheme)
            .ok_or_else(|| anyhow!("signature scheme not registered: {declared_scheme}"))?;
        scheme
            .verify(&public_key, &manifest_bytes, &signature)
            .context("root manifest signature verification failed")
    }
}

pub fn default_required_child() -> bool {
    true
}

fn parse_signature_scheme(value: &str) -> Result<SignatureSchemeId> {
    match value.trim() {
        "dilithium2" => Ok(SignatureSchemeId::Dilithium2),
        "falcon512" => Ok(SignatureSchemeId::Falcon512),
        other => bail!("unsupported signature scheme: {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_ref_round_trip_uses_tagged_strings() {
        let digest = DigestRef::new(
            DigestAlgorithm::Blake3_256,
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        )
        .unwrap();
        let encoded = digest.to_string();
        assert_eq!(
            encoded,
            "blake3-256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        );
        let decoded = DigestRef::from_str(&encoded).unwrap();
        assert_eq!(decoded, digest);
    }

    #[test]
    fn root_manifest_from_workspace_manifest_carries_repo_state() {
        let workspace_manifest = WorkspaceManifest::new(
            "/tmp/pudding",
            "/tmp/pudding/.loadngo-cas",
            "/tmp/pudding/pudding.workspace.ron",
            1_700_000_000,
            Vec::new(),
            vec![WorkspaceRepoState {
                name: "loadngo".to_string(),
                path: "loadngo".to_string(),
                branch: "dev".to_string(),
                head: "abc123".to_string(),
                status_short: vec![" M data/src/pudding.rs".to_string()],
            }],
        );
        let root_manifest = RootManifest::from_workspace_manifest(
            "pudding",
            DigestRef::new(
                DigestAlgorithm::Blake3_256,
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            )
            .unwrap(),
            &workspace_manifest,
            None,
            vec!["transitional root".to_string()],
        );

        assert_eq!(root_manifest.repository_id, "pudding");
        assert_eq!(root_manifest.content_root_kind, "workspace-manifest");
        assert_eq!(root_manifest.child_repos.len(), 1);
        assert_eq!(root_manifest.child_repos[0].name, "loadngo");
        assert_eq!(
            root_manifest.child_repos[0].local_divergence_summary,
            vec![" M data/src/pudding.rs".to_string()]
        );
    }

    #[test]
    fn signed_root_manifest_round_trip_verifies() {
        let registry = default_registry();
        let scheme = registry.get(&SignatureSchemeId::Dilithium2).unwrap();
        let (public_key, private_key) = scheme.keygen().unwrap();
        let manifest = RootManifest::new(
            "pudding",
            1_700_000_000,
            None,
            DigestRef::new(
                DigestAlgorithm::Blake3_256,
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            )
            .unwrap(),
            "workspace-manifest",
            Some("pudding.workspace.ron".to_string()),
            vec![RootChildRepoEntry {
                name: "loadngo".to_string(),
                path: "loadngo".to_string(),
                branch: Some("dev".to_string()),
                local_head: Some("abc123".to_string()),
                file_manifest_root: None,
                local_divergence_summary: vec![],
            }],
            vec!["signed root manifest test".to_string()],
        );

        let signed = SignedRootManifest::sign(manifest, "jay", &public_key, &private_key).unwrap();
        signed.verify().unwrap();
    }

    #[test]
    fn signed_root_manifest_rejects_digest_tamper() {
        let registry = default_registry();
        let scheme = registry.get(&SignatureSchemeId::Falcon512).unwrap();
        let (public_key, private_key) = scheme.keygen().unwrap();
        let manifest = RootManifest::new(
            "pudding",
            1_700_000_000,
            None,
            DigestRef::new(
                DigestAlgorithm::Blake3_256,
                "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
            )
            .unwrap(),
            "workspace-manifest",
            None,
            Vec::new(),
            Vec::new(),
        );

        let mut signed =
            SignedRootManifest::sign(manifest, "arraya", &public_key, &private_key).unwrap();
        signed.manifest_digest = DigestRef::new(
            DigestAlgorithm::Blake3_256,
            "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
        )
        .unwrap();
        let err = signed.verify().unwrap_err();
        assert!(format!("{err:#}").contains("manifest digest mismatch"));
    }
}
