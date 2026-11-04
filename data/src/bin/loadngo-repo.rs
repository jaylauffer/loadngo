//! `loadngo-repo`: our post-quantum version control on the Archive CAS. Thin CLI over
//! `data::repository`; the design is in `docs/REPOSITORY.md`.

use anyhow::{anyhow, bail, Context, Result};
use data::cli::{ArgDoc, Usage};
use data::repository::transfer::{self, Remote};
use data::repository::work_tree::{self, Target};
use data::repository::{self, import, Repository, Signer, Trust};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

fn main() {
    if let Err(error) = run() {
        // `log | head` closing the pipe early is not a failure.
        if error.chain().any(|cause| {
            cause
                .downcast_ref::<std::io::Error>()
                .is_some_and(|io| io.kind() == std::io::ErrorKind::BrokenPipe)
        }) {
            return;
        }
        eprintln!("loadngo-repo: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args.iter().any(|arg| arg == "--help" || arg == "-h") {
        usage().print();
        return Ok(());
    }
    let command = args[0].as_str();
    let flags = Flags::parse(command, &args[1..])?;
    match command {
        "init" => init(&flags),
        "import" => import_command(&flags),
        "log" => log(&flags),
        "checkout" => checkout(&flags),
        "status" => status(&flags),
        "commit" => commit(&flags),
        "verify" => verify(&flags),
        "send" => send(&flags),
        "receive" => receive(&flags),
        "heads" => heads(&flags),
        "help" => {
            usage().print();
            Ok(())
        }
        other => bail!("unknown command {other:?}\n{}", usage().hint()),
    }
}

/// The flags each command takes; `true` marks one that may be repeated.
fn allowed(command: &str) -> &'static [(&'static str, bool)] {
    const SIGNING: [(&str, bool); 3] = [
        ("--signer-identity", false),
        ("--public-key", false),
        ("--private-key", false),
    ];
    match command {
        "init" => &[
            ("--store", false),
            ("--repository", false),
            ("--branch", false),
            ("--tree", false),
            ("--message", false),
            ("--author", false),
            SIGNING[0],
            SIGNING[1],
            SIGNING[2],
        ],
        "import" => &[
            ("--store", false),
            ("--repository", false),
            ("--branch", false),
            ("--from-git", false),
            ("--git-ref", false),
            ("--trusted-public-key", false),
            SIGNING[0],
            SIGNING[1],
            SIGNING[2],
        ],
        "log" => &[
            ("--store", false),
            ("--repository", false),
            ("--branch", false),
            ("--version", false),
            ("--trusted-public-key", false),
        ],
        "checkout" => &[
            ("--store", false),
            ("--repository", false),
            ("--branch", false),
            ("--version", false),
            ("--destination", false),
            ("--trusted-public-key", false),
        ],
        "status" => &[("--tree", false)],
        "commit" => &[
            ("--tree", false),
            ("--path", true),
            ("--message", false),
            ("--author", false),
            ("--trusted-public-key", false),
            SIGNING[0],
            SIGNING[1],
            SIGNING[2],
        ],
        "verify" => &[
            ("--store", false),
            ("--repository", false),
            ("--branch", false),
            ("--trusted-public-key", false),
        ],
        "send" => &[
            ("--store", false),
            ("--repository", false),
            ("--branch", false),
            ("--to", false),
            ("--to-ssh", false),
            ("--remote-store", false),
            ("--remote-program", false),
            ("--remote-trusted-public-key", false),
            ("--trusted-public-key", false),
        ],
        "receive" => &[("--store", false), ("--trusted-public-key", false)],
        "heads" => &[("--store", false), ("--repository", false)],
        _ => &[],
    }
}

struct Flags {
    values: BTreeMap<&'static str, Vec<String>>,
}

impl Flags {
    fn parse(command: &str, args: &[String]) -> Result<Self> {
        let allowed = allowed(command);
        let mut values: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            let Some(&(flag, repeatable)) = allowed.iter().find(|(flag, _)| flag == arg) else {
                bail!("{command} does not take {arg:?}\n{}", usage().hint());
            };
            let value = args
                .next()
                .ok_or_else(|| anyhow!("{flag} needs a value\n{}", usage().hint()))?;
            let list = values.entry(flag).or_default();
            if !list.is_empty() && !repeatable {
                bail!("{flag} is given twice");
            }
            list.push(value.clone());
        }
        Ok(Self { values })
    }

    fn get(&self, flag: &str) -> Option<&str> {
        self.values
            .get(flag)
            .and_then(|list| list.first())
            .map(String::as_str)
    }

    fn required(&self, flag: &str) -> Result<&str> {
        self.get(flag)
            .ok_or_else(|| anyhow!("missing {flag}\n{}", usage().hint()))
    }

    fn all(&self, flag: &str) -> Vec<String> {
        self.values.get(flag).cloned().unwrap_or_default()
    }

    fn store(&self) -> Result<PathBuf> {
        self.get("--store")
            .map_or_else(repository::default_store, |store| Ok(PathBuf::from(store)))
    }

    fn repository(&self, create: bool) -> Result<Repository> {
        Repository::open(&self.store()?, self.required("--repository")?, create)
    }

    fn trust(&self) -> Result<Trust> {
        match self.get("--trusted-public-key") {
            Some(path) => Trust::from_file(Path::new(path)),
            None => Trust::from_default_keys(),
        }
    }

    fn signer(&self) -> Result<Signer> {
        match (
            self.get("--signer-identity"),
            self.get("--public-key"),
            self.get("--private-key"),
        ) {
            (Some(identity), Some(public), Some(private)) => {
                Signer::from_files(identity, Path::new(public), Path::new(private))
            }
            (None, None, None) => Signer::from_default_keys()?.ok_or_else(|| {
                anyhow!(
                    "no signing key: pass --signer-identity, --public-key and --private-key, or \
                     keep exactly one <identity>.dilithium2.key with its .pub in ~/.loadngo/keys"
                )
            }),
            _ => bail!("--signer-identity, --public-key and --private-key go together"),
        }
    }

    fn tree(&self) -> Result<PathBuf> {
        match self.get("--tree") {
            Some(tree) => Ok(PathBuf::from(tree)),
            None => work_tree::find_tree(Path::new(".")),
        }
    }
}

fn init(flags: &Flags) -> Result<()> {
    let repository = flags.repository(true)?;
    let tree = PathBuf::from(flags.required("--tree")?);
    let report = work_tree::init(
        &repository,
        flags.required("--branch")?,
        &tree,
        flags.required("--message")?,
        flags.required("--author")?,
        &flags.signer()?,
    )?;
    println!(
        "Recorded {} paths of {} as version {} on {} of {}; the folder is now its checkout.",
        report.changes.len(),
        tree.display(),
        report.version,
        report.branch,
        repository.id()
    );
    Ok(())
}

fn import_command(flags: &Flags) -> Result<()> {
    let repository = flags.repository(true)?;
    let branch = flags.required("--branch")?;
    let git_ref = flags.get("--git-ref").unwrap_or(branch);
    let trust = flags.trust()?;
    let report = import::import_git(
        &repository,
        branch,
        Path::new(flags.required("--from-git")?),
        git_ref,
        &flags.signer()?,
        &trust,
    )?;
    if report.moved {
        println!(
            "Imported {} commits of {git_ref}; {} of {} is at {} (git {}), signed.",
            report.imported,
            branch,
            repository.id(),
            report.head,
            report.head_commit
        );
    } else {
        println!(
            "{branch} of {} is already at {} (git {}).",
            repository.id(),
            report.head,
            report.head_commit
        );
    }
    Ok(())
}

fn log(flags: &Flags) -> Result<()> {
    let repository = flags.repository(false)?;
    let trust = flags.trust()?;
    let start = match (flags.get("--branch"), flags.get("--version")) {
        (Some(branch), None) => repository.trusted_head(branch, &trust)?.0,
        (None, Some(version)) => {
            let version = version.parse().context("--version is not a version hash")?;
            if repository
                .trusted_branch_holding(version, &trust)?
                .is_none()
            {
                bail!("version {version} is not in the history of any trusted branch");
            }
            version
        }
        _ => bail!("name --branch or --version\n{}", usage().hint()),
    };
    let mut out = std::io::stdout().lock();
    for (hash, version) in repository.first_parent_history(start)? {
        let parents = if version.parents.is_empty() {
            "-".to_string()
        } else {
            version
                .parents
                .iter()
                .map(|parent| parent.to_hex())
                .collect::<Vec<_>>()
                .join(",")
        };
        writeln!(
            out,
            "{hash}\t{}\t{parents}\t{}\t{}\t{}",
            version
                .origin
                .as_ref()
                .map_or("-", |origin| origin.commit.as_str()),
            repository::format_utc(version.authored_at_unix_secs),
            version.author,
            version.subject()
        )?;
    }
    Ok(())
}

fn checkout(flags: &Flags) -> Result<()> {
    let repository = flags.repository(false)?;
    let target = match (flags.get("--branch"), flags.get("--version")) {
        (Some(branch), None) => Target::Branch(branch.to_string()),
        (None, Some(version)) => {
            Target::Version(version.parse().context("--version is not a version hash")?)
        }
        _ => bail!("name --branch or --version\n{}", usage().hint()),
    };
    let destination = PathBuf::from(flags.required("--destination")?);
    let report = work_tree::checkout(&repository, &target, &flags.trust()?, &destination)?;
    println!(
        "Checked out {} into {}: {} files, {} bytes.",
        report.version,
        destination.display(),
        report.files,
        report.bytes
    );
    Ok(())
}

fn status(flags: &Flags) -> Result<()> {
    let tree = flags.tree()?;
    let mut out = std::io::stdout().lock();
    for change in work_tree::status(&tree)?.changes {
        writeln!(out, "{}\t{}", change.kind(), change.path())?;
    }
    Ok(())
}

fn commit(flags: &Flags) -> Result<()> {
    let tree = flags.tree()?;
    let marker = work_tree::CheckoutMarker::read(&tree)?;
    let report = work_tree::commit(
        &tree,
        &flags.all("--path"),
        flags.required("--message")?,
        flags.required("--author")?,
        &flags.signer()?,
        &flags.trust()?,
    )?;
    println!(
        "Version {} on {} of {} (parent {}), signed: {} changes.",
        report.version,
        report.branch,
        marker.repository_id,
        report
            .parent
            .map_or_else(|| "none".into(), |parent| parent.to_hex()),
        report.changes.len()
    );
    for change in &report.changes {
        println!("  {}\t{}", change.kind(), change.path());
    }
    Ok(())
}

fn verify(flags: &Flags) -> Result<()> {
    let repository = flags.repository(false)?;
    let branch = flags.required("--branch")?;
    let report = repository.verify(branch, &flags.trust()?)?;
    println!(
        "Verified {branch} of {} at {}, signed by {}: {} versions, {} trees, {} files ({} bytes), every hash checked.",
        repository.id(),
        report.head,
        report.signer_identity,
        report.versions,
        report.trees,
        report.files,
        report.bytes
    );
    Ok(())
}

fn send(flags: &Flags) -> Result<()> {
    let repository = flags.repository(false)?;
    let branch = flags.required("--branch")?;
    let (remote, remote_store) = match (flags.get("--to"), flags.get("--to-ssh")) {
        (Some(to), None) => (
            Remote::Local(std::env::current_exe()?.into_os_string()),
            to.to_string(),
        ),
        (None, Some(host)) => (
            Remote::Ssh {
                host: host.to_string(),
                program: flags
                    .get("--remote-program")
                    .unwrap_or("loadngo-repo")
                    .to_string(),
            },
            flags.required("--remote-store")?.to_string(),
        ),
        _ => bail!("name --to <store> or --to-ssh <host>\n{}", usage().hint()),
    };
    let report = transfer::send(
        &repository,
        branch,
        &flags.trust()?,
        &remote,
        &remote_store,
        flags.get("--remote-trusted-public-key"),
    )?;
    println!(
        "Sent {branch} of {} at {}: {} versions, {} objects, {} bytes.",
        repository.id(),
        report.head,
        report.versions,
        report.objects,
        report.bytes
    );
    print!("{}", report.receiver_said);
    Ok(())
}

fn receive(flags: &Flags) -> Result<()> {
    let store = flags.store()?;
    let report = transfer::receive(&store, &mut std::io::stdin().lock(), &flags.trust()?)?;
    if report.moved {
        println!(
            "Received {} objects into {}; {} of {} is at {}, its signature verified.",
            report.objects,
            store.display(),
            report.branch,
            report.repository_id,
            report.head
        );
    } else {
        println!(
            "Received {} objects into {}; {} of {} was already at {}.",
            report.objects,
            store.display(),
            report.branch,
            report.repository_id,
            report.head
        );
    }
    Ok(())
}

fn heads(flags: &Flags) -> Result<()> {
    print!(
        "{}",
        transfer::list_heads(&flags.store()?, flags.required("--repository")?)?
    );
    Ok(())
}

fn usage() -> Usage {
    const ARGS: &[ArgDoc] = &[
        ArgDoc::optional(
            "--store",
            "<folder>",
            "the store (an Archive CAS root); default $LOADNGO_REPO_STORE, else ~/.loadngo/store",
        ),
        ArgDoc::optional("--repository", "<id>", "repository id, e.g. loadngo (all but status, commit, receive)"),
        ArgDoc::optional("--branch", "<name>", "branch, e.g. dev"),
        ArgDoc::optional(
            "--trusted-public-key",
            "<file.pub>",
            "key whose head signatures are trusted; default the one *.dilithium2.pub in ~/.loadngo/keys",
        ),
        ArgDoc::optional("init --tree", "<folder>", "record a folder as the first version of a new branch"),
        ArgDoc::optional("init/commit --message", "<text>", "what the version is and why"),
        ArgDoc::optional("init/commit --author", "<name>", "who made it"),
        ArgDoc::optional("import --from-git", "<folder>", "a git checkout or repository to import from"),
        ArgDoc::optional("import --git-ref", "<ref>", "the git ref or commit to import up to; default the branch name"),
        ArgDoc::optional("log/checkout --version", "<hash>", "one version instead of a branch's head"),
        ArgDoc::optional("checkout --destination", "<new-folder>", "where to check out; must not exist"),
        ArgDoc::optional("status/commit --tree", "<folder>", "the checkout; default the one holding the current folder"),
        ArgDoc::repeated("commit --path", "<path>", "a file or folder, relative to the top of the tree, whose changes to record"),
        ArgDoc::optional("--signer-identity", "<name>", "with --public-key and --private-key: who signs; default the one key pair in ~/.loadngo/keys"),
        ArgDoc::optional("--public-key", "<file.pub>", "the signing key's public half"),
        ArgDoc::optional("--private-key", "<file.key>", "the signing key"),
        ArgDoc::optional("send --to", "<store>", "a store on this machine to send to"),
        ArgDoc::optional("send --to-ssh", "<user@host>", "a machine to send to over ssh (post-quantum key exchange only)"),
        ArgDoc::optional("send --remote-store", "<folder>", "with --to-ssh: the store there (absolute, or relative to its home)"),
        ArgDoc::optional("send --remote-program", "<path>", "with --to-ssh: loadngo-repo there; default loadngo-repo on its PATH"),
        ArgDoc::optional("send --remote-trusted-public-key", "<file.pub>", "the key the receiver should trust (a path where it runs)"),
    ];
    const NOTES: &[&str] = &[
        "Commands: init, import, log, checkout, status, commit, verify, send, receive (reads a stream on stdin; send runs it), heads (lists a store's heads; send runs it).",
        "Every version is named by the BLAKE3 hash of its bytes and names its parents and tree by hash; a branch's head is a Dilithium2-signed claim. A version is trusted only in the history of a head signed by a trusted key, and every object is hash-checked as it is read.",
        "log prints one line per version, newest first, tab-separated: version, git commit (or -), parents (or -), time (UTC), author, subject.",
        "status prints created/changed/deleted and a path, tab-separated. It looks at tracked paths always and untracked ones unless .gitignore rules, a CACHEDIR.TAG build cache or another checkout hide them.",
        "commit records only the paths named with --path, never the whole tree unasked; the branch must still be at the version the tree is based on.",
        "checkout writes into a temporary sibling and renames it at the end, so a refused or failed checkout leaves nothing at --destination.",
        "receive moves a branch only forward, after the head's signature verifies against its trusted key and every object below it hash-checks.",
    ];
    const EXAMPLES: &[&str] = &[
        "loadngo-repo import --repository loadngo --branch dev --from-git ~/pudding/loadngo",
        "loadngo-repo log --repository loadngo --branch dev | head",
        "loadngo-repo checkout --repository loadngo --branch dev --destination ~/loadngo-work",
        "loadngo-repo status --tree ~/loadngo-work",
        "loadngo-repo commit --tree ~/loadngo-work --path docs/REPOSITORY.md --message 'Explain trust' --author Claudette",
        "loadngo-repo verify --repository loadngo --branch dev",
        "loadngo-repo send --repository loadngo --branch dev --to-ssh jay@10.10.10.6 --remote-store .loadngo/store --remote-program pudding/loadngo/target/release/loadngo-repo",
    ];
    Usage {
        bin: "loadngo-repo",
        invocation: "cargo run -p data --bin loadngo-repo -- <command>",
        about: "post-quantum signed version control on the Archive CAS: import a git history, check versions out, record changes, verify, send between machines",
        args: ARGS,
        examples: EXAMPLES,
        notes: NOTES,
    }
}
