use anyhow::{anyhow, bail, Context, Result};
use data::cas::CasStorage;
use data::pudding::{
    DigestRef, FileSnapshotState, RootManifest, SignedRootManifest, WorkspaceChildInclude,
    WorkspaceConfig, WorkspaceFileManifestEntry, WorkspaceManifest, WorkspaceRepoState,
    WORKSPACE_CONFIG_FORMAT_V1,
};
use qcoin_crypto::{PrivateKey, PublicKey};
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const DEFAULT_CAS_DIR: &str = ".loadngo-cas";
const DEFAULT_MANIFEST_DIR: &str = "build_tmp/pudding-cas-manifests";
const DEFAULT_WORKSPACE_CONFIG: &str = "pudding.workspace.ron";
const ROOT_TEXT_EXTENSIONS: &[&str] = &["md", "sh", "txt", "ron", "json", "toml"];

#[derive(Debug, Clone)]
struct CapturedWorkspaceFile {
    absolute_path: PathBuf,
    relative_path: String,
    captured: FileSnapshotState,
}

fn main() {
    if let Err(err) = run() {
        eprintln!("pudding_cas_ingest: {err:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args = Args::parse()?;
    let store = CasStorage::new(&args.cas_root)?;
    let config = load_workspace_config(&args.workspace_config)?;
    validate_workspace_config(&args.workspace_root, &config)?;
    let initial_repo_state = collect_repo_state(&args.workspace_root, &config)?;
    let files = capture_workspace_files(&args.workspace_root, &config)?;

    let mut manifest_files = Vec::with_capacity(files.len());
    for file in files {
        let (hash, size, inserted) = store
            .add_file(&file.absolute_path)
            .with_context(|| format!("failed to ingest {}", file.absolute_path.display()))?;
        let verified = stat_file(&file.absolute_path)?;
        ensure_file_state_unchanged(&file.relative_path, &file.captured, &verified)?;
        manifest_files.push(WorkspaceFileManifestEntry {
            path: file.relative_path,
            hash: hash.to_hex(),
            size,
            inserted,
            captured: file.captured,
            verified,
        });
    }

    let final_repo_state = collect_repo_state(&args.workspace_root, &config)?;
    ensure_repo_state_unchanged(&initial_repo_state, &final_repo_state)?;

    let created_at_unix_secs = unix_now()?;
    let manifest = WorkspaceManifest::new(
        args.workspace_root.display().to_string(),
        args.cas_root.display().to_string(),
        args.workspace_config.display().to_string(),
        created_at_unix_secs,
        manifest_files,
        initial_repo_state,
    );

    fs::create_dir_all(&args.manifest_dir)
        .with_context(|| format!("failed to create {}", args.manifest_dir.display()))?;
    let manifest_name = format!("pudding-{}.json", unix_now()?);
    let manifest_path = args.manifest_dir.join(manifest_name);
    let manifest_bytes =
        serde_json::to_vec_pretty(&manifest).context("failed to serialize workspace manifest")?;
    fs::write(&manifest_path, &manifest_bytes)
        .with_context(|| format!("failed to write {}", manifest_path.display()))?;
    let (manifest_hash, manifest_inserted) = store
        .add_content(&manifest_bytes)
        .context("failed to add manifest to CAS")?;
    let root_manifest = RootManifest::from_workspace_manifest(
        "pudding",
        DigestRef::from_cas_hash(manifest_hash),
        &manifest,
        None,
        vec![
            "transitional root manifest generated from workspace CAS manifest".to_string(),
            "child file_manifest_root fields are not populated yet".to_string(),
        ],
    );
    let root_manifest_name = format!("pudding-root-{}.json", created_at_unix_secs);
    let root_manifest_path = args.manifest_dir.join(root_manifest_name);
    let root_manifest_bytes =
        serde_json::to_vec_pretty(&root_manifest).context("failed to serialize root manifest")?;
    fs::write(&root_manifest_path, &root_manifest_bytes)
        .with_context(|| format!("failed to write {}", root_manifest_path.display()))?;
    let (root_manifest_hash, root_manifest_inserted) = store
        .add_content(&root_manifest_bytes)
        .context("failed to add root manifest to CAS")?;
    let signed_root_result = if let Some(signing) = args.signing.as_ref() {
        let public_key = read_public_key(&signing.public_key)?;
        let private_key = read_private_key(&signing.private_key)?;
        let signed_root_manifest = SignedRootManifest::sign(
            root_manifest.clone(),
            signing.signer_identity.clone(),
            &public_key,
            &private_key,
        )
        .context("failed to sign root manifest")?;
        signed_root_manifest
            .verify()
            .context("signed root manifest self-verification failed")?;
        let signed_root_manifest_name =
            format!("pudding-root-signed-{}.json", created_at_unix_secs);
        let signed_root_manifest_path = args.manifest_dir.join(signed_root_manifest_name);
        let signed_root_manifest_bytes = serde_json::to_vec_pretty(&signed_root_manifest)
            .context("failed to serialize signed root manifest")?;
        fs::write(&signed_root_manifest_path, &signed_root_manifest_bytes)
            .with_context(|| format!("failed to write {}", signed_root_manifest_path.display()))?;
        let (signed_root_manifest_hash, signed_root_manifest_inserted) = store
            .add_content(&signed_root_manifest_bytes)
            .context("failed to add signed root manifest to CAS")?;
        Some((
            signed_root_manifest_path,
            signed_root_manifest_hash,
            signed_root_manifest_inserted,
        ))
    } else {
        None
    };

    println!("CAS root: {}", args.cas_root.display());
    println!("Workspace root: {}", args.workspace_root.display());
    println!("Workspace config: {}", args.workspace_config.display());
    println!("Manifest: {}", manifest_path.display());
    println!("Manifest hash: {}", manifest_hash);
    println!("Manifest inserted: {}", manifest_inserted);
    println!("Root manifest: {}", root_manifest_path.display());
    println!("Root manifest hash: {}", root_manifest_hash);
    println!("Root manifest inserted: {}", root_manifest_inserted);
    if let Some((
        signed_root_manifest_path,
        signed_root_manifest_hash,
        signed_root_manifest_inserted,
    )) = signed_root_result
    {
        println!(
            "Signed root manifest: {}",
            signed_root_manifest_path.display()
        );
        println!("Signed root manifest hash: {}", signed_root_manifest_hash);
        println!(
            "Signed root manifest inserted: {}",
            signed_root_manifest_inserted
        );
    }
    println!("Files ingested: {}", manifest.files.len());
    Ok(())
}

struct Args {
    workspace_root: PathBuf,
    cas_root: PathBuf,
    manifest_dir: PathBuf,
    workspace_config: PathBuf,
    signing: Option<SigningArgs>,
}

#[derive(Debug)]
struct SigningArgs {
    signer_identity: String,
    public_key: PathBuf,
    private_key: PathBuf,
}

impl Args {
    fn parse() -> Result<Self> {
        let mut args = std::env::args().skip(1);
        let mut workspace_root = None;
        let mut cas_root = None;
        let mut manifest_dir = None;
        let mut workspace_config = None;
        let mut signer_identity = None;
        let mut public_key = None;
        let mut private_key = None;

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--workspace-root" => workspace_root = args.next().map(PathBuf::from),
                "--cas-root" => cas_root = args.next().map(PathBuf::from),
                "--manifest-dir" => manifest_dir = args.next().map(PathBuf::from),
                "--workspace-config" => workspace_config = args.next().map(PathBuf::from),
                "--signer-identity" => signer_identity = args.next(),
                "--public-key" => public_key = args.next().map(PathBuf::from),
                "--private-key" => private_key = args.next().map(PathBuf::from),
                "--help" | "-h" => {
                    print_usage();
                    std::process::exit(0);
                }
                other => return Err(anyhow!("unknown argument: {other}")),
            }
        }

        let cwd = std::env::current_dir().context("failed to determine current directory")?;
        let workspace_root =
            resolve_workspace_root(workspace_root, workspace_config.as_deref(), &cwd);
        let cas_root = cas_root.unwrap_or_else(|| workspace_root.join(DEFAULT_CAS_DIR));
        let manifest_dir =
            manifest_dir.unwrap_or_else(|| workspace_root.join(DEFAULT_MANIFEST_DIR));
        let workspace_config = resolve_workspace_config_path(workspace_config, &workspace_root);
        let signing = resolve_signing_args(signer_identity, public_key, private_key)?;

        Ok(Self {
            workspace_root,
            cas_root,
            manifest_dir,
            workspace_config,
            signing,
        })
    }
}

fn print_usage() {
    eprintln!(
        "Usage: cargo run -p data --bin pudding_cas_ingest -- [--workspace-root <path>] [--cas-root <path>] [--manifest-dir <path>] [--workspace-config <path>] [--signer-identity <name> --public-key <path> --private-key <path>]"
    );
}

fn resolve_workspace_root(
    workspace_root: Option<PathBuf>,
    workspace_config: Option<&Path>,
    cwd: &Path,
) -> PathBuf {
    if let Some(workspace_root) = workspace_root {
        return workspace_root;
    }

    if let Some(workspace_config) = workspace_config {
        let config_path = if workspace_config.is_absolute() {
            workspace_config.to_path_buf()
        } else {
            cwd.join(workspace_config)
        };
        if let Some(parent) = config_path.parent() {
            return parent.to_path_buf();
        }
    }

    for ancestor in cwd.ancestors() {
        if ancestor.join(DEFAULT_WORKSPACE_CONFIG).is_file() {
            return ancestor.to_path_buf();
        }
    }

    cwd.to_path_buf()
}

fn resolve_workspace_config_path(
    workspace_config: Option<PathBuf>,
    workspace_root: &Path,
) -> PathBuf {
    match workspace_config {
        Some(path) if path.is_absolute() => path,
        Some(path) => workspace_root.join(path),
        None => workspace_root.join(DEFAULT_WORKSPACE_CONFIG),
    }
}

fn resolve_signing_args(
    signer_identity: Option<String>,
    public_key: Option<PathBuf>,
    private_key: Option<PathBuf>,
) -> Result<Option<SigningArgs>> {
    match (signer_identity, public_key, private_key) {
        (None, None, None) => Ok(None),
        (Some(signer_identity), Some(public_key), Some(private_key)) => {
            if signer_identity.trim().is_empty() {
                bail!("--signer-identity must not be empty");
            }
            Ok(Some(SigningArgs {
                signer_identity,
                public_key,
                private_key,
            }))
        }
        _ => bail!(
            "root manifest signing requires --signer-identity, --public-key, and --private-key together"
        ),
    }
}

fn load_workspace_config(path: &Path) -> Result<WorkspaceConfig> {
    let text =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let config: WorkspaceConfig =
        ron::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(config)
}

fn validate_workspace_config(workspace_root: &Path, config: &WorkspaceConfig) -> Result<()> {
    if config.format != WORKSPACE_CONFIG_FORMAT_V1 {
        bail!(
            "unsupported workspace config format {:?}; expected \"pudding-workspace-v1\"",
            config.format
        );
    }
    if config.children.is_empty() {
        bail!("workspace config must declare at least one child repo");
    }
    for child in &config.children {
        let child_path = workspace_root.join(&child.path);
        if child.required && !child_path.exists() {
            bail!(
                "required child repo {} missing at {}",
                child.name,
                child_path.display()
            );
        }
    }
    Ok(())
}

fn capture_workspace_files(
    workspace_root: &Path,
    config: &WorkspaceConfig,
) -> Result<Vec<CapturedWorkspaceFile>> {
    let files = collect_workspace_files(workspace_root, config)?;
    files
        .into_iter()
        .map(|absolute_path| {
            let relative_path = absolute_path
                .strip_prefix(workspace_root)
                .with_context(|| {
                    format!("path {} not under workspace root", absolute_path.display())
                })?
                .to_string_lossy()
                .to_string();
            let captured = stat_file(&absolute_path)?;
            Ok(CapturedWorkspaceFile {
                absolute_path,
                relative_path,
                captured,
            })
        })
        .collect()
}

fn collect_workspace_files(
    workspace_root: &Path,
    config: &WorkspaceConfig,
) -> Result<Vec<PathBuf>> {
    let mut files = BTreeSet::new();

    for entry in fs::read_dir(workspace_root)
        .with_context(|| format!("failed to read {}", workspace_root.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') {
            continue;
        }
        let ext = path
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or_default();
        if ROOT_TEXT_EXTENSIONS.contains(&ext) {
            files.insert(path);
        }
    }

    for child in &config.children {
        let child_root = workspace_root.join(&child.path);
        if !child_root.exists() {
            if child.required {
                bail!(
                    "required child repo {} missing at {}",
                    child.name,
                    child_root.display()
                );
            }
            continue;
        }
        let child_files = match child.include {
            WorkspaceChildInclude::GitVisible => git_visible_files(&child_root)?,
        };
        for relative in child_files {
            files.insert(child_root.join(relative));
        }
    }

    Ok(files.into_iter().collect())
}

fn git_visible_files(repo_root: &Path) -> Result<Vec<PathBuf>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["ls-files", "-z", "-c", "-m", "-o", "--exclude-standard"])
        .output()
        .with_context(|| format!("failed to run git in {}", repo_root.display()))?;
    if !output.status.success() {
        return Err(anyhow!(
            "git ls-files failed in {}: {}",
            repo_root.display(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let mut files = Vec::new();
    for relative in parse_git_ls_files_output(&output.stdout) {
        if relative.as_os_str().is_empty() {
            continue;
        }
        let path = repo_root.join(&relative);
        if path.is_file() {
            files.push(relative);
        }
    }
    Ok(files)
}

fn parse_git_ls_files_output(bytes: &[u8]) -> Vec<PathBuf> {
    bytes
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .map(pathbuf_from_git_bytes)
        .collect()
}

#[cfg(unix)]
fn pathbuf_from_git_bytes(bytes: &[u8]) -> PathBuf {
    PathBuf::from(OsString::from_vec(bytes.to_vec()))
}

#[cfg(not(unix))]
fn pathbuf_from_git_bytes(bytes: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
}

fn collect_repo_state(
    workspace_root: &Path,
    config: &WorkspaceConfig,
) -> Result<Vec<WorkspaceRepoState>> {
    let mut states = Vec::new();
    for child in &config.children {
        let repo_root = workspace_root.join(&child.path);
        if !repo_root.exists() {
            if child.required {
                bail!(
                    "required child repo {} missing at {}",
                    child.name,
                    repo_root.display()
                );
            }
            continue;
        }
        let branch = git_stdout(&repo_root, &["branch", "--show-current"])?;
        let head = git_stdout(&repo_root, &["rev-parse", "HEAD"])?;
        let status = git_stdout(&repo_root, &["status", "--short"])?;
        states.push(WorkspaceRepoState {
            name: child.name.clone(),
            path: child.path.display().to_string(),
            branch: branch.trim().to_string(),
            head: head.trim().to_string(),
            status_short: status.lines().map(|line| line.to_string()).collect(),
        });
    }
    Ok(states)
}

fn stat_file(path: &Path) -> Result<FileSnapshotState> {
    let metadata =
        fs::metadata(path).with_context(|| format!("failed to stat {}", path.display()))?;
    let modified_unix_millis = metadata
        .modified()
        .ok()
        .and_then(system_time_to_unix_millis);
    Ok(FileSnapshotState {
        size_bytes: metadata.len(),
        modified_unix_millis,
    })
}

fn system_time_to_unix_millis(time: SystemTime) -> Option<u64> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
}

fn ensure_repo_state_unchanged(
    initial: &[WorkspaceRepoState],
    final_state: &[WorkspaceRepoState],
) -> Result<()> {
    if initial == final_state {
        return Ok(());
    }

    let mut changed = Vec::new();
    let count = initial.len().max(final_state.len());
    for index in 0..count {
        let before = initial.get(index);
        let after = final_state.get(index);
        if before == after {
            continue;
        }
        match (before, after) {
            (Some(before), Some(after)) if before.name == after.name => {
                changed.push(format!(
                    "{} changed during ingest (before: branch={} head={} status={:?}; after: branch={} head={} status={:?})",
                    before.name,
                    before.branch,
                    before.head,
                    before.status_short,
                    after.branch,
                    after.head,
                    after.status_short
                ));
            }
            (Some(before), Some(after)) => {
                changed.push(format!(
                    "repo ordering changed during ingest (before: {}, after: {})",
                    before.name, after.name
                ));
            }
            (Some(before), None) => {
                changed.push(format!("repo disappeared during ingest: {}", before.name));
            }
            (None, Some(after)) => {
                changed.push(format!("repo appeared during ingest: {}", after.name));
            }
            (None, None) => {}
        }
    }

    bail!(
        "workspace changed during ingest; rerun to capture a coherent snapshot:\n{}",
        changed.join("\n")
    );
}

fn ensure_file_state_unchanged(
    relative_path: &str,
    captured: &FileSnapshotState,
    verified: &FileSnapshotState,
) -> Result<()> {
    if captured == verified {
        return Ok(());
    }

    bail!(
        "file changed during ingest: {} (captured: size={} modified={:?}; verified: size={} modified={:?})",
        relative_path,
        captured.size_bytes,
        captured.modified_unix_millis,
        verified.size_bytes,
        verified.modified_unix_millis
    );
}

fn git_stdout(repo_root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .output()
        .with_context(|| format!("failed to run git {:?} in {}", args, repo_root.display()))?;
    if !output.status.success() {
        return Err(anyhow!(
            "git {:?} failed in {}: {}",
            args,
            repo_root.display(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
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
    let text =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    hex::decode(text.trim()).with_context(|| format!("invalid hex in {}", path.display()))
}

fn unix_now() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock before unix epoch")?
        .as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use qcoin_crypto::{default_registry, PqSchemeRegistry, SignatureSchemeId};

    #[test]
    fn parse_git_ls_files_output_uses_nul_delimiters() {
        let files = parse_git_ls_files_output(b"alpha.txt\0dir/line\nbreak.ron\0");
        assert_eq!(
            files,
            vec![
                PathBuf::from("alpha.txt"),
                PathBuf::from("dir/line\nbreak.ron")
            ]
        );
    }

    #[test]
    fn resolve_workspace_root_prefers_ancestor_config() {
        let root = tempfile::tempdir().unwrap();
        let nested = root.path().join("loadngo/data");
        fs::create_dir_all(&nested).unwrap();
        fs::write(
            root.path().join(DEFAULT_WORKSPACE_CONFIG),
            "(format:\"pudding-workspace-v1\",children:[])",
        )
        .unwrap();

        let resolved = resolve_workspace_root(None, None, &nested);
        assert_eq!(resolved, root.path());
    }

    #[test]
    fn resolve_workspace_root_uses_explicit_config_parent() {
        let cwd = Path::new("/tmp/current");
        let resolved = resolve_workspace_root(
            None,
            Some(Path::new("/var/pudding/pudding.workspace.ron")),
            cwd,
        );
        assert_eq!(resolved, Path::new("/var/pudding"));
    }

    #[test]
    fn resolve_signing_args_requires_complete_set() {
        let err = resolve_signing_args(
            Some("jay".to_string()),
            Some(PathBuf::from("public.hex")),
            None,
        )
        .unwrap_err();
        assert!(format!("{err:#}")
            .contains("requires --signer-identity, --public-key, and --private-key together"));
    }

    #[test]
    fn resolve_signing_args_accepts_complete_set() {
        let signing = resolve_signing_args(
            Some("jay".to_string()),
            Some(PathBuf::from("public.hex")),
            Some(PathBuf::from("private.hex")),
        )
        .unwrap()
        .unwrap();
        assert_eq!(signing.signer_identity, "jay");
        assert_eq!(signing.public_key, PathBuf::from("public.hex"));
        assert_eq!(signing.private_key, PathBuf::from("private.hex"));
    }

    #[test]
    fn read_key_files_accept_hex_encoded_qcoin_keys() {
        let registry = default_registry();
        let scheme = registry.get(&SignatureSchemeId::Dilithium2).unwrap();
        let (public_key, private_key) = scheme.keygen().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let public_key_path = dir.path().join("public.hex");
        let private_key_path = dir.path().join("private.hex");

        fs::write(
            &public_key_path,
            format!("{}\n", hex::encode(public_key.to_bytes().unwrap())),
        )
        .unwrap();
        fs::write(
            &private_key_path,
            format!("{}\n", hex::encode(private_key.to_bytes().unwrap())),
        )
        .unwrap();

        assert_eq!(read_public_key(&public_key_path).unwrap(), public_key);
        assert_eq!(read_private_key(&private_key_path).unwrap(), private_key);
    }

    #[test]
    fn ensure_repo_state_unchanged_accepts_identical_state() {
        let state = vec![WorkspaceRepoState {
            name: "loadngo".to_string(),
            path: "loadngo".to_string(),
            branch: "main".to_string(),
            head: "abc123".to_string(),
            status_short: vec![" M data/src/bin/pudding_cas_ingest.rs".to_string()],
        }];
        assert!(ensure_repo_state_unchanged(&state, &state).is_ok());
    }

    #[test]
    fn ensure_repo_state_unchanged_rejects_drift() {
        let before = vec![WorkspaceRepoState {
            name: "loadngo".to_string(),
            path: "loadngo".to_string(),
            branch: "main".to_string(),
            head: "abc123".to_string(),
            status_short: vec![],
        }];
        let after = vec![WorkspaceRepoState {
            name: "loadngo".to_string(),
            path: "loadngo".to_string(),
            branch: "main".to_string(),
            head: "def456".to_string(),
            status_short: vec![" M docs/PUDDING_CAS_PQ_MODEL.md".to_string()],
        }];
        let err = ensure_repo_state_unchanged(&before, &after).unwrap_err();
        let message = format!("{err:#}");
        assert!(message.contains("workspace changed during ingest"));
        assert!(message.contains("loadngo changed during ingest"));
    }

    #[test]
    fn validate_workspace_config_requires_children() {
        let config = WorkspaceConfig {
            format: WORKSPACE_CONFIG_FORMAT_V1.to_string(),
            children: Vec::new(),
        };
        let err = validate_workspace_config(Path::new("/tmp"), &config).unwrap_err();
        assert!(format!("{err:#}").contains("must declare at least one child repo"));
    }

    #[test]
    fn validate_workspace_config_rejects_missing_required_child() {
        let root = tempfile::tempdir().unwrap();
        let config = WorkspaceConfig {
            format: WORKSPACE_CONFIG_FORMAT_V1.to_string(),
            children: vec![data::pudding::WorkspaceChildConfig {
                name: "loadngo".to_string(),
                path: PathBuf::from("loadngo"),
                required: true,
                include: WorkspaceChildInclude::GitVisible,
            }],
        };
        let err = validate_workspace_config(root.path(), &config).unwrap_err();
        assert!(format!("{err:#}").contains("required child repo loadngo missing"));
    }

    #[test]
    fn validate_workspace_config_allows_missing_optional_child() {
        let root = tempfile::tempdir().unwrap();
        let config = WorkspaceConfig {
            format: WORKSPACE_CONFIG_FORMAT_V1.to_string(),
            children: vec![data::pudding::WorkspaceChildConfig {
                name: "legacy".to_string(),
                path: PathBuf::from("legacy"),
                required: false,
                include: WorkspaceChildInclude::GitVisible,
            }],
        };
        assert!(validate_workspace_config(root.path(), &config).is_ok());
    }

    #[test]
    fn ensure_file_state_unchanged_accepts_identical_state() {
        let state = FileSnapshotState {
            size_bytes: 12,
            modified_unix_millis: Some(1234),
        };
        assert!(ensure_file_state_unchanged("notes.txt", &state, &state).is_ok());
    }

    #[test]
    fn ensure_file_state_unchanged_rejects_drift() {
        let captured = FileSnapshotState {
            size_bytes: 12,
            modified_unix_millis: Some(1234),
        };
        let verified = FileSnapshotState {
            size_bytes: 18,
            modified_unix_millis: Some(4567),
        };
        let err = ensure_file_state_unchanged("notes.txt", &captured, &verified).unwrap_err();
        let message = format!("{err:#}");
        assert!(message.contains("file changed during ingest: notes.txt"));
        assert!(message.contains("captured: size=12"));
        assert!(message.contains("verified: size=18"));
    }

    #[test]
    fn stat_file_reports_size_and_modified_time() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("sample.txt");
        fs::write(&path, b"hello world").unwrap();
        let state = stat_file(&path).unwrap();
        assert_eq!(state.size_bytes, 11);
        assert!(state.modified_unix_millis.is_some());
    }
}
