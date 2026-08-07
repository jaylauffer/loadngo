use anyhow::{anyhow, bail, Context, Result};
use data::cas::{CasHash, CasStorage};
use data::pudding::{
    DigestAlgorithm, DigestRef, SignedRootManifest, WorkspaceManifest, ROOT_MANIFEST_FORMAT_V1,
    SIGNED_ROOT_MANIFEST_FORMAT_V1, WORKSPACE_MANIFEST_FORMAT_V1,
};
use qcoin_crypto::PublicKey;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

fn main() {
    if let Err(err) = run() {
        eprintln!("pudding_cas_verify: {err:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args = Args::parse()?;
    let report = verify_signed_root(&args)?;

    println!("Verified signed root: {}", args.signed_root.display());
    println!("Repository: {}", report.repository_id);
    println!("Signer: {}", report.signer_identity);
    println!("Signature scheme: {}", report.signature_scheme);
    println!("Root manifest digest: {}", report.root_manifest_digest);
    println!(
        "Workspace manifest hash: {}",
        report.workspace_manifest_hash
    );
    println!("Workspace files declared: {}", report.files_declared);
    println!("Workspace files verified: {}", report.files_verified);
    println!("Child repos declared: {}", report.child_repos_declared);
    if args.manifest_only {
        println!("Blob verification: skipped (--manifest-only)");
    } else {
        println!("Blob verification: complete");
    }

    Ok(())
}

#[derive(Debug, Clone)]
struct Args {
    signed_root: PathBuf,
    cas_root: PathBuf,
    expect_signer: Option<String>,
    trusted_public_key: Option<PathBuf>,
    manifest_only: bool,
}

impl Args {
    fn parse() -> Result<Self> {
        let mut args = std::env::args().skip(1);
        let mut signed_root = None;
        let mut cas_root = None;
        let mut expect_signer = None;
        let mut trusted_public_key = None;
        let mut manifest_only = false;

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--signed-root" => signed_root = args.next().map(PathBuf::from),
                "--cas-root" => cas_root = args.next().map(PathBuf::from),
                "--expect-signer" => expect_signer = args.next(),
                "--trusted-public-key" => trusted_public_key = args.next().map(PathBuf::from),
                "--manifest-only" => manifest_only = true,
                "--help" | "-h" => {
                    print_usage();
                    std::process::exit(0);
                }
                other => return Err(anyhow!("unknown argument: {other}")),
            }
        }

        Ok(Self {
            signed_root: signed_root.ok_or_else(|| anyhow!("missing --signed-root <path>"))?,
            cas_root: cas_root.ok_or_else(|| anyhow!("missing --cas-root <path>"))?,
            expect_signer,
            trusted_public_key,
            manifest_only,
        })
    }
}

fn print_usage() {
    eprintln!(
        "Usage: cargo run -p data --bin pudding_cas_verify -- --signed-root <pudding-root-signed.json> --cas-root <path> [--expect-signer <name>] [--trusted-public-key <path>] [--manifest-only]"
    );
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct VerificationReport {
    repository_id: String,
    signer_identity: String,
    signature_scheme: String,
    root_manifest_digest: DigestRef,
    workspace_manifest_hash: CasHash,
    files_declared: usize,
    files_verified: usize,
    child_repos_declared: usize,
}

fn verify_signed_root(args: &Args) -> Result<VerificationReport> {
    if !args.cas_root.exists() {
        bail!("CAS root does not exist: {}", args.cas_root.display());
    }

    let signed_root_bytes = fs::read(&args.signed_root)
        .with_context(|| format!("failed to read {}", args.signed_root.display()))?;
    let signed: SignedRootManifest = serde_json::from_slice(&signed_root_bytes)
        .with_context(|| format!("failed to parse {}", args.signed_root.display()))?;

    verify_signed_root_envelope(&signed)?;
    signed.verify().context("signed root verification failed")?;
    verify_expected_signer(&signed, args.expect_signer.as_deref())?;
    verify_trusted_public_key(&signed, args.trusted_public_key.as_deref())?;

    let store = CasStorage::new(&args.cas_root)?;
    let workspace_manifest_hash = digest_ref_to_cas_hash(&signed.manifest.content_root)
        .context("root manifest content_root is not a supported CAS reference")?;
    let workspace_manifest_bytes = store
        .verified_read_all(workspace_manifest_hash)
        .context("failed to verify workspace manifest CAS blob")?;
    let workspace_manifest_digest = DigestRef::blake3_256(&workspace_manifest_bytes);
    if workspace_manifest_digest != signed.manifest.content_root {
        bail!(
            "workspace manifest digest mismatch: root declares {}, computed {}",
            signed.manifest.content_root,
            workspace_manifest_digest
        );
    }

    let workspace_manifest: WorkspaceManifest =
        serde_json::from_slice(&workspace_manifest_bytes)
            .context("failed to parse workspace manifest from CAS")?;
    verify_workspace_manifest(&signed, &workspace_manifest)?;

    let files_verified = if args.manifest_only {
        0
    } else {
        verify_workspace_file_blobs(&store, &workspace_manifest)?
    };

    Ok(VerificationReport {
        repository_id: signed.manifest.repository_id.clone(),
        signer_identity: signed.signer_identity.clone(),
        signature_scheme: signed.signature_scheme.clone(),
        root_manifest_digest: signed.manifest_digest.clone(),
        workspace_manifest_hash,
        files_declared: workspace_manifest.files.len(),
        files_verified,
        child_repos_declared: signed.manifest.child_repos.len(),
    })
}

fn verify_signed_root_envelope(signed: &SignedRootManifest) -> Result<()> {
    if signed.format != SIGNED_ROOT_MANIFEST_FORMAT_V1 {
        bail!(
            "unsupported signed root format {:?}; expected {:?}",
            signed.format,
            SIGNED_ROOT_MANIFEST_FORMAT_V1
        );
    }
    if signed.manifest.format != ROOT_MANIFEST_FORMAT_V1 {
        bail!(
            "unsupported root manifest format {:?}; expected {:?}",
            signed.manifest.format,
            ROOT_MANIFEST_FORMAT_V1
        );
    }
    if signed.manifest.repository_id.trim().is_empty() {
        bail!("root manifest repository_id must not be empty");
    }
    if signed.manifest.content_root_kind != "workspace-manifest" {
        bail!(
            "unsupported root manifest content_root_kind {:?}; expected \"workspace-manifest\"",
            signed.manifest.content_root_kind
        );
    }
    Ok(())
}

fn verify_expected_signer(signed: &SignedRootManifest, expected: Option<&str>) -> Result<()> {
    if let Some(expected) = expected {
        if signed.signer_identity != expected {
            bail!(
                "signer mismatch: expected {:?}, found {:?}",
                expected,
                signed.signer_identity
            );
        }
    }
    Ok(())
}

fn verify_trusted_public_key(signed: &SignedRootManifest, path: Option<&Path>) -> Result<()> {
    let Some(path) = path else {
        return Ok(());
    };

    let trusted = read_public_key(path)?;
    let embedded = PublicKey::from_bytes(
        &hex::decode(signed.public_key_hex.trim()).context("invalid public_key_hex")?,
    )
    .context("invalid embedded public key")?;
    if embedded != trusted {
        bail!(
            "trusted public key mismatch: {} does not match signed root envelope",
            path.display()
        );
    }
    Ok(())
}

fn verify_workspace_manifest(
    signed: &SignedRootManifest,
    workspace_manifest: &WorkspaceManifest,
) -> Result<()> {
    if workspace_manifest.format != WORKSPACE_MANIFEST_FORMAT_V1 {
        bail!(
            "unsupported workspace manifest format {:?}; expected {:?}",
            workspace_manifest.format,
            WORKSPACE_MANIFEST_FORMAT_V1
        );
    }
    if workspace_manifest.created_at_unix_secs != signed.manifest.created_at_unix_secs {
        bail!(
            "workspace/root timestamp mismatch: workspace {}, root {}",
            workspace_manifest.created_at_unix_secs,
            signed.manifest.created_at_unix_secs
        );
    }
    if signed.manifest.child_repos.len() != workspace_manifest.repos.len() {
        bail!(
            "child repo count mismatch: root {}, workspace {}",
            signed.manifest.child_repos.len(),
            workspace_manifest.repos.len()
        );
    }
    Ok(())
}

fn verify_workspace_file_blobs(
    store: &CasStorage,
    workspace_manifest: &WorkspaceManifest,
) -> Result<usize> {
    for file in &workspace_manifest.files {
        let hash = CasHash::from_str(&file.hash)
            .with_context(|| format!("invalid CAS hash for {}", file.path))?;
        let bytes = store
            .verified_read_all(hash)
            .with_context(|| format!("failed to verify blob for {}", file.path))?;
        let size = u32::try_from(bytes.len())
            .with_context(|| format!("blob too large for {}", file.path))?;
        if size != file.size {
            bail!(
                "size mismatch for {}: manifest {}, CAS {}",
                file.path,
                file.size,
                size
            );
        }
        if u64::from(file.size) != file.captured.size_bytes
            || file.captured.size_bytes != file.verified.size_bytes
        {
            bail!(
                "snapshot size mismatch for {}: entry {}, captured {}, verified {}",
                file.path,
                file.size,
                file.captured.size_bytes,
                file.verified.size_bytes
            );
        }
    }
    Ok(workspace_manifest.files.len())
}

fn digest_ref_to_cas_hash(digest: &DigestRef) -> Result<CasHash> {
    if digest.algorithm != DigestAlgorithm::Blake3_256 {
        bail!(
            "unsupported CAS digest algorithm {}; current CAS storage is blake3-256",
            digest.algorithm
        );
    }
    CasHash::from_str(&digest.hex)
}

fn read_public_key(path: &Path) -> Result<PublicKey> {
    let text =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let bytes = hex::decode(text.trim())
        .with_context(|| format!("invalid public key hex in {}", path.display()))?;
    PublicKey::from_bytes(&bytes).with_context(|| format!("invalid public key {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use data::pudding::{
        FileSnapshotState, RootManifest, SignedRootManifest, WorkspaceFileManifestEntry,
        WorkspaceManifest, WorkspaceRepoState,
    };
    use qcoin_crypto::{default_registry, PqSchemeRegistry, SignatureSchemeId};

    #[test]
    fn verify_signed_root_checks_signature_manifest_and_blobs() {
        let fixture = Fixture::new();
        let args = Args {
            signed_root: fixture.signed_root_path.clone(),
            cas_root: fixture.cas_root.clone(),
            expect_signer: Some("smoke".to_string()),
            trusted_public_key: Some(fixture.public_key_path.clone()),
            manifest_only: false,
        };

        let report = verify_signed_root(&args).unwrap();
        assert_eq!(report.repository_id, "pudding");
        assert_eq!(report.signer_identity, "smoke");
        assert_eq!(report.files_declared, 1);
        assert_eq!(report.files_verified, 1);
        assert_eq!(report.child_repos_declared, 1);
    }

    #[test]
    fn verify_signed_root_manifest_only_skips_blob_walk() {
        let fixture = Fixture::new();
        let args = Args {
            signed_root: fixture.signed_root_path.clone(),
            cas_root: fixture.cas_root.clone(),
            expect_signer: None,
            trusted_public_key: None,
            manifest_only: true,
        };

        let report = verify_signed_root(&args).unwrap();
        assert_eq!(report.files_declared, 1);
        assert_eq!(report.files_verified, 0);
    }

    #[test]
    fn verify_signed_root_rejects_unexpected_signer() {
        let fixture = Fixture::new();
        let args = Args {
            signed_root: fixture.signed_root_path.clone(),
            cas_root: fixture.cas_root.clone(),
            expect_signer: Some("other".to_string()),
            trusted_public_key: None,
            manifest_only: true,
        };

        let err = verify_signed_root(&args).unwrap_err();
        assert!(format!("{err:#}").contains("signer mismatch"));
    }

    #[test]
    fn verify_signed_root_rejects_missing_workspace_manifest_blob() {
        let fixture = Fixture::new();
        let missing_cas = tempfile::tempdir().unwrap();
        let args = Args {
            signed_root: fixture.signed_root_path.clone(),
            cas_root: missing_cas.path().to_path_buf(),
            expect_signer: None,
            trusted_public_key: None,
            manifest_only: true,
        };

        let err = verify_signed_root(&args).unwrap_err();
        assert!(format!("{err:#}").contains("failed to verify workspace manifest CAS blob"));
    }

    struct Fixture {
        #[allow(dead_code)]
        tempdir: tempfile::TempDir,
        cas_root: PathBuf,
        signed_root_path: PathBuf,
        public_key_path: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let tempdir = tempfile::tempdir().unwrap();
            let cas_root = tempdir.path().join("cas");
            let manifest_dir = tempdir.path().join("manifests");
            fs::create_dir_all(&manifest_dir).unwrap();
            let store = CasStorage::new(&cas_root).unwrap();

            let payload = b"hello verified pudding";
            let (payload_hash, payload_inserted) = store.add_content(payload).unwrap();
            assert!(payload_inserted);

            let state = FileSnapshotState {
                size_bytes: payload.len() as u64,
                modified_unix_millis: Some(1),
            };
            let workspace_manifest = WorkspaceManifest::new(
                "/tmp/pudding",
                cas_root.display().to_string(),
                "/tmp/pudding/pudding.workspace.ron",
                1_700_000_000,
                vec![WorkspaceFileManifestEntry {
                    path: "child/file.txt".to_string(),
                    hash: payload_hash.to_hex(),
                    size: payload.len() as u32,
                    inserted: true,
                    captured: state.clone(),
                    verified: state,
                }],
                vec![WorkspaceRepoState {
                    name: "child".to_string(),
                    path: "child".to_string(),
                    branch: "dev".to_string(),
                    head: "abc123".to_string(),
                    status_short: Vec::new(),
                }],
            );
            let workspace_manifest_bytes = serde_json::to_vec_pretty(&workspace_manifest).unwrap();
            let (workspace_manifest_hash, _) =
                store.add_content(&workspace_manifest_bytes).unwrap();
            let root_manifest = RootManifest::from_workspace_manifest(
                "pudding",
                DigestRef::from_cas_hash(workspace_manifest_hash),
                &workspace_manifest,
                None,
                vec!["fixture".to_string()],
            );

            let registry = default_registry();
            let scheme = registry.get(&SignatureSchemeId::Dilithium2).unwrap();
            let (public_key, private_key) = scheme.keygen().unwrap();
            let signed =
                SignedRootManifest::sign(root_manifest, "smoke", &public_key, &private_key)
                    .unwrap();
            let signed_root_path = manifest_dir.join("pudding-root-signed.json");
            fs::write(
                &signed_root_path,
                serde_json::to_vec_pretty(&signed).unwrap(),
            )
            .unwrap();
            let public_key_path = tempdir.path().join("public.hex");
            fs::write(
                &public_key_path,
                format!("{}\n", hex::encode(public_key.to_bytes().unwrap())),
            )
            .unwrap();

            Self {
                tempdir,
                cas_root,
                signed_root_path,
                public_key_path,
            }
        }
    }
}
