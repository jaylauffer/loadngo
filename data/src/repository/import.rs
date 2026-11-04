//! Brings a git history into a repository: every commit reachable from a ref becomes a
//! version, oldest first, with the same parents, files, executable bits and symlinks.
//! The git commit id, committer and commit time are kept as the version's
//! [`Origin`](super::Origin); the author and author time are the version's own.
//! Timezone offsets are not kept.
//!
//! Git is read through one `git cat-file --batch` process. Each git tree and file is
//! converted once (cached by its git id), so unchanged folders cost nothing after the
//! first commit that holds them. An import resumes: commits already imported (found
//! through the branch's trusted head and the origins in its history) are skipped, and
//! the branch moves only forward. Only the new head is signed; it covers every version
//! below it.

use super::{
    now_unix_secs, Origin, Repository, SignedHead, Signer, Tree, TreeCache, TreeEntry, Trust,
    Version, VERSION_FORMAT,
};
use crate::archive_cas::ArchiveObject;
use crate::cas::CasHash;
use anyhow::{anyhow, bail, Context, Result};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

/// What [`import_git`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportReport {
    /// Commits made into versions by this run.
    pub imported: usize,
    pub head: CasHash,
    pub head_commit: String,
    /// Whether the branch moved (false when it was already at the ref).
    pub moved: bool,
}

/// Imports the history of `git_ref` in the git checkout or repository `git_dir` as
/// `branch` of `repository`, signing the new head with `signer`.
///
/// # Errors
/// An existing branch that is not trusted, holds versions made outside git, or is not
/// an ancestor of the ref; a submodule, a name a tree cannot hold, a commit that is not
/// UTF-8; a failed git or store operation.
pub fn import_git(
    repository: &Repository,
    branch: &str,
    git_dir: &Path,
    git_ref: &str,
    signer: &Signer,
    trust: &Trust,
) -> Result<ImportReport> {
    super::validate_id(branch, "branch")?;
    let target = git_text(
        git_dir,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{git_ref}^{{commit}}"),
        ],
    )?;
    let mut known: HashMap<String, CasHash> = HashMap::new();
    let mut current = None;
    let mut exclude = Vec::new();
    if repository.head(branch)?.is_some() {
        let (head, _) = repository.trusted_head(branch, trust)?;
        current = Some(head);
        let version = repository.version(head)?;
        let Some(origin) = version.origin.filter(|origin| origin.system == "git") else {
            bail!(
                "branch {branch} is at {head}, which was not imported from git; an import would not move it forward"
            );
        };
        let ancestor = Command::new("git")
            .arg("-C")
            .arg(git_dir)
            .args(["merge-base", "--is-ancestor", &origin.commit, &target])
            .status()
            .context("failed to run git merge-base")?;
        if !ancestor.success() {
            bail!(
                "branch {branch} is at git commit {}, which is not an ancestor of {git_ref} ({target})",
                origin.commit
            );
        }
        for (hash, version) in repository.ancestry(head)? {
            if let Some(origin) = version.origin.filter(|origin| origin.system == "git") {
                known.insert(origin.commit, hash);
            }
        }
        exclude.push(format!("^{}", origin.commit));
    }
    let mut args = vec!["rev-list", "--reverse", "--topo-order", target.as_str()];
    args.extend(exclude.iter().map(String::as_str));
    let commits: Vec<String> = git_text(git_dir, &args)?
        .lines()
        .map(str::to_string)
        .filter(|commit| !known.contains_key(commit))
        .collect();
    let mut importer = Importer {
        repository,
        git: CatFile::start(git_dir)?,
        blobs: HashMap::new(),
        trees: HashMap::new(),
        cache: TreeCache::default(),
        buffer: Vec::new(),
    };
    let mut head = current;
    for commit in &commits {
        let version = importer.commit(commit, &known)?;
        known.insert(commit.clone(), version);
        head = Some(version);
    }
    importer.git.finish()?;
    let head = *known
        .get(&target)
        .or(head.as_ref())
        .ok_or_else(|| anyhow!("{git_ref} has no commits"))?;
    let moved = current != Some(head);
    if moved {
        let signed = SignedHead::sign(repository.id(), branch, head, signer, now_unix_secs())?;
        repository.set_head(&signed, current)?;
    }
    Ok(ImportReport {
        imported: commits.len(),
        head,
        head_commit: target,
        moved,
    })
}

fn git_text(git_dir: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(git_dir)
        .args(args)
        .output()
        .context("failed to run git")?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    String::from_utf8(output.stdout)
        .map(|text| text.trim().to_string())
        .context("git printed something that is not UTF-8")
}

struct Importer<'a> {
    repository: &'a Repository,
    git: CatFile,
    /// Git blob id to its stored object.
    blobs: HashMap<[u8; 20], ArchiveObject>,
    /// Git tree id to the stored tree and its totals.
    trees: HashMap<[u8; 20], (CasHash, u64, u64)>,
    cache: TreeCache,
    buffer: Vec<u8>,
}

impl Importer<'_> {
    fn commit(&mut self, commit: &str, known: &HashMap<String, CasHash>) -> Result<CasHash> {
        self.git.read(commit, "commit", &mut self.buffer)?;
        let text = std::str::from_utf8(&self.buffer)
            .with_context(|| format!("git commit {commit} is not UTF-8"))?;
        let parsed = parse_commit(text).with_context(|| format!("git commit {commit}"))?;
        let mut parents = Vec::new();
        for parent in &parsed.parents {
            parents.push(*known.get(parent).ok_or_else(|| {
                anyhow!("git commit {commit} has parent {parent}, which is not imported (a shallow history?)")
            })?);
        }
        let (tree, _, _) = self.tree(&parse_id(&parsed.tree)?)?;
        let parent = parents.first().copied();
        let parent_tree = parent
            .map(|parent| self.repository.version(parent).map(|version| version.tree))
            .transpose()?;
        let changes =
            self.repository
                .changes_between(parent, parent_tree, tree, &mut self.cache)?;
        self.repository.put_version(&Version {
            format: VERSION_FORMAT.to_string(),
            repository_id: self.repository.id().to_string(),
            parents,
            tree,
            author: parsed.author.0,
            authored_at_unix_secs: parsed.author.1,
            message: parsed.message,
            changes,
            origin: Some(Origin {
                system: "git".to_string(),
                commit: commit.to_string(),
                committer: parsed.committer.0,
                committed_at_unix_secs: parsed.committer.1,
            }),
        })
    }

    fn tree(&mut self, id: &[u8; 20]) -> Result<(CasHash, u64, u64)> {
        if let Some(done) = self.trees.get(id) {
            return Ok(*done);
        }
        let hex = hex::encode(id);
        self.git.read(&hex, "tree", &mut self.buffer)?;
        let listed = parse_tree(&self.buffer).with_context(|| format!("git tree {hex}"))?;
        let mut entries = Vec::with_capacity(listed.len());
        for (mode, name, child) in listed {
            let name = String::from_utf8(name)
                .map_err(|_| anyhow!("git tree {hex} has a name that is not UTF-8"))?;
            entries.push(match mode.as_str() {
                "100644" | "100664" => TreeEntry::File {
                    name,
                    object: self.blob(&child)?,
                },
                "100755" => TreeEntry::Executable {
                    name,
                    object: self.blob(&child)?,
                },
                "120000" => {
                    self.git
                        .read(&hex::encode(child), "blob", &mut self.buffer)?;
                    let target = String::from_utf8(self.buffer.clone()).map_err(|_| {
                        anyhow!("symlink {name} in git tree {hex} has a target that is not UTF-8")
                    })?;
                    TreeEntry::Symlink { name, target }
                }
                "40000" => {
                    let (tree, files, bytes) = self.tree(&child)?;
                    TreeEntry::Directory {
                        name,
                        tree,
                        files,
                        bytes,
                    }
                }
                "160000" => bail!(
                    "{name} in git tree {hex} is a submodule; submodules are not supported yet"
                ),
                other => bail!("{name} in git tree {hex} has unknown mode {other}"),
            });
        }
        let tree = Tree::new(entries).with_context(|| format!("git tree {hex}"))?;
        let (files, bytes) = tree.totals();
        let hash = self.repository.put_tree(&tree)?;
        self.cache.insert(hash, tree);
        self.trees.insert(*id, (hash, files, bytes));
        Ok((hash, files, bytes))
    }

    fn blob(&mut self, id: &[u8; 20]) -> Result<ArchiveObject> {
        if let Some(object) = self.blobs.get(id) {
            return Ok(*object);
        }
        self.git.read(&hex::encode(id), "blob", &mut self.buffer)?;
        let object = self.repository.store().add_content(&self.buffer)?.object;
        self.blobs.insert(*id, object);
        Ok(object)
    }
}

struct ParsedCommit {
    tree: String,
    parents: Vec<String>,
    author: (String, u64),
    committer: (String, u64),
    message: String,
}

fn parse_commit(text: &str) -> Result<ParsedCommit> {
    let (headers, message) = text.split_once("\n\n").unwrap_or((text, ""));
    let mut tree = None;
    let mut parents = Vec::new();
    let mut author = None;
    let mut committer = None;
    for line in headers.lines() {
        if let Some(rest) = line.strip_prefix("tree ") {
            tree = Some(rest.to_string());
        } else if let Some(rest) = line.strip_prefix("parent ") {
            parents.push(rest.to_string());
        } else if let Some(rest) = line.strip_prefix("author ") {
            author = Some(parse_ident(rest)?);
        } else if let Some(rest) = line.strip_prefix("committer ") {
            committer = Some(parse_ident(rest)?);
        }
        // Other headers (gpgsig and its continuation lines, encoding, mergetag) are not kept.
    }
    Ok(ParsedCommit {
        tree: tree.ok_or_else(|| anyhow!("no tree"))?,
        parents,
        author: author.ok_or_else(|| anyhow!("no author"))?,
        committer: committer.ok_or_else(|| anyhow!("no committer"))?,
        message: message.to_string(),
    })
}

/// `Name <email> 1700000000 +0700` to (`Name <email>`, 1700000000).
fn parse_ident(text: &str) -> Result<(String, u64)> {
    let (rest, _zone) = text
        .rsplit_once(' ')
        .ok_or_else(|| anyhow!("ident {text:?} has no time"))?;
    let (who, time) = rest
        .rsplit_once(' ')
        .ok_or_else(|| anyhow!("ident {text:?} has no time"))?;
    let time = time
        .parse()
        .with_context(|| format!("ident {text:?} has a time before 1970 or not a number"))?;
    Ok((who.to_string(), time))
}

fn parse_id(hex_id: &str) -> Result<[u8; 20]> {
    let bytes = hex::decode(hex_id).with_context(|| format!("{hex_id:?} is not a git id"))?;
    bytes
        .try_into()
        .map_err(|_| anyhow!("{hex_id:?} is not a 20-byte git id"))
}

/// One entry of a git tree: mode, name and id.
type GitTreeEntry = (String, Vec<u8>, [u8; 20]);

/// Git's binary tree format: `<mode> <name>\0<20-byte id>` repeated.
fn parse_tree(bytes: &[u8]) -> Result<Vec<GitTreeEntry>> {
    let mut entries = Vec::new();
    let mut rest = bytes;
    while !rest.is_empty() {
        let space = rest
            .iter()
            .position(|&b| b == b' ')
            .ok_or_else(|| anyhow!("tree entry without a mode"))?;
        let mode = std::str::from_utf8(&rest[..space])?.to_string();
        rest = &rest[space + 1..];
        let nul = rest
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| anyhow!("tree entry without a name"))?;
        let name = rest[..nul].to_vec();
        rest = &rest[nul + 1..];
        if rest.len() < 20 {
            bail!("tree entry cut short");
        }
        let mut id = [0_u8; 20];
        id.copy_from_slice(&rest[..20]);
        rest = &rest[20..];
        entries.push((mode, name, id));
    }
    Ok(entries)
}

/// One `git cat-file --batch` process.
struct CatFile {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}

impl CatFile {
    fn start(git_dir: &Path) -> Result<Self> {
        let mut child = Command::new("git")
            .arg("-C")
            .arg(git_dir)
            .args(["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .context("failed to start git cat-file")?;
        let input = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("no stdin for git"))?;
        let output = BufReader::new(
            child
                .stdout
                .take()
                .ok_or_else(|| anyhow!("no stdout from git"))?,
        );
        Ok(Self {
            child,
            input,
            output,
        })
    }

    /// Reads object `id`, which must be of `kind`, into `buffer`.
    fn read(&mut self, id: &str, kind: &str, buffer: &mut Vec<u8>) -> Result<()> {
        writeln!(self.input, "{id}")?;
        self.input.flush()?;
        let mut header = String::new();
        self.output.read_line(&mut header)?;
        let mut fields = header.split_whitespace();
        let (Some(_), Some(found), Some(size)) = (fields.next(), fields.next(), fields.next())
        else {
            bail!("git has no object {id}: {}", header.trim());
        };
        if found != kind {
            bail!("git object {id} is a {found}, not a {kind}");
        }
        let size: usize = size
            .parse()
            .with_context(|| format!("git object {id} size"))?;
        buffer.clear();
        buffer.resize(size, 0);
        self.output.read_exact(buffer)?;
        let mut newline = [0_u8; 1];
        self.output.read_exact(&mut newline)?;
        Ok(())
    }

    fn finish(mut self) -> Result<()> {
        drop(self.input);
        let status = self.child.wait()?;
        if !status.success() {
            bail!("git cat-file exited with {status}");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_headers_and_idents_parse() {
        let text = "tree 1111111111111111111111111111111111111111\n\
                    parent 2222222222222222222222222222222222222222\n\
                    author Jay Lauffer <jay@example.com> 1793770000 +0700\n\
                    committer Claude <noreply@anthropic.com> 1793770100 -0100\n\
                    gpgsig -----BEGIN-----\n \n -----END-----\n\
                    \nSubject line\n\nBody\n";
        let parsed = parse_commit(text).unwrap();
        assert_eq!(parsed.tree, "1111111111111111111111111111111111111111");
        assert_eq!(parsed.parents.len(), 1);
        assert_eq!(
            parsed.author,
            ("Jay Lauffer <jay@example.com>".into(), 1_793_770_000)
        );
        assert_eq!(parsed.committer.1, 1_793_770_100);
        assert_eq!(parsed.message, "Subject line\n\nBody\n");
    }

    #[test]
    fn binary_trees_parse() {
        let mut bytes = b"100644 a.txt\0".to_vec();
        bytes.extend([7_u8; 20]);
        bytes.extend(b"40000 dir\0");
        bytes.extend([9_u8; 20]);
        let entries = parse_tree(&bytes).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].0, "40000");
        assert_eq!(entries[1].1, b"dir");
        assert_eq!(entries[1].2, [9_u8; 20]);
        assert!(parse_tree(b"100644 cut\0abc").is_err());
    }
}
