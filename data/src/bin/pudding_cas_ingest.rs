use anyhow::{anyhow, bail, Context, Result};
use data::cas::CasStorage;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const FORMAT: &str = "pudding-workspace-cas-v1";
const DEFAULT_CAS_DIR: &str = ".loadngo-cas";
const DEFAULT_MANIFEST_DIR: &str = "build_tmp/pudding-cas-manifests";
const DEFAULT_WORKSPACE_CONFIG: &str = "pudding.workspace.ron";
const ROOT_TEXT_EXTENSIONS: &[&str] = &["md", "sh", "txt", "ron", "json", "toml"];

#[derive(Debug, Serialize)]
struct WorkspaceCasManifest {
    format: String,
    workspace_root: String,
    cas_root: String,
    workspace_config: String,
    created_at_unix_secs: u64,
    files: Vec<WorkspaceCasFile>,
    repos: Vec<WorkspaceRepoState>,
}

#[derive(Debug, Serialize)]
struct WorkspaceCasFile {
    path: String,
    hash: String,
    size: u32,
    inserted: bool,
    captured: FileSnapshotState,
    verified: FileSnapshotState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct WorkspaceRepoState {
    name: String,
    path: String,
    branch: String,
    head: String,
    status_short: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct WorkspaceConfig {
    format: String,
    children: Vec<WorkspaceChildConfig>,
}

#[derive(Debug, Clone, Deserialize)]
struct WorkspaceChildConfig {
    name: String,
    path: PathBuf,
    #[serde(default = "default_true")]
    required: bool,
    #[serde(default)]
    include: WorkspaceChildInclude,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
enum WorkspaceChildInclude {
    #[default]
    GitVisible,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct FileSnapshotState {
    size_bytes: u64,
    modified_unix_millis: Option<u64>,
}

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
        manifest_files.push(WorkspaceCasFile {
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

    let manifest = WorkspaceCasManifest {
        format: FORMAT.to_string(),
        workspace_root: args.workspace_root.display().to_string(),
        cas_root: args.cas_root.display().to_string(),
        workspace_config: args.workspace_config.display().to_string(),
        created_at_unix_secs: unix_now()?,
        files: manifest_files,
        repos: initial_repo_state,
    };

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

    println!("CAS root: {}", args.cas_root.display());
    println!("Manifest: {}", manifest_path.display());
    println!("Manifest hash: {}", manifest_hash);
    println!("Manifest inserted: {}", manifest_inserted);
    println!("Files ingested: {}", manifest.files.len());
    Ok(())
}

struct Args {
    workspace_root: PathBuf,
    cas_root: PathBuf,
    manifest_dir: PathBuf,
    workspace_config: PathBuf,
}

impl Args {
    fn parse() -> Result<Self> {
        let mut args = std::env::args().skip(1);
        let mut workspace_root = None;
        let mut cas_root = None;
        let mut manifest_dir = None;
        let mut workspace_config = None;

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--workspace-root" => workspace_root = args.next().map(PathBuf::from),
                "--cas-root" => cas_root = args.next().map(PathBuf::from),
                "--manifest-dir" => manifest_dir = args.next().map(PathBuf::from),
                "--workspace-config" => workspace_config = args.next().map(PathBuf::from),
                "--help" | "-h" => {
                    print_usage();
                    std::process::exit(0);
                }
                other => return Err(anyhow!("unknown argument: {other}")),
            }
        }

        let workspace_root = workspace_root.unwrap_or_else(|| PathBuf::from("/Users/jay/pudding"));
        let cas_root = cas_root.unwrap_or_else(|| workspace_root.join(DEFAULT_CAS_DIR));
        let manifest_dir =
            manifest_dir.unwrap_or_else(|| workspace_root.join(DEFAULT_MANIFEST_DIR));
        let workspace_config =
            workspace_config.unwrap_or_else(|| workspace_root.join(DEFAULT_WORKSPACE_CONFIG));

        Ok(Self {
            workspace_root,
            cas_root,
            manifest_dir,
            workspace_config,
        })
    }
}

fn print_usage() {
    eprintln!(
        "Usage: cargo run -p data --bin pudding_cas_ingest -- [--workspace-root <path>] [--cas-root <path>] [--manifest-dir <path>] [--workspace-config <path>]"
    );
}

fn load_workspace_config(path: &Path) -> Result<WorkspaceConfig> {
    let text =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let config: WorkspaceConfig =
        ron::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(config)
}

fn validate_workspace_config(workspace_root: &Path, config: &WorkspaceConfig) -> Result<()> {
    if config.format != "pudding-workspace-v1" {
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

fn default_true() -> bool {
    true
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

fn unix_now() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock before unix epoch")?
        .as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

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
            format: "pudding-workspace-v1".to_string(),
            children: Vec::new(),
        };
        let err = validate_workspace_config(Path::new("/tmp"), &config).unwrap_err();
        assert!(format!("{err:#}").contains("must declare at least one child repo"));
    }

    #[test]
    fn validate_workspace_config_rejects_missing_required_child() {
        let root = tempfile::tempdir().unwrap();
        let config = WorkspaceConfig {
            format: "pudding-workspace-v1".to_string(),
            children: vec![WorkspaceChildConfig {
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
            format: "pudding-workspace-v1".to_string(),
            children: vec![WorkspaceChildConfig {
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
