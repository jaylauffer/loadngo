use anyhow::{Context, Result, anyhow};
use data::cas::{CasHash, CasStorage};
use serde::Serialize;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const FORMAT: &str = "pudding-workspace-cas-v1";
const DEFAULT_CAS_DIR: &str = ".loadngo-cas";
const DEFAULT_MANIFEST_DIR: &str = "build_tmp/pudding-cas-manifests";
const ROOT_TEXT_EXTENSIONS: &[&str] = &["md", "sh", "txt", "ron", "json", "toml"];
const REPOS: &[&str] = &[
    "entitlement-achievement-blockchain",
    "loadngo",
    "loadngo-cpp",
    "qcoin",
    "sng-rusty",
];

#[derive(Debug, Serialize)]
struct WorkspaceCasManifest {
    format: String,
    workspace_root: String,
    cas_root: String,
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
}

#[derive(Debug, Serialize)]
struct WorkspaceRepoState {
    name: String,
    branch: String,
    head: String,
    status_short: Vec<String>,
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
    let files = collect_workspace_files(&args.workspace_root)?;

    let mut manifest_files = Vec::with_capacity(files.len());
    for path in files {
        let relative = path
            .strip_prefix(&args.workspace_root)
            .with_context(|| format!("path {} not under workspace root", path.display()))?
            .to_string_lossy()
            .to_string();
        let (hash, size, inserted) = store
            .add_file(&path)
            .with_context(|| format!("failed to ingest {}", path.display()))?;
        manifest_files.push(WorkspaceCasFile {
            path: relative,
            hash: hash.to_hex(),
            size,
            inserted,
        });
    }

    let manifest = WorkspaceCasManifest {
        format: FORMAT.to_string(),
        workspace_root: args.workspace_root.display().to_string(),
        cas_root: args.cas_root.display().to_string(),
        created_at_unix_secs: unix_now()?,
        files: manifest_files,
        repos: collect_repo_state(&args.workspace_root)?,
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
}

impl Args {
    fn parse() -> Result<Self> {
        let mut args = std::env::args().skip(1);
        let mut workspace_root = None;
        let mut cas_root = None;
        let mut manifest_dir = None;

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--workspace-root" => workspace_root = args.next().map(PathBuf::from),
                "--cas-root" => cas_root = args.next().map(PathBuf::from),
                "--manifest-dir" => manifest_dir = args.next().map(PathBuf::from),
                "--help" | "-h" => {
                    print_usage();
                    std::process::exit(0);
                }
                other => return Err(anyhow!("unknown argument: {other}")),
            }
        }

        let workspace_root = workspace_root.unwrap_or_else(|| PathBuf::from("/Users/jay/pudding"));
        let cas_root = cas_root.unwrap_or_else(|| workspace_root.join(DEFAULT_CAS_DIR));
        let manifest_dir = manifest_dir.unwrap_or_else(|| workspace_root.join(DEFAULT_MANIFEST_DIR));

        Ok(Self {
            workspace_root,
            cas_root,
            manifest_dir,
        })
    }
}

fn print_usage() {
    eprintln!(
        "Usage: cargo run -p data --bin pudding_cas_ingest -- [--workspace-root <path>] [--cas-root <path>] [--manifest-dir <path>]"
    );
}

fn collect_workspace_files(workspace_root: &Path) -> Result<Vec<PathBuf>> {
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
        let ext = path.extension().and_then(|ext| ext.to_str()).unwrap_or_default();
        if ROOT_TEXT_EXTENSIONS.contains(&ext) {
            files.insert(path);
        }
    }

    for repo in REPOS {
        for relative in git_visible_files(&workspace_root.join(repo))? {
            files.insert(workspace_root.join(repo).join(relative));
        }
    }

    Ok(files.into_iter().collect())
}

fn git_visible_files(repo_root: &Path) -> Result<Vec<PathBuf>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["ls-files", "-c", "-m", "-o", "--exclude-standard"])
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
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if line.is_empty() {
            continue;
        }
        let path = repo_root.join(line);
        if path.is_file() {
            files.push(PathBuf::from(line));
        }
    }
    Ok(files)
}

fn collect_repo_state(workspace_root: &Path) -> Result<Vec<WorkspaceRepoState>> {
    let mut states = Vec::new();
    for repo in REPOS {
        let repo_root = workspace_root.join(repo);
        let branch = git_stdout(&repo_root, &["branch", "--show-current"])?;
        let head = git_stdout(&repo_root, &["rev-parse", "HEAD"])?;
        let status = git_stdout(&repo_root, &["status", "--short"])?;
        states.push(WorkspaceRepoState {
            name: (*repo).to_string(),
            branch: branch.trim().to_string(),
            head: head.trim().to_string(),
            status_short: status.lines().map(|line| line.to_string()).collect(),
        });
    }
    Ok(states)
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
